// The dog-food view in the side bar: what the analyses look at, then one row
// per analysis with its result, and under it what needs attention.
"use strict";

const vscode = require("vscode");
const model = require("./model");

const { Collapsed, None } = vscode.TreeItemCollapsibleState;

// How a row's kind looks: a codicon, and a theme colour where the row is a verdict.
const ICONS = {
  fail: ["error", "list.errorForeground"],
  warn: ["warning", "list.warningForeground"],
  new: ["diff-added"],
  worse: ["arrow-up"],
  better: ["arrow-down", "testing.iconPassed"],
  group: ["files"],
  copy: ["file-code"],
  mutant: ["bug"],
};

class Side {
  constructor(runs) {
    this.runs = runs;
    this.scope = { type: "scope" };
    this.nodes = Object.fromEntries(Object.keys(model.KINDS).map(kind => [kind, { type: "kind", kind }]));
    this.changed = new vscode.EventEmitter();
    this.onDidChangeTreeData = this.changed.event;
    runs.changed.event(kind => this.changed.fire(kind ? this.nodes[kind] : undefined));
  }

  getChildren(node) {
    if (!node) return [this.scope, ...Object.values(this.nodes)];
    if (node.type === "kind") return this.runs.state(node.kind).items;
    return node.children || [];
  }

  /* Only the analyses are revealed, and they have no row above them. */
  getParent() {
    return undefined;
  }

  getTreeItem(node) {
    return (ROWS[node.type] || finding)(node, this.runs);
  }
}

function scopeRow(node, runs) {
  const item = new vscode.TreeItem("Scope");
  item.description = runs.scopeText();
  item.tooltip = "What the analyses look at. Click to choose files or folders.";
  item.iconPath = new vscode.ThemeIcon("folder-opened");
  item.command = { command: "dogFood.chooseScope", title: "Choose" };
  return item;
}

function kindRow(node, runs) {
  const kind = model.KINDS[node.kind];
  const state = runs.state(node.kind);
  const item = new vscode.TreeItem(kind.title, state.items.length ? Collapsed : None);
  item.description = status(state);
  item.tooltip = kind.hint;
  item.iconPath = new vscode.ThemeIcon(state.running ? "sync~spin" : kind.icon);
  item.contextValue = context(node.kind, state);
  item.command = click(node, state);
  return item;
}

function status(state) {
  if (state.running) return state.progress || "running…";
  return state.error || state.summary || "not run yet";
}

/* Which buttons the row gets: see `view/item/context` in package.json. */
function context(kind, state) {
  if (!model.RUNNABLE.includes(kind)) return "dogFood.dashboardOnly";
  return state.running ? "dogFood.running" : "dogFood.idle";
}

/* A click runs an analysis that has nothing to show yet; one with results opens and closes. */
function click(node, state) {
  if (!model.RUNNABLE.includes(node.kind)) return { command: "dogFood.dashboard", title: "Dashboard", arguments: [node] };
  if (state.running || state.report) return undefined;
  return { command: "dogFood.run", title: "Run", arguments: [node] };
}

function finding(node) {
  const item = new vscode.TreeItem(node.label, node.children ? Collapsed : None);
  item.description = node.description;
  item.tooltip = node.tooltip;
  item.iconPath = icon(node.icon);
  if (node.file) item.command = { command: "dogFood.open", title: "Open", arguments: [node.file, node.line, node.endLine] };
  return item;
}

function icon(kind) {
  const [name, colour] = ICONS[kind] || ["circle-outline"];
  return new vscode.ThemeIcon(name, colour && new vscode.ThemeColor(colour));
}

const ROWS = { scope: scopeRow, kind: kindRow };

module.exports = { Side };
