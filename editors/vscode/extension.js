// dog-food for VS Code: a side bar that runs the analyses and lists what they
// find, a dashboard of dgf's report pages, and scores in the editor.
// It runs the `dgf` command line tool; it has no other dependencies.
"use strict";

const vscode = require("vscode");
const path = require("path");
const os = require("os");
const model = require("./lib/model");
const editor = require("./lib/editor");
const { Runs } = require("./lib/runs");
const { Side } = require("./lib/side");
const { Dashboard } = require("./lib/dashboard");

function activate(context) {
  const output = vscode.window.createOutputChannel("Dog Food");
  const runs = new Runs(context, output);
  const side = new Side(runs);
  const view = vscode.window.createTreeView("dogFood.view", { treeDataProvider: side });
  const dashboard = new Dashboard(runs, pagesFolder(context));
  const command = (id, action) => vscode.commands.registerCommand(id, action);

  editor.activate(context, output);
  context.subscriptions.push(
    output,
    view,
    // A finished run opens its row, and redraws its page if that is the one shown.
    runs.finished.event(kind => view.reveal(side.nodes[kind], { expand: true, select: false }).then(undefined, () => {})),
    runs.finished.event(kind => dashboard.refresh(kind)),
    vscode.window.onDidChangeActiveColorTheme(() => dashboard.refresh()),
    command("dogFood.run", async target => run(runs, kindOf(target) || (await pickKind()))),
    command("dogFood.runAll", () => Promise.all(["complexity", "changes", "duplication"].map(kind => runs.run(kind)))),
    command("dogFood.stop", target => runs.stop(kindOf(target))),
    command("dogFood.dashboard", target => dashboard.show(kindOf(target))),
    command("dogFood.chooseScope", () => chooseScope(runs)),
    command("dogFood.open", (file, line, endLine) => open(runs, file, line, endLine)),
    command("dogFood.complexityOf", onSelection(runs, "complexity")),
    command("dogFood.duplicationOf", onSelection(runs, "duplication")),
    command("dogFood.mutationOf", onSelection(runs, "mutation"))
  );
  return { runs, side, dashboard };
}

/* Report pages go to the workspace's own storage, so two windows do not share them. */
function pagesFolder(context) {
  return context.storageUri ? context.storageUri.fsPath : path.join(os.tmpdir(), "dgf-vscode");
}

/* An analysis as a command gets it: its name, or its row in the side bar. */
function kindOf(target) {
  return typeof target === "string" ? target : target && target.kind;
}

function run(runs, kind) {
  return kind ? runs.run(kind) : undefined;
}

async function pickKind() {
  const items = model.RUNNABLE.map(kind => ({ label: model.KINDS[kind].title, detail: model.KINDS[kind].hint, kind }));
  const picked = await vscode.window.showQuickPick(items, { placeHolder: "Which analysis?" });
  return picked && picked.kind;
}

async function chooseScope(runs) {
  const choices = [
    { label: "Whole workspace", paths: () => [] },
    { label: "The open file", paths: openFile },
    { label: "Choose folders…", paths: () => browse(runs, true) },
    { label: "Choose files…", paths: () => browse(runs, false) },
  ];
  const choice = await vscode.window.showQuickPick(choices, { placeHolder: "What should the analyses look at?" });
  const paths = choice && (await choice.paths());
  if (paths) runs.setScope(paths);
}

function openFile() {
  const uri = activeFile();
  return uri && [uri.fsPath];
}

function activeFile() {
  const active = vscode.window.activeTextEditor;
  return active && active.document.uri.scheme === "file" ? active.document.uri : undefined;
}

/* A dialog picks files or folders, not both: Windows and Linux have no dialog for both. */
async function browse(runs, folders) {
  const folder = runs.folder();
  const picked = await vscode.window.showOpenDialog({
    canSelectFolders: folders,
    canSelectFiles: !folders,
    canSelectMany: true,
    defaultUri: folder ? vscode.Uri.file(folder) : undefined,
    openLabel: "Analyze",
  });
  return picked && picked.map(uri => uri.fsPath);
}

/* From the explorer's menu: the selected files and folders become what is looked at, and one analysis runs. */
function onSelection(runs, kind) {
  return async (uri, uris) => {
    const chosen = selected(uri, uris);
    if (!chosen.length) return;
    runs.setScope(chosen);
    await vscode.commands.executeCommand("dogFood.view.focus");
    await runs.run(kind);
  };
}

function selected(uri, uris) {
  const picked = uris && uris.length ? uris : [uri || activeFile()];
  return picked.filter(each => each && each.scheme === "file").map(each => each.fsPath);
}

async function open(runs, file, line = 1, endLine) {
  const found = runs.locate(file);
  if (!found) return void vscode.window.showWarningMessage(`Dog Food: ${file} was not found.`);
  const first = Math.max(line, 1) - 1;
  // A copy is selected whole, up to the start of the line after it.
  const selection = new vscode.Range(first, 0, endLine || first, 0);
  await vscode.window.showTextDocument(vscode.Uri.file(found), { selection });
}

function deactivate() {}

module.exports = { activate, deactivate };
