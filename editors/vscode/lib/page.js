// Fits one of dgf's HTML reports into a VS Code panel: the editor's theme, no
// network access, a bar to switch between the analyses, and a click on a
// location that opens the file. Text in and out, so it runs without VS Code.
"use strict";

// The reports are self-contained, so nothing has to be loaded from anywhere.
const POLICY = `<meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; script-src 'unsafe-inline'; img-src data:; font-src data:">`;

const STYLE = `<style>
#dgf-bar { display: flex; flex-wrap: wrap; gap: 4px; padding: 6px 12px; background: var(--vscode-editorWidget-background); border-bottom: 1px solid var(--vscode-widget-border, transparent); font: var(--vscode-font-size) var(--vscode-font-family); }
#dgf-bar button { padding: 3px 10px; border: 1px solid transparent; border-radius: 3px; background: none; color: var(--vscode-foreground); font: inherit; cursor: pointer; }
#dgf-bar button:hover { background: var(--vscode-toolbar-hoverBackground); }
#dgf-bar button[aria-current] { border-color: var(--vscode-focusBorder); }
#dgf-bar .gap { flex: 1; }
#dgf-message { padding: 24px; font: var(--vscode-font-size) var(--vscode-font-family); color: var(--vscode-foreground); white-space: pre-wrap; }
td.loc, .where { cursor: pointer; }
td.loc:hover, .where:hover { text-decoration: underline; }
</style>`;

// A location reads `path/file.cs:12`, `…:12-20` or `…:12:5`.
const SCRIPT = `<script>
(() => {
  const vscode = acquireVsCodeApi();
  document.addEventListener("click", event => {
    const button = event.target.closest("#dgf-bar button");
    if (button) return vscode.postMessage(JSON.parse(button.dataset.send));
    const place = event.target.closest("td.loc, .where, li > code");
    const found = place && /^(.+?):(\\d+)/.exec(place.textContent.trim());
    if (found) vscode.postMessage({ type: "open", file: found[1], line: Number(found[2]) });
  });
  window.addEventListener("load", () => vscode.postMessage({ type: "ready", title: document.title, rows: document.querySelectorAll("tr").length }));
})();
</script>`;

const escape = text => String(text).replace(/[&<>"]/g, c => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);

function button(label, message, current) {
  return `<button type="button"${current ? ' aria-current="page"' : ""} data-send="${escape(JSON.stringify(message))}">${escape(label)}</button>`;
}

/* The bar above every page: one tab per analysis, then the actions on the shown one. */
function bar(kind, kinds) {
  const tabs = Object.entries(kinds).map(([id, k]) => button(k.title, { type: "show", kind: id }, id === kind));
  const actions = [button("Run again", { type: "run", kind }), button("Open in browser", { type: "browser" })];
  return `<nav id="dgf-bar">${tabs.join("")}<span class="gap"></span>${actions.join("")}</nav>`;
}

/* A dgf report, ready for the panel. */
function prepare(html, { dark, kind, kinds }) {
  return html
    .replace(/<html\b/, `<html data-theme="${dark ? "dark" : "light"}"`)
    .replace(/<head>/, `<head>${POLICY}`)
    .replace(/<body([^>]*)>/, (_, attributes) => `<body${attributes}>${STYLE}${bar(kind, kinds)}${SCRIPT}`);
}

/* A page for when there is no report: dgf's own words on why. */
function message(text, options) {
  const page = `<!doctype html><html lang="en"><head><meta charset="utf-8"><title>dog-food</title></head><body><div id="dgf-message">${escape(text)}</div></body></html>`;
  return prepare(page, options);
}

module.exports = { prepare, message };
