// The dashboard: dgf's HTML report of one analysis in an editor tab, with a
// bar to switch between the analyses, run one again or open it in the browser.
"use strict";

const vscode = require("vscode");
const fs = require("fs");
const path = require("path");
const dgf = require("./dgf");
const model = require("./model");
const page = require("./page");
const { program } = require("./settings");

const DARK = [vscode.ColorThemeKind.Dark, vscode.ColorThemeKind.HighContrast];

class Dashboard {
  /* `folder` is where the report pages are written. */
  constructor(runs, folder) {
    this.runs = runs;
    this.folder = folder;
    this.kind = "complexity";
    this.panel = null;
    this.ready = new vscode.EventEmitter(); // a page has been drawn
  }

  async show(kind) {
    this.kind = kind || this.kind;
    this.reveal();
    await this.render();
  }

  reveal() {
    if (this.panel) return this.panel.reveal();
    this.panel = vscode.window.createWebviewPanel("dogFood.dashboard", "Dog Food", vscode.ViewColumn.Active, { enableScripts: true, retainContextWhenHidden: true });
    this.panel.onDidDispose(() => (this.panel = null));
    this.panel.webview.onDidReceiveMessage(message => this.received(message));
  }

  /* Draws the shown analysis again, if it is `kind` (or whatever it is, without one). */
  async refresh(kind) {
    if (this.panel && (!kind || kind === this.kind)) await this.render();
  }

  async render() {
    const kind = this.kind;
    const options = { dark: DARK.includes(vscode.window.activeColorTheme.kind), kind, kinds: model.KINDS };
    this.panel.title = `Dog Food: ${model.KINDS[kind].title}`;
    const html = await this.build(kind).then(
      report => page.prepare(report, options),
      error => page.message(this.runs.problem(error), options)
    );
    // The tab may have been closed or switched while dgf worked.
    if (this.panel && this.kind === kind) this.panel.webview.html = html;
  }

  file(kind) {
    return path.join(this.folder, `${kind}.html`);
  }

  /* The page dgf writes for an analysis. A mutation page shows the last run: testing starts from the side bar. */
  async build(kind) {
    const cwd = this.runs.folder();
    if (!cwd) throw new Error("Open a folder to see its dashboard.");
    const file = this.file(kind);
    await fs.promises.mkdir(this.folder, { recursive: true });
    await fs.promises.rm(file, { force: true });
    const args = this.runs.arguments(kind, "html", cwd, { output: file, cached: true });
    const result = await dgf.start(program(), args, { cwd }).done;
    return fs.promises.readFile(file, "utf8").catch(() => {
      throw new Error(dgf.failure(result));
    });
  }

  received(message) {
    const act = ACTIONS[message.type];
    if (act) act(this, message);
  }
}

const ACTIONS = {
  show: (dashboard, message) => dashboard.show(message.kind),
  run: (dashboard, message) => (model.RUNNABLE.includes(message.kind) ? dashboard.runs.run(message.kind) : dashboard.render()),
  browser: dashboard => vscode.env.openExternal(vscode.Uri.file(dashboard.file(dashboard.kind))),
  open: (dashboard, message) => vscode.commands.executeCommand("dogFood.open", message.file, message.line),
  ready: (dashboard, message) => dashboard.ready.fire(message),
};

module.exports = { Dashboard };
