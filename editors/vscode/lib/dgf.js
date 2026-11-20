// Runs the dgf command line tool: its report from stdout, its progress lines
// from stderr while it works, and a stop that takes its child processes too.
"use strict";

const { spawn, execFile } = require("child_process");

const WINDOWS = process.platform === "win32";

/* Starts dgf. `done` resolves to { code, stdout, stderr }; `stop()` ends the run. */
function start(file, args, { cwd, progress = () => {} }) {
  // DGF_PROGRESS: dgf only reports its steps to a terminal unless asked.
  // Off Windows it leads its own process group, so stop() reaches its children.
  const child = spawn(file, args, { cwd, env: { ...process.env, DGF_PROGRESS: "1" }, windowsHide: true, detached: !WINDOWS });
  const out = [];
  const err = [];
  const lines = splitter(progress);
  child.stdout.on("data", chunk => out.push(chunk));
  child.stderr.on("data", chunk => {
    err.push(chunk);
    lines(chunk.toString("utf8"));
  });
  const run = { stopped: false };
  run.done = new Promise((resolve, reject) => {
    child.on("error", reject);
    child.on("close", code => resolve({ code, stopped: run.stopped, stdout: text(out), stderr: text(err) }));
  });
  run.stop = () => {
    run.stopped = true;
    stopTree(child);
  };
  return run;
}

const text = chunks => Buffer.concat(chunks).toString("utf8");

/* Progress arrives in pieces, and a counter rewrites its line with \r: hand over whole lines. */
function splitter(each) {
  let rest = "";
  return piece => {
    const parts = (rest + piece).split(/[\r\n]+/);
    rest = parts.pop();
    parts.map(line => line.trim()).filter(Boolean).forEach(each);
  };
}

/* dgf's builds and test runs are its children; ending dgf alone would leave them running. */
function stopTree(child) {
  if (child.exitCode !== null || !child.pid) return;
  if (WINDOWS) {
    execFile("taskkill", ["/pid", String(child.pid), "/T", "/F"], () => {});
    return;
  }
  try {
    process.kill(-child.pid, "SIGKILL");
  } catch {
    child.kill("SIGKILL");
  }
}

/* The report dgf printed, or an error that says why there is none. */
function report(result) {
  if (result.stopped) throw Object.assign(new Error("stopped"), { stopped: true });
  try {
    return JSON.parse(result.stdout);
  } catch {
    throw new Error(failure(result));
  }
}

/* What dgf said went wrong: its last lines that are not progress steps. */
function failure(result) {
  const said = result.stderr.split(/[\r\n]+/).filter(line => line && !line.startsWith(" "));
  return said.slice(-3).join(" ").trim() || `dgf ended with code ${result.code} and no report`;
}

module.exports = { start, report, failure };
