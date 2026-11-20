// In the editor: scores above each function and where its code is duplicated,
// problems for functions above the limits in dgf.toml, and the most complex
// function of the file in the status bar.
"use strict";

const vscode = require("vscode");
const path = require("path");
const dgf = require("./dgf");
const { setting, program } = require("./settings");
const { Duplicates } = require("./duplicates");

const LANGUAGES = ["csharp", "typescript", "typescriptreact", "javascript", "javascriptreact", "rust"];
const SOURCES = "**/*.{cs,ts,tsx,js,jsx,mjs,cjs,rs}";
// Builds write source files too; they are not what anyone edits.
const BUILD_OUTPUT = /[/](bin|obj|target|node_modules|[.]git)[/]/;
const results = new Map();
const lensChanged = new vscode.EventEmitter();
let duplicates;
let diagnostics;
let statusItem;
let warnedMissing = false;

function activate(context, output) {
  duplicates = new Duplicates(() => lensChanged.fire(), line => output.appendLine(line));
  diagnostics = vscode.languages.createDiagnosticCollection("dog-food");
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 90);
  statusItem.command = "dogFood.dashboard";
  const selector = LANGUAGES.map(language => ({ language, scheme: "file" }));
  const watcher = vscode.workspace.createFileSystemWatcher(SOURCES);
  context.subscriptions.push(
    diagnostics,
    statusItem,
    duplicates,
    vscode.languages.registerCodeLensProvider(selector, { onDidChangeCodeLenses: lensChanged.event, provideCodeLenses: lenses }),
    vscode.workspace.onDidSaveTextDocument(analyze),
    watcher,
    watcher.onDidCreate(sourceChanged),
    watcher.onDidChange(sourceChanged),
    watcher.onDidDelete(sourceChanged),
    vscode.workspace.onDidOpenTextDocument(analyze),
    vscode.window.onDidChangeActiveTextEditor(editor => showStatus(editor && editor.document)),
    vscode.commands.registerCommand("dogFood.analyzeFile", () => analyzeActive()),
    vscode.commands.registerCommand("dogFood.showCopies", showCopies)
  );
  vscode.workspace.textDocuments.forEach(analyze);
}

const watched = document => LANGUAGES.includes(document.languageId) && document.uri.scheme === "file";

/* dgf.toml is found from the folder dgf runs in, so run it in the workspace folder. */
function folderOf(document) {
  const folder = vscode.workspace.getWorkspaceFolder(document.uri);
  return folder ? folder.uri.fsPath : path.dirname(document.uri.fsPath);
}

async function analyze(document) {
  if (!watched(document)) return;
  lookForCopies(document);
  try {
    const run = dgf.start(program(), ["analyze", document.uri.fsPath, "--format", "json"], { cwd: folderOf(document) });
    store(document, dgf.report(await run.done).functions);
  } catch (error) {
    reportError(error);
  }
}

/* Copies are looked for in the whole workspace folder, the first time a file of it is opened. */
function lookForCopies(document) {
  const folder = vscode.workspace.getWorkspaceFolder(document.uri);
  if (folder && setting("duplicationLens")) duplicates.ensure(folder.uri.fsPath);
}

/* And again when a source file changes on disk: a save, a checkout, or a tool that writes files. */
function sourceChanged(uri) {
  const folder = vscode.workspace.getWorkspaceFolder(uri);
  if (!folder || BUILD_OUTPUT.test(uri.path) || !setting("duplicationLens")) return;
  duplicates.refresh(folder.uri.fsPath);
}

/* The command: this file's scores again, and its folder's copies. */
function analyzeActive() {
  const editor = vscode.window.activeTextEditor;
  if (!editor) return;
  analyze(editor.document);
  sourceChanged(editor.document.uri);
}

function store(document, functions) {
  results.set(document.uri.toString(), functions);
  diagnostics.set(document.uri, functions.filter(f => f.status === "warn" || f.status === "fail").map(diagnostic));
  lensChanged.fire();
  showStatus(vscode.window.activeTextEditor && vscode.window.activeTextEditor.document);
}

function diagnostic(f) {
  const fail = f.status === "fail";
  const limit = fail ? f.limits.fail : f.limits.warn;
  const message = `${f.name}: cyclomatic complexity ${f.complexity} is above the ${fail ? "fail" : "warn"} limit ${limit} (cognitive ${f.cognitive}). ${decisions(f)}`;
  const range = new vscode.Range(f.startLine - 1, 0, f.startLine - 1, 200);
  const item = new vscode.Diagnostic(range, message, fail ? vscode.DiagnosticSeverity.Error : vscode.DiagnosticSeverity.Warning);
  item.source = "dog-food";
  return item;
}

function decisions(f) {
  const parts = Object.entries(f.breakdown).map(([kind, count]) => `${kind} ×${count}`);
  return parts.length ? `Decisions: ${parts.join(", ")}.` : "";
}

function lenses(document) {
  if (!setting("codeLens")) return [];
  const minimum = setting("codeLensMinimum");
  const functions = results.get(document.uri.toString()) || [];
  return [...functions.filter(f => f.complexity >= minimum).map(lens), ...copyLenses(document, functions)];
}

function copyLenses(document, functions) {
  if (!setting("duplicationLens")) return [];
  return duplicates.notes(document, functions).map(note => {
    const range = new vscode.Range(note.line - 1, 0, note.line - 1, 0);
    return new vscode.CodeLens(range, { title: note.title, tooltip: note.tooltip, command: "dogFood.showCopies", arguments: [note.others] });
  });
}

/* Opens the other copy, or asks which one when there are several. */
async function showCopies(others) {
  const copy = others.length === 1 ? others[0] : await chooseCopy(others);
  if (copy) await vscode.commands.executeCommand("dogFood.open", copy.file, copy.startLine, copy.endLine);
}

async function chooseCopy(others) {
  const items = others.map(copy => ({
    label: `${path.basename(copy.file)}:${copy.startLine}-${copy.endLine}`,
    description: copy.function || "",
    detail: vscode.workspace.asRelativePath(copy.file),
    copy,
  }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder: "Which copy?" });
  return picked && picked.copy;
}

function lens(f) {
  const marks = { fail: " · FAIL", warn: " · warn", ignored: " · ignored" };
  const coverage = f.coverage == null ? "" : ` · ${Math.round(f.coverage * 100)}% covered · CRAP ${f.crap.toFixed(1)}`;
  const title = `complexity ${f.complexity} · cognitive ${f.cognitive}${coverage}${marks[f.status] || ""}`;
  const range = new vscode.Range(f.startLine - 1, 0, f.startLine - 1, 0);
  return new vscode.CodeLens(range, { title, tooltip: decisions(f) || "No branches", command: "" });
}

/* The most complex function in the open file, in the status bar. */
function showStatus(document) {
  const functions = document ? results.get(document.uri.toString()) : null;
  if (!functions || !functions.length) {
    statusItem.hide();
    return;
  }
  const worst = functions.reduce((a, b) => (b.complexity > a.complexity ? b : a));
  const icon = { fail: "$(error)", warn: "$(warning)" }[worst.status] || "$(pulse)";
  statusItem.text = `${icon} dgf ${worst.complexity}`;
  statusItem.tooltip = `Most complex: ${worst.name} (${worst.complexity}). Click for the dashboard.`;
  statusItem.show();
}

function reportError(error) {
  if (error.code === "ENOENT") {
    if (!warnedMissing) vscode.window.showWarningMessage("dog-food: `dgf` was not found. Install it, or set dogFood.path.");
    warnedMissing = true;
    return;
  }
  console.error(`dog-food: ${error.message}`);
}

module.exports = { activate };
