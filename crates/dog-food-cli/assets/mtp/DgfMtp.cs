// dog-food's test worker for Microsoft.Testing.Platform test applications
// (xUnit v3, the MSTest runner, TUnit): it starts the test application once in
// the platform's server mode and keeps it running, so every later test run
// costs only its tests. The test application has nothing to say from outside
// which test runs, so a planned run asks for one test at a time and tells the
// mutant switches the test and its mutant through a small control file
// (DGF_CONTROL), which they read again when its first four bytes change.
// xUnit v3 says which test runs inside the test process, so there a whole set
// of tests goes in one request, which costs far less than a request per test:
// the control file then only names the plan.
//
// Same requests and replies as dog-food's VSTest worker:
//   plan<TAB>file[<TAB>nocheck]   file of "test<TAB>mutant id" lines: run those tests, each with its mutant;
//                                 a failed test runs again at once with no mutant (the kill check) unless
//                                 "nocheck", and one that fails again marks every failed test polluted
//   kill<TAB>file                 run the tests named in the file in that order and stop at the first
//                                 failure (a test application started with one mutant on)
//   tests<TAB>file[<TAB>folder]   file holding a test filter (empty: every test): run the matching tests;
//                                 with a folder, record which mutants each test reaches there
//   list<TAB>file                 the tests a filter keeps, as results
//   names<TAB>file[<TAB>folder]   run the tests named in the file, one per line; a folder records reach
//   quit
// Replies: "ready<TAB>tests" (or "error<TAB>why"), then per request "result<TAB>test<TAB>passed|failed<TAB>ms<TAB>file"
// lines and "done". While a plan runs, "running<TAB>test" says a test started and a passed test's result
// comes at once, so a run stopped at its time limit keeps what it reported; a failed test's result waits
// for the kill check.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Linq;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using Microsoft.Testing.Platform.ServerMode.Client;

namespace DogFood.Mtp
{
    public static class Program
    {
        public static async Task<int> Main(string[] args)
        {
            var protocol = new StreamWriter(Console.OpenStandardOutput()) { AutoFlush = false };
            Console.SetOut(Console.Error);
            try
            {
                return await Serve(Path.GetFullPath(args[0]), protocol);
            }
            catch (Exception error)
            {
                protocol.WriteLine("error\t" + error.ToString().Replace('\n', ' ').Replace('\r', ' '));
                protocol.Flush();
                return 1;
            }
        }

        private static async Task<int> Serve(string tests, StreamWriter protocol)
        {
            using var control = Control.Create();
            // The test application inherits the environment: the switches find the control file.
            Environment.SetEnvironmentVariable("DGF_CONTROL", control.Path);
            using IMtpServerClient client = await MtpServerClient.LaunchAsync(tests);
            var nodes = new Nodes();
            client.TestNodesUpdated += (_, update) => nodes.Add(update.Changes);
            await client.InitializeAsync();
            await client.DiscoverTestsAsync();
            var batch = File.Exists(Path.Combine(Path.GetDirectoryName(tests) ?? ".", "xunit.v3.core.dll"));
            var server = new Server(client, nodes, control, nodes.Discovered(), protocol, batch);
            protocol.WriteLine("ready\t" + server.Count);
            protocol.Flush();
            string? line;
            while ((line = Console.In.ReadLine()) != null && await server.Handle(line))
            {
            }
            await client.ExitAsync();
            return 0;
        }
    }

    /// Serves the requests of one dgf run.
    internal sealed class Server
    {
        private readonly IMtpServerClient client;
        private readonly Nodes nodes;
        private readonly Control control;
        private readonly Dictionary<string, List<string>> byName;
        private readonly StreamWriter protocol;
        /// The switches know the running test by themselves (xUnit v3).
        private readonly bool batch;

        public Server(IMtpServerClient client, Nodes nodes, Control control, Dictionary<string, List<string>> byName, StreamWriter protocol, bool batch)
        {
            this.batch = batch;
            this.client = client;
            this.nodes = nodes;
            this.control = control;
            this.byName = byName;
            this.protocol = protocol;
        }

        public int Count => byName.Count;

        /// Serves one request; false on "quit".
        public async Task<bool> Handle(string line)
        {
            var parts = line.TrimEnd('\r', '\n').Split('\t');
            if (parts[0] == "quit")
            {
                return false;
            }
            var results = new Results();
            try
            {
                using (parts[0] == "list" ? null : new Heartbeat(protocol, nodes))
                {
                    await Serve(parts, results);
                }
            }
            catch (Exception error)
            {
                // The test application failed the request, as a rule because
                // it died: this worker cannot serve another. It ends with
                // the results it has and no "done", so dgf sees a crash and
                // starts a new one, instead of pooling this one as warm.
                Console.Error.WriteLine("dgf mtp worker: " + error);
                results.Add(nodes.Finished());
                Reply(results, false);
                Environment.Exit(2);
            }
            Reply(results, true);
            return true;
        }

        private async Task Serve(string[] parts, Results results)
        {
            var third = parts.Length > 2 ? parts[2] : null;
            if (parts[0] == "plan")
            {
                await Plan(parts[1], third != "nocheck", results);
            }
            else if (parts[0] == "tests")
            {
                var matches = Filter.Parse(File.ReadAllText(parts[1]).Trim());
                await Tests(byName.Keys.Where(matches).ToList(), third, results);
            }
            else if (parts[0] == "names")
            {
                await Tests(File.ReadAllLines(parts[1]).Where(byName.ContainsKey).ToList(), third, results);
            }
            else if (parts[0] == "list")
            {
                var matches = Filter.Parse(File.ReadAllText(parts[1]).Trim());
                results.Add(byName.Keys.Where(matches).Select(n => (n, true, 0L, "")));
            }
            else if (parts[0] == "kill")
            {
                await Kill(File.ReadAllLines(parts[1]), results);
            }
        }

        /// dgf lists each mutant's tests quickest first; once one fails, the
        /// mutant is killed and its other tests are skipped. The kill check:
        /// a failed test runs again with no mutant, and one that fails again
        /// was failed by state the test application carries, so every failed
        /// test of this request is reported polluted. The switches note each
        /// planned mutant that took effect in a file next to the plan, which
        /// dgf reads itself when it stops a run at its time limit.
        private async Task Plan(string file, bool check, Results results)
        {
            var rows = File.ReadAllLines(file).Select(Row).Where(r => r.Id >= 0 && byName.ContainsKey(r.Name)).ToList();
            var activated = file + ".activated";
            nodes.Started = name => Write("running\t" + name);
            nodes.Passed = (name, ms, source) =>
            {
                results.Streamed.Add(name);
                Write("result\t" + name + "\tpassed\t" + ms + "\t" + source);
            };
            var failed = new List<string>();
            try
            {
                var polluted = batch
                    ? await PlanInSets(file, rows, check, activated, failed, results)
                    : await PlanOneByOne(rows, check, activated, failed, results);
                if (polluted)
                {
                    results.Polluted.AddRange(failed);
                }
            }
            finally
            {
                nodes.Started = null;
                nodes.Passed = null;
            }
            TakeActivated(activated, results);
        }

        /// One request per test: the switches learn the test from the control file.
        private async Task<bool> PlanOneByOne(List<(string Name, int Id)> rows, bool check, string activated, List<string> failed, Results results)
        {
            var killed = new HashSet<int>();
            var polluted = false;
            foreach (var (name, id) in rows)
            {
                if (killed.Contains(id) || await RunOne(name, id, null, activated, results))
                {
                    continue;
                }
                killed.Add(id);
                failed.Add(name);
                polluted |= check && !await Quietly(() => RunOne(name, -1, null, null, new Results()));
            }
            return polluted;
        }

        /// One request per set: each mutant's first test, then the second
        /// test of each mutant still alive, and so on, so a killed mutant's
        /// other tests are still skipped. The kill check runs after each set.
        private async Task<bool> PlanInSets(string file, List<(string Name, int Id)> rows, bool check, string activated, List<string> failed, Results results)
        {
            var killed = new HashSet<int>();
            var mutantOf = new Dictionary<string, int>();
            var sets = new List<List<(string Name, int Id)>>();
            var taken = new Dictionary<int, int>();
            foreach (var row in rows)
            {
                taken.TryGetValue(row.Id, out var turn);
                taken[row.Id] = turn + 1;
                while (sets.Count <= turn)
                {
                    sets.Add(new List<(string Name, int Id)>());
                }
                sets[turn].Add(row);
                mutantOf[row.Name] = row.Id;
            }
            var polluted = false;
            var setFile = file + ".set";
            foreach (var set in sets)
            {
                var alive = set.Where(r => !killed.Contains(r.Id)).ToList();
                if (alive.Count == 0)
                {
                    continue;
                }
                File.WriteAllLines(setFile, alive.Select(r => r.Name + "\t" + r.Id));
                control.Set("", -1, null, activated, setFile);
                var finished = await RunSet(alive.Select(r => r.Name), results);
                var bad = finished.Where(r => !r.Passed).Select(r => r.Name).Distinct().Where(mutantOf.ContainsKey).ToList();
                foreach (var name in bad)
                {
                    killed.Add(mutantOf[name]);
                    failed.Add(name);
                }
                if (check && bad.Count > 0)
                {
                    control.Set("", -1, null, null, Control.NoPlan);
                    polluted |= await Quietly(async () => (await RunSet(bad, new Results())).Any(r => !r.Passed));
                }
            }
            return polluted;
        }

        /// Runs `run` with no "running" lines or streamed results.
        private async Task<bool> Quietly(Func<Task<bool>> run)
        {
            var (started, passed) = (nodes.Started, nodes.Passed);
            nodes.Started = null;
            nodes.Passed = null;
            try
            {
                return await run();
            }
            finally
            {
                nodes.Started = started;
                nodes.Passed = passed;
            }
        }

        /// Runs the named tests in one request; what they reported.
        private async Task<List<(string Name, bool Passed, long Milliseconds, string File)>> RunSet(IEnumerable<string> names, Results results)
        {
            nodes.Start();
            await client.RunTestsAsync(names.SelectMany(n => byName[n]).ToList());
            var finished = nodes.Finished();
            results.Add(finished);
            return finished;
        }

        private void Write(string line)
        {
            lock (protocol)
            {
                protocol.WriteLine(line);
                protocol.Flush();
            }
        }

        /// The pairs the switches noted in `file`, which is then removed.
        private static void TakeActivated(string file, Results results)
        {
            if (!File.Exists(file))
            {
                return;
            }
            results.Activated.AddRange(File.ReadAllLines(file).Where(l => l.Length > 0));
            File.Delete(file);
        }

        /// Runs the named tests in that order and stops at the first failure:
        /// in a test application started with one mutant on, one failing test
        /// decides it. No test needs its own request here, so the requests
        /// grow: the likeliest killers come first, and a survivor's long list
        /// costs few requests.
        private async Task Kill(string[] names, Results results)
        {
            var known = names.Where(byName.ContainsKey).ToList();
            control.Set("", -1, null, null, null);
            var size = 1;
            for (var at = 0; at < known.Count; at += size, size = Math.Min(size * 2, 256))
            {
                var finished = await RunSet(known.Skip(at).Take(size), results);
                if (finished.Any(r => !r.Passed))
                {
                    return;
                }
            }
        }

        private static (string Name, int Id) Row(string row)
        {
            var tab = row.LastIndexOf('\t');
            return tab > 0 && int.TryParse(row.Substring(tab + 1), out var id) ? (row.Substring(0, tab), id) : ("", -1);
        }

        /// Which test reaches what is known one test at a time, unless the
        /// switches know the running test themselves.
        private async Task Tests(List<string> chosen, string? reach, Results results)
        {
            if (reach == null || batch)
            {
                control.Set("", -1, reach, null, reach == null ? null : Control.NoPlan);
                await RunSet(chosen, results);
                return;
            }
            foreach (var name in chosen)
            {
                await RunOne(name, -1, reach, null, results);
            }
        }

        /// Runs one test (every row of it); whether it passed.
        private async Task<bool> RunOne(string name, int mutant, string? reach, string? activated, Results results)
        {
            var clock = Stopwatch.StartNew();
            control.Set(name, mutant, reach, activated, null);
            var finished = await RunSet(new[] { name }, results);
            // A request costs more than its test: a time limit built from this counts it.
            results.AtLeast(name, clock.ElapsedMilliseconds);
            return finished.All(r => r.Passed);
        }

        private void Reply(Results results, bool done)
        {
            foreach (var result in results.All.Where(r => !results.Streamed.Contains(r.Name)))
            {
                protocol.WriteLine("result\t" + result.Name + "\t" + (result.Passed ? "passed" : "failed") + "\t" + result.Milliseconds + "\t" + result.File);
            }
            foreach (var pair in results.Activated)
            {
                protocol.WriteLine("activated\t" + pair);
            }
            foreach (var name in results.Polluted)
            {
                protocol.WriteLine("polluted\t" + name);
            }
            if (done)
            {
                protocol.WriteLine("done");
            }
            protocol.Flush();
        }
    }

    /// The control file the switches read: four bytes that change with each
    /// run, then "test\nmutant id\nreach folder\nactivated file\nplan" in UTF-8.
    /// With a plan the switches ask xUnit which test runs: the plan is a file
    /// of "test<TAB>mutant id" lines, or NoPlan for none.
    internal sealed class Control : IDisposable
    {
        private const int Size = 64 * 1024;
        public const string NoPlan = "*";
        private readonly MemoryMappedFile file;
        private readonly MemoryMappedViewAccessor view;
        private int generation;

        private Control(string path)
        {
            Path = path;
            // Shared, so the test application can map it too.
            var stream = new FileStream(path, FileMode.Create, FileAccess.ReadWrite, FileShare.ReadWrite | FileShare.Delete);
            stream.SetLength(Size);
            file = MemoryMappedFile.CreateFromFile(stream, null, Size, MemoryMappedFileAccess.ReadWrite, HandleInheritability.None, false);
            view = file.CreateViewAccessor(0, Size, MemoryMappedFileAccess.ReadWrite);
        }

        public string Path { get; }

        public static Control Create()
        {
            return new Control(System.IO.Path.Combine(System.IO.Path.GetTempPath(), "dgf-control-" + Guid.NewGuid().ToString("N") + ".bin"));
        }

        public void Set(string test, int mutant, string? reach, string? activated, string? plan)
        {
            var text = Encoding.UTF8.GetBytes(test + "\n" + mutant + "\n" + (reach ?? "") + "\n" + (activated ?? "") + "\n" + (plan ?? ""));
            view.Write(4, text.Length);
            view.WriteArray(8, text, 0, Math.Min(text.Length, Size - 8));
            view.Write(0, ++generation);
            view.Flush();
        }

        public void Dispose()
        {
            view.Dispose();
            file.Dispose();
            try
            {
                File.Delete(Path);
            }
            catch (IOException)
            {
            }
        }
    }

    internal sealed class Result
    {
        public string Name = "";
        public bool Passed = true;
        public long Milliseconds;
        public string File = "";
    }

    /// Each test's result, all rows of a theory together.
    internal sealed class Results
    {
        public readonly List<Result> All = new List<Result>();
        /// "test<TAB>mutant id" for each planned mutant that took effect in its test.
        public readonly List<string> Activated = new List<string>();
        /// Failed tests whose failure the kill check could not pin on their mutant.
        public readonly List<string> Polluted = new List<string>();
        /// Tests whose result already went out while the request ran.
        public readonly HashSet<string> Streamed = new HashSet<string>();
        private readonly Dictionary<string, Result> byName = new Dictionary<string, Result>();

        /// `name` took at least `milliseconds`.
        public void AtLeast(string name, long milliseconds)
        {
            if (byName.TryGetValue(name, out var result))
            {
                result.Milliseconds = Math.Max(result.Milliseconds, milliseconds);
            }
        }

        public void Add(IEnumerable<(string Name, bool Passed, long Milliseconds, string File)> rows)
        {
            foreach (var row in rows)
            {
                if (!byName.TryGetValue(row.Name, out var result))
                {
                    result = new Result { Name = row.Name, File = row.File };
                    byName[row.Name] = result;
                    All.Add(result);
                }
                result.Passed &= row.Passed;
                result.Milliseconds += row.Milliseconds;
            }
        }
    }

    /// What the test application reported about its test nodes.
    /// Says every few seconds what the worker is doing while a request runs,
    /// on a thread of its own: "alive", seconds, tests reported so far, the
    /// running test, threads, queued work items, working set, gen 2 collections.
    internal sealed class Heartbeat : IDisposable
    {
        private readonly ManualResetEventSlim stop = new ManualResetEventSlim();
        private readonly Thread thread;

        public Heartbeat(StreamWriter protocol, Nodes nodes)
        {
            var clock = Stopwatch.StartNew();
            var before = nodes.Reported;
            thread = new Thread(() =>
            {
                while (!stop.Wait(TimeSpan.FromSeconds(5)))
                {
                    try
                    {
                        using var self = Process.GetCurrentProcess();
                        lock (protocol)
                        {
                            protocol.WriteLine("alive\t" + (long)clock.Elapsed.TotalSeconds + "\t" + (nodes.Reported - before) + "\t" + nodes.Current + "\t" + ThreadPool.ThreadCount + "\t" + ThreadPool.PendingWorkItemCount + "\t" + self.WorkingSet64 + "\t" + GC.CollectionCount(2));
                            protocol.Flush();
                        }
                    }
                    catch (Exception error) when (error is IOException || error is ObjectDisposedException)
                    {
                        return;
                    }
                }
            }) { IsBackground = true, Name = "dgf heartbeat", Priority = ThreadPriority.AboveNormal };
            thread.Start();
        }

        public void Dispose()
        {
            stop.Set();
            thread.Join();
        }
    }

    internal sealed class Nodes
    {
        private readonly object gate = new object();
        private readonly Dictionary<string, (string Name, string File)> known = new Dictionary<string, (string, string)>();
        private readonly Dictionary<string, string> state = new Dictionary<string, string>();
        private readonly Dictionary<string, Stopwatch> clocks = new Dictionary<string, Stopwatch>();
        private readonly Dictionary<string, long> times = new Dictionary<string, long>();
        private readonly Stopwatch since = Stopwatch.StartNew();
        private Dictionary<string, List<string>> uidsOf = new Dictionary<string, List<string>>();

        /// Told when a test starts, and when all of a test's rows passed.
        public Action<string>? Started;
        public Action<string, long, string>? Passed;
        /// For the heartbeat: the test running now, and how many tests reported.
        public volatile string? Current;
        public volatile int Reported;

        public void Add(IReadOnlyList<MtpTestNodeUpdate> changes)
        {
            lock (gate)
            {
                foreach (var change in changes.Where(c => c.Uid != null && c.NodeType != "group"))
                {
                    Add(change.Uid!, change);
                }
            }
        }

        private void Add(string uid, MtpTestNodeUpdate change)
        {
            if (!known.ContainsKey(uid))
            {
                known[uid] = (NameOf(change), Text(change, "location.file"));
            }
            var execution = change.ExecutionState ?? "";
            if (execution == "in-progress")
            {
                clocks[uid] = Stopwatch.StartNew();
                Current = known[uid].Name;
                Started?.Invoke(known[uid].Name);
            }
            else if (execution != "discovered")
            {
                state[uid] = execution;
                // Tests run one at a time, so what came since the last one
                // ended (a class fixture, the request itself) is this test's.
                times[uid] = Math.Max(Duration(uid, change), since.ElapsedMilliseconds);
                since.Restart();
                Current = null;
                Reported = Reported + 1;
                TellPassed(known[uid].Name);
            }
        }

        private void TellPassed(string name)
        {
            if (Passed != null && uidsOf.TryGetValue(name, out var uids) && uids.All(u => state.TryGetValue(u, out var s) && s == "passed"))
            {
                Passed(name, uids.Sum(u => times[u]), known[uids[0]].File);
            }
        }

        /// The platform's own timing, else the time since it reported the test started.
        private long Duration(string uid, MtpTestNodeUpdate change)
        {
            var invariant = System.Globalization.CultureInfo.InvariantCulture;
            if (change.Node.TryGetValue("time.duration-ms", out var duration)
                && double.TryParse(Convert.ToString(duration, invariant), System.Globalization.NumberStyles.Float, invariant, out var ms))
            {
                return (long)ms;
            }
            return clocks.TryGetValue(uid, out var clock) ? clock.ElapsedMilliseconds : 0;
        }

        /// Full test name → its node ids (one per row of a theory).
        public Dictionary<string, List<string>> Discovered()
        {
            lock (gate)
            {
                uidsOf = known.GroupBy(k => k.Value.Name).ToDictionary(g => g.Key, g => g.Select(k => k.Key).ToList());
                return uidsOf;
            }
        }

        public void Start()
        {
            lock (gate)
            {
                state.Clear();
                times.Clear();
                since.Restart();
            }
        }

        public List<(string Name, bool Passed, long Milliseconds, string File)> Finished()
        {
            lock (gate)
            {
                return state
                    .Where(s => s.Value != "skipped")
                    .Select(s => (known.TryGetValue(s.Key, out var k) ? k.Name : s.Key, s.Value == "passed", times.TryGetValue(s.Key, out var t) ? t : 0L, known.TryGetValue(s.Key, out var f) ? f.File : ""))
                    .ToList();
            }
        }

        /// The namespace, type and method, as VSTest names a test.
        private static string NameOf(MtpTestNodeUpdate change)
        {
            var type = Text(change, "location.type");
            var method = Text(change, "location.method");
            var paren = method.IndexOf('(');
            if (paren >= 0)
            {
                method = method.Substring(0, paren);
            }
            return type.Length > 0 && method.Length > 0 ? type + "." + method : change.DisplayName ?? change.Uid ?? "";
        }

        private static string Text(MtpTestNodeUpdate change, string key)
        {
            return change.Node.TryGetValue(key, out var value) && value != null ? value.ToString() ?? "" : "";
        }
    }

    /// The test filters dgf passes: empty (every test), or names joined with
    /// "|", each "FullyQualifiedName=name" or "FullyQualifiedName~part".
    internal static class Filter
    {
        public static Func<string, bool> Parse(string filter)
        {
            if (filter.Length == 0)
            {
                return _ => true;
            }
            var terms = filter.Split('|').Select(t => t.Trim().Trim('(', ')')).ToList();
            return name => terms.Any(term =>
            {
                var equals = term.IndexOf('=');
                var contains = term.IndexOf('~');
                if (contains > 0 && (equals < 0 || contains < equals))
                {
                    return name.Contains(Unescape(term.Substring(contains + 1)));
                }
                return equals > 0 && name == Unescape(term.Substring(equals + 1));
            });
        }

        private static string Unescape(string text)
        {
            var out_ = new StringBuilder(text.Length);
            for (var i = 0; i < text.Length; i++)
            {
                if (text[i] == '\\' && i + 1 < text.Length)
                {
                    i++;
                }
                out_.Append(text[i]);
            }
            return out_.ToString();
        }
    }
}
