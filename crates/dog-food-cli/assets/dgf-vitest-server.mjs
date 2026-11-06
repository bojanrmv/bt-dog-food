// Added by dog-food (dgf mutate --run), outside the repository. A Vitest
// reporter that keeps the test runner alive after its first run and runs test
// files on request, so that a run does not pay for a new `ng test` build.
//
// stdin: one JSON line per run, {"files": [...], "report": path, "log": path};
// it ends when stdin closes, so it never outlives dgf.
// stdout: lines starting with \x01dgf, the rest is the runner's own output.
import { writeFileSync } from 'node:fs';
import { createInterface } from 'node:readline';

const say = (...parts) => process.stdout.write(`\n\u0001dgf\t${parts.join('\t')}\n`);
const forward = (path) => path.split(String.fromCharCode(92)).join('/').toLowerCase();

function row(test) {
  const ancestorTitles = [];
  for (let parent = test.parent; parent && parent.type === 'suite'; parent = parent.parent) {
    ancestorTitles.unshift(parent.name);
  }
  const state = test.result().state;
  return { ancestorTitles, title: test.name, status: state, duration: test.diagnostic()?.duration };
}

// The file dgf reads: the shape of Vitest's JSON reporter.
function writeReport(path, modules) {
  const testResults = modules.map((module) => ({
    name: module.moduleId,
    message: module.errors().map((error) => error.message).join('\n'),
    assertionResults: [...module.children.allTests()].map(row),
  }));
  writeFileSync(path, JSON.stringify({ testResults }));
}

function writeLog(path, modules, errors) {
  const text = [...errors, ...modules.flatMap((module) => module.errors())]
    .map((error) => `Error: ${error.stack || error.message}`)
    .join('\n');
  writeFileSync(path, text);
}

export default class DgfServer {
  onInit(vitest) {
    this.vitest = vitest;
    this.report = process.env.DGF_FIRST_REPORT;
    this.log = process.env.DGF_FIRST_LOG;
    if (typeof vitest.runTestSpecifications !== 'function') {
      say('refused', `Vitest ${vitest.version} cannot run test files on request`);
      return;
    }
    // Vitest uses half the processors when it watches.
    const jobs = Number(process.env.DGF_JOBS);
    if (jobs > 0) vitest.config.maxWorkers = jobs;
    say('init');
  }

  onTestRunEnd(modules, errors, reason) {
    const failed = reason === 'failed' || errors.length > 0 || modules.some((m) => !m.ok());
    writeReport(this.report, modules);
    writeLog(this.log, modules, errors);
    this.failed = failed;
    if (!this.serving) {
      this.serving = true;
      say('done', failed ? 1 : 0);
      setTimeout(() => this.serve().catch((error) => {
        say('refused', error.message);
        process.exit(1);
      }), 0);
    }
  }

  async serve() {
    const known = await this.vitest.globTestSpecifications();
    const specs = known.map((spec) => ({ spec, file: forward(spec.moduleId) }));
    for await (const line of createInterface({ input: process.stdin })) {
      const { files, report, log } = JSON.parse(line);
      this.report = report;
      this.log = log;
      const wanted = files.map((file) => `/${forward(file)}`);
      const chosen = specs
        .filter(({ file }) => wanted.length === 0 || wanted.some((w) => file.endsWith(w)))
        .map(({ spec }) => spec);
      await this.vitest.runTestSpecifications(chosen, false);
      say('done', this.failed ? 1 : 0);
    }
    process.exit(0);
  }
}
