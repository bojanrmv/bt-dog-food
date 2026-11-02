// dog-food's in-process data collector. The .NET test host loads it next to
// the tests (through the run settings dgf writes) and calls it around every
// test. It tells the mutant switches which test is running (AppDomain data "DgfTest"), and writes each
// test's result, source file and time, so one test run can try a different mutant in every test.
using System;
using System.IO;
using Microsoft.VisualStudio.TestPlatform.ObjectModel;
using Microsoft.VisualStudio.TestPlatform.ObjectModel.DataCollection;
using Microsoft.VisualStudio.TestPlatform.ObjectModel.DataCollector.InProcDataCollector;
using Microsoft.VisualStudio.TestPlatform.ObjectModel.InProcDataCollector;

namespace DogFood.Collector
{
    public class DgfCollector : InProcDataCollection
    {
        private static readonly object Gate = new object();
        private string results;
        private long started;

        public void Initialize(IDataCollectionSink dataCollectionSink)
        {
            var folder = Environment.GetEnvironmentVariable("DGF_RESULTS");
            results = string.IsNullOrEmpty(folder)
                ? null
                : Path.Combine(folder, "results-" + Guid.NewGuid().ToString("N") + ".txt");
        }

        public void TestSessionStart(TestSessionStartArgs testSessionStartArgs)
        {
        }

        public void TestCaseStart(TestCaseStartArgs testCaseStartArgs)
        {
            var name = testCaseStartArgs.TestCase.FullyQualifiedName;
            AppDomain.CurrentDomain.SetData("DgfTest", name);
            GivePending(name);
            started = System.Diagnostics.Stopwatch.GetTimestamp();
        }

        // Mutants the switches reached while no test was known (this start
        // can arrive after the test began; class fixtures run before it)
        // belong to the test that starts now.
        private static void GivePending(string test)
        {
            var folder = Environment.GetEnvironmentVariable("DGF_REACH");
            var text = TakePending(test);
            if (string.IsNullOrEmpty(folder) || text.Length == 0)
            {
                return;
            }
            lock (Gate)
            {
                File.AppendAllText(Path.Combine(folder, "pending-" + System.Diagnostics.Process.GetCurrentProcess().Id + ".txt"), text);
            }
        }

        // "test<TAB>mutant" lines for the pending mutants, which are no longer pending.
        private static string TakePending(string test)
        {
            var pending = AppDomain.CurrentDomain.GetData("DgfPending") as System.Collections.Concurrent.ConcurrentDictionary<int, bool>;
            var text = new System.Text.StringBuilder();
            if (pending == null)
            {
                return "";
            }
            foreach (var id in pending.Keys)
            {
                bool ignored;
                if (pending.TryRemove(id, out ignored))
                {
                    text.Append(test).Append('\t').Append(id).Append('\n');
                }
            }
            return text.ToString();
        }

        public void TestCaseEnd(TestCaseEndArgs testCaseEndArgs)
        {
            AppDomain.CurrentDomain.SetData("DgfTest", null);
            if (results == null)
            {
                return;
            }
            var test = testCaseEndArgs.DataCollectionContext.TestCase;
            var outcome = testCaseEndArgs.TestOutcome == TestOutcome.Failed ? "failed" : "passed";
            var milliseconds = (System.Diagnostics.Stopwatch.GetTimestamp() - started) * 1000 / System.Diagnostics.Stopwatch.Frequency;
            var line = test.FullyQualifiedName + "\t" + outcome + "\t" + test.Source + "\t" + (test.CodeFilePath ?? "") + "\t" + milliseconds + "\n";
            lock (Gate)
            {
                File.AppendAllText(results, line);
            }
        }

        public void TestSessionEnd(TestSessionEndArgs testSessionEndArgs)
        {
        }
    }
}
