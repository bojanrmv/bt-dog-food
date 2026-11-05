// dog-food's test worker. dgf starts one per test project, on the test
// project's own runtime and dependencies (its runtimeconfig and deps.json),
// and keeps it running: the test assembly loads, compiles and warms up once,
// and every later test run costs only its tests. It hosts the VSTest adapter
// found next to the tests (xUnit, NUnit and MSTest each ship one), so it needs
// nothing per framework.
//
// Requests, one per line on standard input:
//   plan<TAB>file[<TAB>nocheck]   file of "test<TAB>mutant id" lines: run those tests, each with its mutant;
//                                 then run each failed test again with no mutant (the kill check), unless
//                                 "nocheck": a test that fails again was failed by state the process
//                                 carries, not by its mutant
//   tests<TAB>file[<TAB>folder]   file holding a VSTest test filter (empty: every test): run the matching
//                                 tests; with a folder, record which mutants each test reaches there
//   list<TAB>file                 the tests a filter on FullyQualifiedName or DisplayName keeps, as results
//   names<TAB>file[<TAB>folder]   run the tests named in the file, one per line; a folder records reach
//   kill<TAB>file                 run the tests named in the file in that order, a class at a time, and
//                                 stop at the first failure (a worker started with one mutant on)
//   quit
// Replies on standard output: "ready<TAB>tests" once started (or "error<TAB>why" and exit), then for
// every request "result<TAB>test<TAB>passed|failed<TAB>ms<TAB>source file" lines, after a plan
// "activated<TAB>test<TAB>mutant id" for each planned mutant that took effect in its test and
// "polluted<TAB>test" for each failed test when the kill check found one that fails without its
// mutant, a "stats" line (bytes allocated so far, gen 0/1/2 collections, heap, working set, peak
// working set) and "done". Anything the tests write to the console goes to standard error.
// A request that takes a while also gets "alive<TAB>seconds<TAB>results so far<TAB>running test<TAB>threads
// <TAB>queued work items<TAB>working set<TAB>gen 2 collections" every 5 seconds, for the debug log.
using System;
using System.Collections.Generic;
using System.Diagnostics;
using System.IO;
using System.Linq;
using System.Reflection;
using System.Runtime.Loader;
using System.Threading;
using Microsoft.VisualStudio.TestPlatform.ObjectModel;
using Microsoft.VisualStudio.TestPlatform.ObjectModel.Adapter;
using Microsoft.VisualStudio.TestPlatform.ObjectModel.Logging;

namespace DogFood.Worker
{
    public static class Program
    {
        public static int Main(string[] args)
        {
            var protocol = new StreamWriter(Console.OpenStandardOutput()) { AutoFlush = false };
            Console.SetOut(Console.Error);
            try
            {
                return Serve(Path.GetFullPath(args[0]), protocol);
            }
            catch (Exception error)
            {
                protocol.WriteLine("error\t" + error.ToString().Replace('\n', ' ').Replace('\r', ' '));
                protocol.Flush();
                return 1;
            }
        }

        private static int Serve(string tests, StreamWriter protocol)
        {
            var folder = Path.GetDirectoryName(tests);
            // Adapters are copied next to the tests but not listed in their
            // deps.json: the test host loads them by path, and so does the worker.
            AssemblyLoadContext.Default.Resolving += (context, name) =>
            {
                var path = Path.Combine(folder, name.Name + ".dll");
                return File.Exists(path) ? context.LoadFromAssemblyPath(path) : null;
            };
            var adapters = Adapters.Load(folder);
            if (adapters.Executors.Count == 0)
            {
                protocol.WriteLine("error\tno test adapter next to " + tests);
                protocol.Flush();
                return 1;
            }
            var server = new Server(tests, adapters, protocol);
            protocol.WriteLine("ready\t" + server.Suite.Names.Count());
            protocol.Flush();
            string line;
            while ((line = Console.In.ReadLine()) != null && server.Handle(line))
            {
            }
            return 0;
        }

        /// Sets AppDomain data `name` while `run` runs.
        internal static void With(string name, object value, Action run)
        {
            AppDomain.CurrentDomain.SetData(name, value);
            try
            {
                run();
            }
            finally
            {
                AppDomain.CurrentDomain.SetData(name, null);
            }
        }

        /// Runs the planned tests; the mutant switches read the plan from
        /// AppDomain data "DgfPlan" while they run, and note in "DgfActivated"
        /// the planned mutants that took effect. Then the kill check: each
        /// failed test runs again with no mutant. One that fails again was
        /// failed by state this process carries (a static built with an
        /// earlier mutant on, a cached value), so no kill of this request can
        /// be trusted: every failed test is reported polluted.
        internal static void RunPlan(string[] lines, bool check, Suite suite, Recorder recorder)
        {
            var plan = new Dictionary<string, int>();
            foreach (var row in lines)
            {
                var tab = row.LastIndexOf('\t');
                int id;
                if (tab > 0 && int.TryParse(row.Substring(tab + 1), out id))
                {
                    plan[row.Substring(0, tab)] = id;
                }
            }
            var activated = new System.Collections.Concurrent.ConcurrentDictionary<string, bool>();
            With("DgfActivated", activated, () => With("DgfPlan", plan, () => suite.Run(plan.Keys, recorder)));
            recorder.Activated.AddRange(activated.Keys);
            var failed = recorder.Results.Where(r => !r.Passed).Select(r => r.Name).ToList();
            if (check && failed.Any(name => FailsWithoutMutant(name, suite)))
            {
                recorder.Polluted.AddRange(failed);
            }
        }

        private static bool FailsWithoutMutant(string name, Suite suite)
        {
            var again = new Recorder();
            suite.Run(new[] { name }, again);
            return again.Results.Any(r => !r.Passed);
        }

        /// Runs `names` in that order, a class at a time, and stops at the
        /// first failure: in a worker started with one mutant on, one failing
        /// test decides it.
        internal static void RunUntilKilled(string[] names, Suite suite, Recorder recorder)
        {
            foreach (var group in ByClass(names.Where(n => n.Length > 0)))
            {
                suite.Run(group, recorder);
                if (recorder.Results.Any(r => !r.Passed))
                {
                    return;
                }
            }
        }

        /// Consecutive names of one class, in order.
        internal static IEnumerable<List<string>> ByClass(IEnumerable<string> names)
        {
            var group = new List<string>();
            string current = null;
            foreach (var name in names)
            {
                var cls = ClassOf(name);
                if (group.Count > 0 && cls != current)
                {
                    yield return group;
                    group = new List<string>();
                }
                current = cls;
                group.Add(name);
            }
            if (group.Count > 0)
            {
                yield return group;
            }
        }

        private static string ClassOf(string name)
        {
            var paren = name.IndexOf('(');
            var method = paren > 0 ? name.Substring(0, paren) : name;
            var dot = method.LastIndexOf('.');
            return dot > 0 ? method.Substring(0, dot) : method;
        }

        /// The tests a filter on FullyQualifiedName or DisplayName keeps, with the test platform's own matching.
        internal static IEnumerable<string> Matching(IEnumerable<string> names, string filter, Func<string, IEnumerable<TestCase>> cases)
        {
            if (filter.Length == 0)
            {
                return names;
            }
            var expression = new Context(RunSettings(), filter).GetTestCaseFilter(null, null);
            Func<string, TestProperty> property = name =>
                name == "FullyQualifiedName" ? TestCaseProperties.FullyQualifiedName
                : name == "DisplayName" ? TestCaseProperties.DisplayName
                : TestProperty.Find(name);
            return names.Where(n => cases(n).Any(c => expression.MatchTestCase(c, property))).ToList();
        }

        /// Runs the tests that match the filter, the way the test host does:
        /// every adapter gets the assembly and applies the filter itself.
        internal static void RunFiltered(string tests, string filter, string reach, Adapters adapters, Recorder recorder)
        {
            var context = new Context(RunSettings(), filter.Length == 0 ? null : filter);
            With("DgfReach", reach, () =>
            {
                foreach (var executor in adapters.Executors.Values.Distinct())
                {
                    executor.RunTests(new[] { tests }, context, recorder);
                }
            });
        }

        /// What every test run is asked for: one test at a time,
        /// so the running test is always known.
        internal static string RunSettings()
        {
            return "<RunSettings>"
                + "<RunConfiguration><DisableParallelization>true</DisableParallelization><DisableAppDomain>true</DisableAppDomain></RunConfiguration>"
                + "<xUnit><ParallelizeTestCollections>false</ParallelizeTestCollections><MaxParallelThreads>1</MaxParallelThreads></xUnit>"
                + "<NUnit><NumberOfTestWorkers>0</NumberOfTestWorkers></NUnit>"
                + "<MSTest><Parallelize><Workers>1</Workers></Parallelize></MSTest>"
                + "</RunSettings>";
        }
    }

    /// Serves the requests of one dgf run.
    internal sealed class Server
    {
        private readonly string tests;
        private readonly Adapters adapters;
        private readonly StreamWriter protocol;
        public readonly Suite Suite;

        public Server(string tests, Adapters adapters, StreamWriter protocol)
        {
            this.tests = tests;
            this.adapters = adapters;
            this.protocol = protocol;
            var context = new Context(Program.RunSettings(), null);
            var found = new Found();
            foreach (var discoverer in adapters.Discoverers)
            {
                discoverer.DiscoverTests(new[] { tests }, context, new Recorder(), found);
            }
            var byName = found.Cases
                .GroupBy(c => c.Id).Select(g => g.First())
                .GroupBy(c => c.FullyQualifiedName)
                .ToDictionary(g => g.Key, g => g.ToList());
            Suite = XunitOrNull(tests) ?? new AdapterSuite(byName, adapters, context);
        }

        private static Suite XunitOrNull(string tests)
        {
            try
            {
                return XunitSuite.Load(tests);
            }
            catch (Exception error)
            {
                Console.Error.WriteLine("dgf worker: xUnit's engine did not load, the adapter runs the tests: " + error.Message);
                return null;
            }
        }

        /// Serves one request; false on "quit".
        public bool Handle(string line)
        {
            var parts = line.TrimEnd('\r', '\n').Split('\t');
            if (parts[0] == "quit")
            {
                return false;
            }
            var recorder = new Recorder();
            try
            {
                using (parts[0] == "list" ? null : new Heartbeat(protocol, () => recorder.Results.Count))
                {
                    Dispatch(parts, recorder);
                }
            }
            catch (Exception error)
            {
                Console.Error.WriteLine("dgf worker: " + error);
            }
            Reply(recorder);
            return true;
        }

        private void Dispatch(string[] parts, Recorder recorder)
        {
            var third = Part(parts, 2);
            var reach = third;
            switch (parts[0])
            {
                case "plan":
                    Program.RunPlan(File.ReadAllLines(parts[1]), third != "nocheck", Suite, recorder);
                    break;
                case "kill":
                    Program.RunUntilKilled(File.ReadAllLines(parts[1]), Suite, recorder);
                    break;
                case "tests":
                    Program.RunFiltered(tests, File.ReadAllText(parts[1]).Trim(), reach, adapters, recorder);
                    break;
                case "list":
                    List(File.ReadAllText(parts[1]).Trim());
                    break;
                case "names":
                    Program.With("DgfReach", reach, () => Suite.Run(File.ReadAllLines(parts[1]), recorder));
                    break;
            }
        }

        private static string Part(string[] parts, int i)
        {
            return parts.Length > i ? parts[i] : null;
        }

        private void List(string filter)
        {
            foreach (var name in Suite.Matching(filter))
            {
                protocol.WriteLine("result\t" + name + "\tpassed\t0\t");
            }
        }

        private void Reply(Recorder recorder)
        {
            foreach (var result in recorder.Results)
            {
                protocol.WriteLine("result\t" + result.Name + "\t" + (result.Passed ? "passed" : "failed") + "\t" + result.Milliseconds + "\t" + result.File);
            }
            foreach (var pair in recorder.Activated)
            {
                protocol.WriteLine("activated\t" + pair);
            }
            foreach (var name in recorder.Polluted)
            {
                protocol.WriteLine("polluted\t" + name);
            }
            using (var self = Process.GetCurrentProcess())
            {
                protocol.WriteLine("stats\t" + GC.GetTotalAllocatedBytes() + "\t" + GC.CollectionCount(0) + "\t" + GC.CollectionCount(1) + "\t" + GC.CollectionCount(2) + "\t" + GC.GetTotalMemory(false) + "\t" + self.WorkingSet64 + "\t" + self.PeakWorkingSet64);
            }
            protocol.WriteLine("done");
            protocol.Flush();
        }
    }

    /// Says every few seconds what the worker is doing while a request runs,
    /// from a thread of its own so a stuck test or thread pool cannot stop it.
    internal sealed class Heartbeat : IDisposable
    {
        private readonly ManualResetEventSlim stop = new ManualResetEventSlim();
        private readonly Thread thread;

        public Heartbeat(StreamWriter protocol, Func<int> results)
        {
            var clock = Stopwatch.StartNew();
            thread = new Thread(() =>
            {
                while (!stop.Wait(TimeSpan.FromSeconds(5)))
                {
                    try
                    {
                        using (var self = Process.GetCurrentProcess())
                        {
                            protocol.WriteLine("alive\t" + (long)clock.Elapsed.TotalSeconds + "\t" + results() + "\t" + AppDomain.CurrentDomain.GetData("DgfTest") + "\t" + ThreadPool.ThreadCount + "\t" + ThreadPool.PendingWorkItemCount + "\t" + self.WorkingSet64 + "\t" + GC.CollectionCount(2));
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

    /// The tests by name, and a way to run some of them.
    internal abstract class Suite
    {
        public abstract IEnumerable<string> Names { get; }
        public abstract IEnumerable<string> Matching(string filter);
        public abstract void Run(IEnumerable<string> names, Recorder recorder);
    }

    /// Tests run through the VSTest adapter next to them.
    internal sealed class AdapterSuite : Suite
    {
        private readonly Dictionary<string, List<TestCase>> byName;
        private readonly Adapters adapters;
        private readonly Context context;

        public AdapterSuite(Dictionary<string, List<TestCase>> byName, Adapters adapters, Context context)
        {
            this.byName = byName;
            this.adapters = adapters;
            this.context = context;
        }

        public override IEnumerable<string> Names => byName.Keys;

        public override IEnumerable<string> Matching(string filter) => Program.Matching(byName.Keys, filter, n => byName[n]);

        public override void Run(IEnumerable<string> names, Recorder recorder)
        {
            var chosen = names.Where(byName.ContainsKey).SelectMany(n => byName[n]);
            foreach (var group in chosen.GroupBy(c => Adapters.Key(c.ExecutorUri)))
            {
                ITestExecutor executor;
                if (adapters.Executors.TryGetValue(group.Key, out executor))
                {
                    executor.RunTests(group.ToList(), context, recorder);
                }
            }
        }
    }

    /// xUnit v2 tests run by xUnit's own engine with synchronous messages: a
    /// test's start reaches the recorder on the test's thread before the test
    /// runs, so the switches always know it. The adapter reports it from
    /// another thread, sometimes after the test began.
    internal sealed class XunitSuite : Suite
    {
        private readonly Xunit.Abstractions.ITestFrameworkExecutor executor;
        private readonly Dictionary<string, List<Xunit.Abstractions.ITestCase>> byName;
        private readonly string source;

        private XunitSuite(Xunit.Abstractions.ITestFrameworkExecutor executor, Dictionary<string, List<Xunit.Abstractions.ITestCase>> byName, string source)
        {
            this.executor = executor;
            this.byName = byName;
            this.source = source;
        }

        public static XunitSuite Load(string tests)
        {
            if (!File.Exists(Path.Combine(Path.GetDirectoryName(tests), "xunit.execution.dotnet.dll")))
            {
                return null;
            }
            var assembly = Assembly.Load(AssemblyName.GetAssemblyName(tests));
            var framework = new Xunit.Sdk.XunitTestFramework(new XunitFound());
            var found = new XunitFound();
            using (var discoverer = framework.GetDiscoverer(Xunit.Sdk.Reflector.Wrap(assembly)))
            {
                var options = new XunitOptions();
                options.SetValue("xunit.discovery.PreEnumerateTheories", (bool?)true);
                discoverer.Find(false, found, options);
                found.Done.WaitOne();
            }
            var byName = found.Cases.GroupBy(Name).ToDictionary(g => g.Key, g => g.ToList());
            return byName.Count == 0 ? null : new XunitSuite(framework.GetExecutor(assembly.GetName()), byName, tests);
        }

        /// The name the adapter gives the test.
        public static string Name(Xunit.Abstractions.ITestCase test) => test.TestMethod.TestClass.Class.Name + "." + test.TestMethod.Method.Name;

        public override IEnumerable<string> Names => byName.Keys;

        public override IEnumerable<string> Matching(string filter) =>
            Program.Matching(byName.Keys, filter, n => new[] { new TestCase(n, new Uri("executor://dgf/xunit"), source) { DisplayName = n } });

        public override void Run(IEnumerable<string> names, Recorder recorder)
        {
            var chosen = names.Where(byName.ContainsKey).SelectMany(n => byName[n]).ToList();
            if (chosen.Count == 0)
            {
                return;
            }
            var sink = new XunitRun(recorder);
            var options = new XunitOptions();
            options.SetValue("xunit.execution.SynchronousMessageReporting", (bool?)true);
            options.SetValue("xunit.execution.DisableParallelization", (bool?)true);
            options.SetValue("xunit.execution.MaxParallelThreads", (int?)1);
            executor.RunTests(chosen, sink, options);
            sink.Done.WaitOne();
        }
    }

    internal sealed class XunitOptions : Xunit.Abstractions.ITestFrameworkDiscoveryOptions, Xunit.Abstractions.ITestFrameworkExecutionOptions
    {
        private readonly Dictionary<string, object> values = new Dictionary<string, object>();

        public TValue GetValue<TValue>(string name)
        {
            object value;
            return values.TryGetValue(name, out value) && value is TValue typed ? typed : default(TValue);
        }

        public void SetValue<TValue>(string name, TValue value) => values[name] = value;
    }

    /// xUnit's discovered tests.
    internal sealed class XunitFound : Xunit.Abstractions.IMessageSink
    {
        public readonly List<Xunit.Abstractions.ITestCase> Cases = new List<Xunit.Abstractions.ITestCase>();
        public readonly ManualResetEvent Done = new ManualResetEvent(false);

        public bool OnMessage(Xunit.Abstractions.IMessageSinkMessage message)
        {
            if (message is Xunit.Abstractions.ITestCaseDiscoveryMessage found)
            {
                Cases.Add(found.TestCase);
            }
            else if (message is Xunit.Abstractions.IDiscoveryCompleteMessage)
            {
                Done.Set();
            }
            return true;
        }
    }

    /// Each test's start, result and end, as xUnit reports them.
    internal sealed class XunitRun : Xunit.Abstractions.IMessageSink
    {
        public readonly ManualResetEvent Done = new ManualResetEvent(false);
        private readonly Recorder recorder;
        private bool failed;

        public XunitRun(Recorder recorder)
        {
            this.recorder = recorder;
        }

        public bool OnMessage(Xunit.Abstractions.IMessageSinkMessage message)
        {
            switch (message)
            {
                case Xunit.Abstractions.ITestStarting starting:
                    failed = false;
                    recorder.Start(XunitSuite.Name(starting.TestCase));
                    break;
                case Xunit.Abstractions.ITestFailed _:
                    failed = true;
                    break;
                case Xunit.Abstractions.ITestFinished finished:
                    recorder.Finish(XunitSuite.Name(finished.TestCase), !failed, (long)(finished.ExecutionTime * 1000), "");
                    recorder.End();
                    break;
                case Xunit.Abstractions.ITestAssemblyFinished _:
                    Done.Set();
                    break;
            }
            return true;
        }
    }

    /// Mutants the switches reached while no test was known: the adapter may
    /// report a test's start after the test began, and class fixtures run
    /// before it. They belong to the test that starts next (and its class).
    internal static class Pending
    {
        public static void Give(string test, string folder)
        {
            var text = Take(test);
            if (folder != null && text.Length > 0)
            {
                File.AppendAllText(Path.Combine(folder, "pending-" + Process.GetCurrentProcess().Id + ".txt"), text);
            }
        }

        /// "test<TAB>mutant" lines for the pending mutants, which are no longer pending.
        private static string Take(string test)
        {
            var pending = AppDomain.CurrentDomain.GetData("DgfPending") as System.Collections.Concurrent.ConcurrentDictionary<int, bool>;
            var text = new System.Text.StringBuilder();
            foreach (var id in pending?.Keys ?? Enumerable.Empty<int>())
            {
                bool ignored;
                if (pending.TryRemove(id, out ignored))
                {
                    text.Append(test).Append('\t').Append(id).Append('\n');
                }
            }
            return text.ToString();
        }
    }

    /// The discoverers and executors of every test adapter in a folder.
    internal sealed class Adapters
    {
        public readonly List<ITestDiscoverer> Discoverers = new List<ITestDiscoverer>();
        public readonly Dictionary<string, ITestExecutor> Executors = new Dictionary<string, ITestExecutor>();

        /// Test cases carry their executor's address as a Uri, which lowercases
        /// it and ends it with a slash.
        public static string Key(Uri uri)
        {
            return uri.ToString().TrimEnd('/').ToLowerInvariant();
        }

        public static Adapters Load(string folder)
        {
            var found = new Adapters();
            var types = Directory.GetFiles(folder, "*.dll")
                .Where(f => Path.GetFileName(f).IndexOf("testadapter", StringComparison.OrdinalIgnoreCase) >= 0)
                .SelectMany(TypesIn)
                .Where(t => !t.IsAbstract && t.IsClass && t.GetConstructor(Type.EmptyTypes) != null);
            foreach (var type in types)
            {
                found.Add(type);
            }
            return found;
        }

        private static IEnumerable<Type> TypesIn(string file)
        {
            try
            {
                return Assembly.LoadFrom(file).GetTypes();
            }
            catch (ReflectionTypeLoadException partial)
            {
                return partial.Types.Where(t => t != null);
            }
            catch (Exception)
            {
                return Enumerable.Empty<Type>();
            }
        }

        private void Add(Type type)
        {
            if (typeof(ITestDiscoverer).IsAssignableFrom(type) && Create(type) is ITestDiscoverer discoverer)
            {
                Discoverers.Add(discoverer);
            }
            var uri = type.GetCustomAttributesData()
                .FirstOrDefault(a => a.AttributeType.Name == "ExtensionUriAttribute")?
                .ConstructorArguments[0].Value as string;
            if (uri != null && typeof(ITestExecutor).IsAssignableFrom(type) && Create(type) is ITestExecutor executor)
            {
                Executors[Key(new Uri(uri))] = executor;
            }
        }

        private static object Create(Type type)
        {
            try
            {
                return Activator.CreateInstance(type);
            }
            catch (Exception)
            {
                return null;
            }
        }
    }

    internal sealed class Settings : IRunSettings
    {
        private readonly string xml;

        public Settings(string xml)
        {
            this.xml = xml;
        }

        public string SettingsXml => xml;

        public ISettingsProvider GetSettings(string settingsName) => null;
    }

    /// A test run's context; with a filter, the VSTest filter expression the
    /// adapters match their tests against.
    internal sealed class Context : IRunContext
    {
        private readonly string filter;

        public Context(string settings, string filter)
        {
            RunSettings = new Settings(settings);
            this.filter = filter;
        }

        public IRunSettings RunSettings { get; }
        public bool KeepAlive => false;
        public bool InIsolation => false;
        public bool IsDataCollectionEnabled => false;
        public bool IsBeingDebugged => false;
        public string TestRunDirectory { get; } = Path.Combine(Path.GetTempPath(), "dgf-worker-" + Process.GetCurrentProcess().Id);
        public string SolutionDirectory => null;

        public ITestCaseFilterExpression GetTestCaseFilter(IEnumerable<string> supportedProperties, Func<string, TestProperty> propertyProvider)
        {
            if (filter == null)
            {
                return null;
            }
            // The test platform's own filter types, from the test project's
            // copy of it: the same parsing and matching as dotnet test.
            var common = Assembly.Load(new AssemblyName("Microsoft.VisualStudio.TestPlatform.Common"));
            var wrapper = Activator.CreateInstance(
                common.GetType("Microsoft.VisualStudio.TestPlatform.Common.Filtering.FilterExpressionWrapper", true),
                new object[] { filter });
            return (ITestCaseFilterExpression)Activator.CreateInstance(
                common.GetType("Microsoft.VisualStudio.TestPlatform.Common.Filtering.TestCaseFilterExpression", true),
                new[] { wrapper });
        }
    }

    internal sealed class Found : ITestCaseDiscoverySink
    {
        public readonly List<TestCase> Cases = new List<TestCase>();

        public void SendTestCase(TestCase discoveredTest) => Cases.Add(discoveredTest);
    }

    internal sealed class Result
    {
        public string Name;
        public bool Passed;
        public long Milliseconds;
        public string File;
    }

    /// Tells the mutant switches which test runs (AppDomain data "DgfTest"),
    /// and keeps each test's result: all rows of a
    /// theory or test case source together, failed when any row failed.
    internal sealed class Recorder : IFrameworkHandle
    {
        public readonly List<Result> Results = new List<Result>();
        /// "test<TAB>mutant id" for each planned mutant that took effect in its test.
        public readonly List<string> Activated = new List<string>();
        /// Failed tests whose failure the kill check could not pin on their mutant.
        public readonly List<string> Polluted = new List<string>();
        private readonly Dictionary<string, Result> byName = new Dictionary<string, Result>();
        // A test's time is the wall time since the one before it ended: with the class fixtures it set up.
        private readonly Stopwatch since = Stopwatch.StartNew();

        public bool EnableShutdownAfterTestRun { get; set; }

        public void RecordStart(TestCase testCase) => Start(testCase.FullyQualifiedName);

        public void RecordEnd(TestCase testCase, TestOutcome outcome) => End();

        public void RecordResult(TestResult testResult) =>
            Finish(testResult.TestCase.FullyQualifiedName, testResult.Outcome != TestOutcome.Failed, (long)testResult.Duration.TotalMilliseconds, testResult.TestCase.CodeFilePath);

        public void Start(string name)
        {
            AppDomain.CurrentDomain.SetData("DgfTest", name);
            Pending.Give(name, AppDomain.CurrentDomain.GetData("DgfReach") as string);
        }

        public void End() => AppDomain.CurrentDomain.SetData("DgfTest", null);

        public void Finish(string name, bool passed, long milliseconds, string file)
        {
            Result result;
            if (!byName.TryGetValue(name, out result))
            {
                result = new Result { Name = name, Passed = true, File = file ?? "" };
                byName[name] = result;
                Results.Add(result);
            }
            result.Passed &= passed;
            result.Milliseconds += Math.Max(milliseconds, since.ElapsedMilliseconds);
            since.Restart();
        }

        public void RecordAttachments(IList<AttachmentSet> attachmentSets)
        {
        }

        public void SendMessage(TestMessageLevel testMessageLevel, string message)
        {
            if (testMessageLevel == TestMessageLevel.Error)
            {
                Console.Error.WriteLine(message);
            }
        }

        public int LaunchProcessWithDebuggerAttached(string filePath, string workingDirectory, string arguments, IDictionary<string, string> environmentVariables) => -1;
    }
}
