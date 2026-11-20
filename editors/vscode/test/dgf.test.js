"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const fs = require("node:fs");
const os = require("node:os");
const path = require("node:path");
const dgf = require("../lib/dgf");

/* Stands in for dgf: a node script with the given body. */
function program(t, body) {
  const folder = fs.mkdtempSync(path.join(os.tmpdir(), "dgf-vscode-test-"));
  t.after(() => fs.rmSync(folder, { recursive: true, force: true }));
  const script = path.join(folder, "fake.js");
  fs.writeFileSync(script, body);
  return { start: options => dgf.start(process.execPath, [script], { cwd: folder, ...options }) };
}

const alive = pid => {
  try {
    process.kill(pid, 0);
    return true;
  } catch {
    return false;
  }
};

async function gone(pid) {
  for (let tries = 0; tries < 100 && alive(pid); tries++) await new Promise(resolve => setTimeout(resolve, 50));
  return !alive(pid);
}

test("the report comes from stdout and whole progress lines from stderr", async t => {
  const fake = program(t, `
    process.stderr.write("  finding source files: 1\\r  finding sou");
    setTimeout(() => {
      process.stderr.write("rce files: 2\\n  scoring every function: 3\\n");
      process.stdout.write(JSON.stringify({ asked: process.env.DGF_PROGRESS, functions: [] }));
    }, 100);
  `);
  const seen = [];
  const result = await fake.start({ progress: line => seen.push(line) }).done;
  assert.deepEqual(seen, ["finding source files: 1", "finding source files: 2", "scoring every function: 3"]);
  assert.deepEqual(dgf.report(result), { asked: "1", functions: [] });
});

test("a run that fails gives dgf's words, without the progress lines", async t => {
  const fake = program(t, `
    process.stderr.write("  finding source files: 1\\nerror: no saved runs in .dgf/history\\n");
    process.exitCode = 2;
  `);
  const result = await fake.start({}).done;
  assert.throws(() => dgf.report(result), { message: "error: no saved runs in .dgf/history" });
});

test("a run that says nothing is reported with its exit code", async t => {
  const result = await program(t, "process.exitCode = 3;").start({}).done;
  assert.throws(() => dgf.report(result), { message: "dgf ended with code 3 and no report" });
});

test("stopping a run stops what it started too", async t => {
  const fake = program(t, `
    const { spawn } = require("child_process");
    const child = spawn(process.execPath, ["-e", "setInterval(() => {}, 1000)"], { stdio: "ignore" });
    process.stdout.write(String(child.pid));
    process.stderr.write("started\\n");
    setInterval(() => {}, 1000);
  `);
  const run = fake.start({ progress: () => run.stop() });
  const result = await run.done;
  const child = Number(result.stdout);
  assert.ok(child > 0, "the child's process id");
  assert.ok(await gone(child), "the child is gone");
  assert.throws(() => dgf.report(result), { stopped: true });
});

test("a program that is not there is an error, not a hang", async () => {
  const run = dgf.start(path.join(os.tmpdir(), "no-such-dgf"), [], { cwd: os.tmpdir() });
  await assert.rejects(run.done, { code: "ENOENT" });
});
