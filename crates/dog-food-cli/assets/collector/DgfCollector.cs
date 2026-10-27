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
            AppDomain.CurrentDomain.SetData("DgfTest", testCaseStartArgs.TestCase.FullyQualifiedName);
            started = System.Diagnostics.Stopwatch.GetTimestamp();
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
