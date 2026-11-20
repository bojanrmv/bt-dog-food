// The analyses of the side bar: what they look at, running and stopping
// them, and each one's last result.
"use strict";

const vscode = require("vscode");
const fs = require("fs");
const path = require("path");
const dgf = require("./dgf");
const model = require("./model");
const { program } = require("./settings");

const SCOPE = "dogFood.scope";
const blank = () => ({ running: null, report: null, progress: "", error: "", summary: "", items: [] });
// History has nothing to run or list: it is a dashboard.
const HISTORY = { ...blank(), summary: "saved runs, as a dashboard" };

class Runs {
  constructor(context, output) {
    this.context = context;
    this.output = output;
    this.paths = context.workspaceState.get(SCOPE, []);
    this.states = Object.fromEntries(model.RUNNABLE.map(kind => [kind, blank()]));
    this.roots = new Set();
    this.changed = new vscode.EventEmitter(); // an analysis, or nothing for all of them
    this.finished = new vscode.EventEmitter();
  }

  state(kind) {
    return this.states[kind] || HISTORY;
  }

  /* The folder dgf runs in: the workspace folder of what was chosen, else the first one. */
  folder() {
    const folders = vscode.workspace.workspaceFolders || [];
    const chosen = this.paths.length ? vscode.workspace.getWorkspaceFolder(vscode.Uri.file(this.paths[0])) : null;
    const folder = chosen || folders[0];
    return folder ? folder.uri.fsPath : undefined;
  }

  arguments(kind, format, cwd, options) {
    const paths = this.paths.map(chosen => path.relative(cwd, chosen) || ".");
    return model.command(kind, paths, format, options);
  }

  scopeText() {
    if (!this.paths.length) return "whole workspace";
    if (this.paths.length > 1) return `${this.paths.length} files and folders`;
    return path.relative(this.folder() || "", this.paths[0]).split(path.sep).join("/") || "whole workspace";
  }

  /* Results belong to what was looked at, so a new choice starts empty. */
  setScope(paths) {
    this.stop();
    this.paths = paths;
    this.context.workspaceState.update(SCOPE, paths);
    model.RUNNABLE.forEach(kind => Object.assign(this.states[kind], blank()));
    this.changed.fire();
  }

  async run(kind) {
    const state = this.states[kind];
    const cwd = this.folder();
    if (!cwd) return void vscode.window.showWarningMessage("Dog Food: open a folder first.");
    if (state.running) return;
    const args = this.arguments(kind, "json", cwd);
    this.output.appendLine(`> dgf ${args.join(" ")}`);
    const run = dgf.start(program(), args, { cwd, progress: line => this.progress(kind, line) });
    Object.assign(state, { running: run, error: "", progress: "" });
    this.changed.fire(kind);
    await this.finish(kind, run, cwd);
    this.changed.fire(kind);
    this.finished.fire(kind);
  }

  async finish(kind, run, cwd) {
    const state = this.states[kind];
    const outcome = await this.outcome(kind, run, cwd);
    // When what to look at changed meanwhile, this run's result is no longer wanted.
    if (state.running === run) Object.assign(state, outcome, { running: null, progress: "" });
  }

  /* What a finished run leaves on its row: its findings, or why there are none. */
  async outcome(kind, run, cwd) {
    try {
      const report = dgf.report(await run.done);
      this.remember(cwd, report);
      return { report, summary: model.summary(kind, report), items: model.findings(kind, report) };
    } catch (error) {
      const said = this.problem(error);
      this.output.appendLine(said);
      return { error: said };
    }
  }

  progress(kind, line) {
    this.states[kind].progress = line;
    this.output.appendLine(line);
    this.changed.fire(kind);
  }

  /* Reports name files from the folder dgf ran in, or from the one that holds dgf.toml. */
  remember(cwd, report) {
    this.roots.add(cwd);
    if (typeof report.config === "string") this.roots.add(path.dirname(report.config));
  }

  problem(error) {
    if (error.stopped) return "stopped";
    if (error.code === "ENOENT") return "`dgf` was not found: install it, or set dogFood.path";
    return error.message.trim();
  }

  /* Stops one analysis, or all of them. */
  stop(kind) {
    const kinds = kind ? [kind] : model.RUNNABLE;
    kinds.map(each => this.states[each].running).filter(Boolean).forEach(run => run.stop());
  }

  /* Where a file a report names is: under a folder dgf ran in, or one above it (git names files from the repository). */
  locate(file) {
    if (path.isAbsolute(file)) return file;
    const known = [...this.roots, ...(vscode.workspace.workspaceFolders || []).map(folder => folder.uri.fsPath)];
    return known.flatMap(ancestors).map(root => path.join(root, file)).find(candidate => fs.existsSync(candidate));
  }
}

function ancestors(folder) {
  const parent = path.dirname(folder);
  return parent === folder ? [folder] : [folder, ...ancestors(parent)];
}

module.exports = { Runs };
