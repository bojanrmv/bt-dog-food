// dog-food for VS Code: scores above each function, problems for functions
// above the limits in dgf.toml, and commands that open the HTML reports.
// It runs the `dgf` command line tool; it has no other dependencies.
"use strict";

const vscode = require("vscode");
const { execFile } = require("child_process");
const os = require("os");
const path = require("path");

const LANGUAGES = ["csharp", "typescript", "typescriptreact", "javascript", "javascriptreact", "rust"];
const results = new Map();
const lensChanged = new vscode.EventEmitter();
let diagnostics;
let statusItem;
let warnedMissing = false;

function activate(context) {
  diagnostics = vscode.languages.createDiagnosticCollection("dog-food");
  statusItem = vscode.window.createStatusBarItem(vscode.StatusBarAlignment.Right, 90);
  statusItem.command = "dogFood.openReport";
  const selector = LANGUAGES.map(language => ({ language, scheme: "file" }));
  context.subscriptions.push(
    diagnostics,
    statusItem,
    vscode.languages.registerCodeLensProvider(selector, { onDidChangeCodeLenses: lensChanged.event, provideCodeLenses: lenses }),
    vscode.workspace.onDidSaveTextDocument(analyze),
    vscode.workspace.onDidOpenTextDocument(analyze),
    vscode.window.onDidChangeActiveTextEditor(editor => showStatus(editor && editor.document)),
    vscode.commands.registerCommand("dogFood.analyzeFile", () => analyzeActive()),
    vscode.commands.registerCommand("dogFood.openReport", () => openReport("analyze", "dgf-report.html")),
    vscode.commands.registerCommand("dogFood.openChanges", () => openReport("changes", "dgf-changes.html")),
    vscode.commands.registerCommand("dogFood.openHistory", () => openReport("history", "dgf-history.html"))
  );
  vscode.workspace.textDocuments.forEach(analyze);
}

function settings() {
  return vscode.workspace.getConfiguration("dogFood");
}

/* dgf.toml is found from the folder dgf runs in, so run it in the workspace folder. */
function folderOf(document) {
  const folder = vscode.workspace.getWorkspaceFolder(document.uri);
  return folder ? folder.uri.fsPath : path.dirname(document.uri.fsPath);
}

function run(args, cwd) {
  return new Promise((resolve, reject) => {
    execFile(settings().get("path") || "dgf", args, { cwd, maxBuffer: 64 * 1024 * 1024 }, (error, stdout, stderr) => {
      // Exit code 1 only means a function is above the fail limit; the output is complete.
      if (error && error.code !== 1) reject(Object.assign(new Error(stderr || error.message), { code: error.code }));
      else resolve(stdout);
    });
  });
}

async function analyze(document) {
  if (!LANGUAGES.includes(document.languageId) || document.uri.scheme !== "file") return;
  try {
    const output = await run(["analyze", document.uri.fsPath, "--format", "json"], folderOf(document));
    store(document, JSON.parse(output).functions);
  } catch (error) {
    reportError(error);
  }
}

function analyzeActive() {
  const editor = vscode.window.activeTextEditor;
  if (editor) analyze(editor.document);
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
  if (!settings().get("codeLens")) return [];
  const minimum = settings().get("codeLensMinimum") || 1;
  const functions = results.get(document.uri.toString()) || [];
  return functions.filter(f => f.complexity >= minimum).map(lens);
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
  statusItem.tooltip = `Most complex: ${worst.name} (${worst.complexity}). Click for the workspace report.`;
  statusItem.show();
}

async function openReport(command, file) {
  const folder = (vscode.workspace.workspaceFolders || [])[0];
  if (!folder) return vscode.window.showWarningMessage("Open a folder to build a dog-food report.");
  const output = path.join(os.tmpdir(), file);
  try {
    await run([command, "--format", "html", "-o", output], folder.uri.fsPath);
    await vscode.env.openExternal(vscode.Uri.file(output));
  } catch (error) {
    vscode.window.showErrorMessage(`dog-food: ${error.message.trim()}`);
  }
}

function reportError(error) {
  if (error.code === "ENOENT") {
    if (!warnedMissing) vscode.window.showWarningMessage("dog-food: `dgf` was not found. Install it, or set dogFood.path.");
    warnedMissing = true;
    return;
  }
  console.error(`dog-food: ${error.message}`);
}

function deactivate() {}

module.exports = { activate, deactivate };
