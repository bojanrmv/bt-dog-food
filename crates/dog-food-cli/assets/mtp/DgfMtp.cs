// dog-food's test worker for Microsoft.Testing.Platform test applications
// (xUnit v3, the MSTest runner, TUnit): it starts the test application once in
// the platform's server mode and keeps it running, so every later test run
// costs only its tests. The test application has no VSTest collector to say
// which test runs, so a planned run asks for one test at a time and tells the
// mutant switches the test and its mutant through a small control file
// (DGF_CONTROL), which they read again when its first four bytes change.
//
// Same requests and replies as dog-food's VSTest worker:
//   plan<TAB>file[<TAB>nocheck]   file of "test<TAB>mutant id" lines: run those tests, each with its mutant;
//                                 a failed test runs again at once with no mutant (the kill check) unless
//                                 "nocheck", and one that fails again marks every failed test polluted
//   kill<TAB>file                 run the tests named in the file in that order and stop at the first
//                                 failure (a test application started with one mutant on)
//   tests<TAB>file[<TAB>folder]   file holding a test filter (empty: every test): run the matching tests;
//                                 with a folder, record which mutants each test reaches there
//   quit
// Replies: "ready<TAB>tests" (or "error<TAB>why"), then per request "result<TAB>test<TAB>passed|failed<TAB>ms<TAB>file"
// lines and "done".
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.IO.MemoryMappedFiles;
using System.Linq;
using System.Text;
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
            var server = new Server(client, nodes, control, nodes.Discovered(), protocol);
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

        public Server(IMtpServerClient client, Nodes nodes, Control control, Dictionary<string, List<string>> byName, StreamWriter protocol)
        {
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
                await Serve(parts, results);
            }
            catch (Exception error)
            {
                // The test application failed the request, as a rule because
                // it died: this worker cannot serve another. It ends with
                // the results it has and no "done", so dgf sees a crash and
                // starts a new one, instead of pooling this one as warm.
                Console.Error.WriteLine("dgf mtp worker: " + error);
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
                await Plan(File.ReadAllLines(parts[1]), third != "nocheck", results);
            }
            else if (parts[0] == "tests")
            {
                await Tests(File.ReadAllText(parts[1]).Trim(), third, results);
            }
            else if (parts[0] == "kill")
            {
                await Kill(File.ReadAllLines(parts[1]), results);
            }
        }

        /// dgf lists each mutant's tests quickest first; once one fails, the
        /// mutant is killed and its other tests are skipped. The kill check:
        /// a failed test runs again at once with no mutant, and one that
        /// fails again was failed by state the test application carries, so
        /// every failed test of this request is reported polluted. The
        /// switches note each planned mutant that took effect in a file
        /// named in the control file.
        private async Task Plan(string[] rows, bool check, Results results)
        {
            var killed = new HashSet<int>();
            var failed = new List<string>();
            var polluted = false;
            var activated = control.Path + ".activated";
            foreach (var (name, id) in rows.Select(Row).Where(r => r.Id >= 0 && byName.ContainsKey(r.Name)))
            {
                if (killed.Contains(id) || await RunOne(name, id, null, activated, results))
                {
                    continue;
                }
                killed.Add(id);
                failed.Add(name);
                polluted |= check && !await RunOne(name, -1, null, null, new Results());
            }
            if (polluted)
            {
                results.Polluted.AddRange(failed);
            }
            TakeActivated(activated, results);
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
        /// decides it.
        private async Task Kill(string[] names, Results results)
        {
            foreach (var name in names.Where(byName.ContainsKey))
            {
                if (!await RunOne(name, -1, null, null, results))
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

        /// Which test reaches what is only known one test at a time.
        private async Task Tests(string filter, string? reach, Results results)
        {
            var matches = Filter.Parse(filter);
            var chosen = byName.Keys.Where(matches).ToList();
            if (reach == null)
            {
                control.Set("", -1, null, null);
                nodes.Start();
                await client.RunTestsAsync(chosen.SelectMany(n => byName[n]).ToList());
                results.Add(nodes.Finished());
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
            control.Set(name, mutant, reach, activated);
            nodes.Start();
            await client.RunTestsAsync(byName[name]);
            var finished = nodes.Finished();
            results.Add(finished);
            return finished.All(r => r.Item2);
        }

        private void Reply(Results results, bool done)
        {
            foreach (var result in results.All)
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
    /// run, then "test\nmutant id\nreach folder" in UTF-8.
    internal sealed class Control : IDisposable
    {
        private const int Size = 64 * 1024;
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

        public void Set(string test, int mutant, string? reach, string? activated)
        {
            var text = Encoding.UTF8.GetBytes(test + "\n" + mutant + "\n" + (reach ?? "") + "\n" + (activated ?? ""));
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
        private readonly Dictionary<string, Result> byName = new Dictionary<string, Result>();

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
    internal sealed class Nodes
    {
        private readonly object gate = new object();
        private readonly Dictionary<string, (string Name, string File)> known = new Dictionary<string, (string, string)>();
        private readonly Dictionary<string, string> state = new Dictionary<string, string>();
        private readonly Dictionary<string, Stopwatch> clocks = new Dictionary<string, Stopwatch>();
        private readonly Dictionary<string, long> times = new Dictionary<string, long>();

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
            }
            else if (execution != "discovered")
            {
                state[uid] = execution;
                times[uid] = Duration(uid, change);
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
                return known.GroupBy(k => k.Value.Name).ToDictionary(g => g.Key, g => g.Select(k => k.Key).ToList());
            }
        }

        public void Start()
        {
            lock (gate)
            {
                state.Clear();
                times.Clear();
            }
        }

        public List<(string, bool, long, string)> Finished()
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
