// Where the code of a function is duplicated, from a dgf duplication report,
// as the notes shown above the function. Plain data, so it runs without VS Code.
"use strict";

const path = require("path");

// Windows names one file in more than one spelling of upper and lower case.
const key = file => (process.platform === "win32" ? file.toLowerCase() : file);
const place = copy => `${path.basename(copy.file)}:${copy.startLine}`;
const named = copy => (copy.function ? `${place(copy)} (${copy.function})` : place(copy));

/* Every copy by its file, with the other copies of its group. `root` is the folder dgf ran in. */
function index(report, root) {
  const byFile = new Map();
  for (const group of report.classes) {
    const copies = group.copies.map(copy => ({ file: path.resolve(root, copy.file), startLine: copy.startLine, endLine: copy.endLine, function: copy.function }));
    for (const copy of copies) {
      const entry = { copy, others: copies.filter(other => other !== copy), unite: group.unite || "" };
      byFile.set(key(copy.file), [...(byFile.get(key(copy.file)) || []), entry]);
    }
  }
  return byFile;
}

/* The notes for one file: each on the first line of the function its copy is in. */
function notes(byFile, file, functions) {
  const entries = byFile.get(key(path.resolve(file))) || [];
  return entries.map(entry => ({
    line: (home(entry.copy, functions) || entry.copy).startLine,
    title: title(entry.others),
    tooltip: entry.unite,
    others: entry.others,
  }));
}

/* The function a copy belongs to: the innermost one around its first line, else the first one it reaches. */
function home(copy, functions) {
  const around = functions.filter(f => f.startLine <= copy.startLine && copy.startLine <= f.endLine);
  if (around.length) return around.reduce((a, b) => (b.endLine - b.startLine < a.endLine - a.startLine ? b : a));
  return functions.find(f => f.startLine <= copy.endLine && copy.startLine <= f.endLine);
}

function title(others) {
  if (others.length === 1) return `duplicated in ${named(others[0])}`;
  const shown = others.slice(0, 2).map(place).join(", ");
  return `duplicated in ${others.length} places: ${shown}${others.length > 2 ? ", …" : ""}`;
}

module.exports = { index, notes };
