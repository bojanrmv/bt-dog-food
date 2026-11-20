// Keeps the copies of each workspace folder known, for the notes above
// functions: one duplication scan when a folder is first looked at, and
// another a moment after a source file changes on disk.
"use strict";

const vscode = require("vscode");
const dgf = require("./dgf");
const copies = require("./copies");
const { program } = require("./settings");

// Saves come in bursts, and a scan reads the whole folder.
const WAIT = 1500;
// A scan that failed is not tried again at every file that is opened.
const RETRY = 60000;

class Duplicates {
  /* `changed` is called when a scan found something new to show, `log` with a line about each scan. */
  constructor(changed, log) {
    this.changed = changed;
    this.log = log;
    this.folders = new Map();
  }

  state(folder) {
    if (!this.folders.has(folder)) this.folders.set(folder, { index: null, run: null, again: false, timer: null, failed: 0 });
    return this.folders.get(folder);
  }

  /* The first look at a folder scans it. */
  ensure(folder) {
    const state = this.state(folder);
    const failedJustNow = Date.now() - state.failed < RETRY;
    if (!state.index && !state.run && !failedJustNow) this.scan(folder);
  }

  /* A file changed: scan again soon, once the changes stop coming. */
  refresh(folder) {
    const state = this.state(folder);
    clearTimeout(state.timer);
    state.timer = setTimeout(() => this.scan(folder), WAIT);
  }

  /* One scan of a folder at a time; a change during it is scanned afterwards. */
  async scan(folder) {
    const state = this.state(folder);
    if (state.run) return void (state.again = true);
    const started = Date.now();
    state.run = dgf.start(program(), ["duplication", "--format", "json"], { cwd: folder });
    vscode.window.setStatusBarMessage("$(sync~spin) Dog Food: looking for copies", state.run.done.catch(() => {}));
    try {
      const report = dgf.report(await state.run.done);
      state.index = copies.index(report, folder);
      this.log(`copies in ${folder}: ${report.classes.length} groups in ${state.index.size} files (${((Date.now() - started) / 1000).toFixed(1)} s)`);
      this.changed();
    } catch (error) {
      state.failed = Date.now();
      this.log(`looking for copies in ${folder} failed: ${error.message}`);
    }
    state.run = null;
    if (state.again) this.refresh(folder);
    state.again = false;
  }

  notes(document, functions) {
    const folder = vscode.workspace.getWorkspaceFolder(document.uri);
    const state = folder && this.folders.get(folder.uri.fsPath);
    return state && state.index ? copies.notes(state.index, document.uri.fsPath, functions) : [];
  }

  dispose() {
    for (const state of this.folders.values()) {
      clearTimeout(state.timer);
      if (state.run) state.run.stop();
    }
  }
}

module.exports = { Duplicates };
