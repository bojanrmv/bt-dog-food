"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const model = require("../lib/model");

const limits = { warn: 5, fail: 10 };
const fn = (name, complexity, status, more = {}) => ({
  name, complexity, status, limits, cognitive: complexity, lines: 20, breakdown: { if: complexity - 1 },
  file: "src/Shop/Pricing.cs", startLine: 5, endLine: 24, ...more,
});

test("each analysis is its dgf command over the chosen paths", () => {
  assert.deepEqual(model.command("complexity", [], "json"), ["analyze", "--format", "json"]);
  assert.deepEqual(model.command("complexity", ["src", "a.cs"], "json"), ["analyze", "src", "a.cs", "--format", "json"]);
  assert.deepEqual(model.command("duplication", ["src"], "html", { output: "d.html" }), ["duplication", "src", "--format", "html", "-o", "d.html"]);
});

test("changes and history are of the whole branch, whatever was chosen", () => {
  assert.deepEqual(model.command("changes", ["src"], "json"), ["changes", "--format", "json"]);
  assert.deepEqual(model.command("history", ["src"], "html"), ["history", "--format", "html"]);
});

test("mutation tests the changed functions unless paths were chosen", () => {
  assert.deepEqual(model.command("mutation", [], "json"), ["mutate", "--changes", "--run", "--format", "json"]);
  assert.deepEqual(model.command("mutation", ["src/Calc"], "json"), ["mutate", "src/Calc", "--run", "--format", "json"]);
});

test("a mutation page is drawn from the last run, not by testing again", () => {
  const args = model.command("mutation", [], "html", { output: "m.html", cached: true });
  assert.deepEqual(args, ["mutate", "--changes", "--run", "--format", "html", "--cached", "-o", "m.html"]);
  assert.ok(!model.command("complexity", [], "html", { cached: true }).includes("--cached"));
});

test("complexity lists the functions above the limits, the failing ones first", () => {
  const report = {
    summary: { fail: 1, warn: 1, functions: 3 },
    functions: [fn("Fine", 2, "ok"), fn("Warned", 7, "warn"), fn("Failed", 12, "fail", { coverage: 0.5, crap: 30.25 })],
  };
  const rows = model.findings("complexity", report);
  assert.deepEqual(rows.map(row => row.label), ["Failed", "Warned"]);
  assert.equal(rows[0].description, "12 · Pricing.cs:5");
  assert.equal(rows[0].icon, "fail");
  assert.deepEqual([rows[0].file, rows[0].line], ["src/Shop/Pricing.cs", 5]);
  assert.match(rows[0].tooltip, /50% covered · CRAP 30\.3/);
  assert.equal(model.summary("complexity", report), "1 fail · 1 warn · 3 functions");
});

test("with nothing above the limits, complexity lists the most complex functions", () => {
  const report = { summary: { fail: 0, warn: 0, functions: 2 }, functions: [fn("Small", 1, "ok"), fn("Larger", 4, "ok")] };
  assert.deepEqual(model.findings("complexity", report).map(row => row.label), ["Larger", "Small"]);
  assert.equal(model.summary("complexity", report), "2 functions within the limits");
});

test("changes lists what is new or moved, and leaves out what stayed or went", () => {
  const change = (name, kind, before, after, status) => ({ name, change: kind, before, after, status, breakdown: {}, file: "src/a.ts", startLine: 3 });
  const report = {
    base: "main", verdict: "fail",
    summary: { functions: 4, worse: 1, new: 1, better: 0 },
    functions: [change("same", "same", 3, 3, "ok"), change("gone", "removed", 4, null, "ok"), change("added", "new", null, 2, "ok"), change("grew", "worse", 4, 12, "fail")],
  };
  const rows = model.findings("changes", report);
  assert.deepEqual(rows.map(row => row.description), ["worse 4 → 12 · a.ts:3", "new 2 · a.ts:3"]);
  assert.deepEqual(rows.map(row => row.icon), ["fail", "new"]);
  assert.equal(model.summary("changes", report), "fail · 1 worse · 1 new · 0 better · since main");
});

test("a branch that changed nothing says so", () => {
  const report = { base: "main", verdict: "pass", summary: { functions: 0 }, functions: [] };
  assert.equal(model.summary("changes", report), "nothing changed since main");
});

test("duplication lists each group with its copies under it", () => {
  const copy = (file, startLine, name) => ({ file, startLine, endLine: startLine + 7, function: name });
  const report = {
    files: 4, lines: 400, duplicatedLines: 16,
    classes: [{ lines: 8, unite: "move it to a shared helper", cautions: ["one copy logs"], copies: [copy("src/a.cs", 10, "Load"), copy("src/b.cs", 40, null)] }],
  };
  const [group] = model.findings("duplication", report);
  assert.equal(group.label, "2 copies · 8 lines");
  assert.equal(group.description, "Load, b.cs");
  assert.equal(group.tooltip, "move it to a shared helper\none copy logs");
  assert.deepEqual(group.children.map(row => [row.label, row.file, row.line, row.endLine]), [["a.cs:10-17", "src/a.cs", 10, 17], ["b.cs:40-47", "src/b.cs", 40, 47]]);
  assert.equal(model.summary("duplication", report), "1 group · 16 duplicated lines (4.0%)");
});

test("no copies is said with the number of files looked at", () => {
  assert.equal(model.summary("duplication", { files: 1, lines: 10, duplicatedLines: 0, classes: [] }), "no copies in 1 file");
});

const mutant = (line, outcome, original, replacement) => ({ outcome, mutant: { line, column: 9, mutator: "negate", original, replacement } });
const tally = (killed, survived, noCoverage = 0, untested = 0) => ({ killed, survived, noCoverage, untested });

test("mutation lists the functions with mutants no test noticed, the weakest first", () => {
  const tested = (name, score, mutants) => ({ name, score, mutants, tally: tally(1, 1), file: "src/Calc.cs", startLine: 13 });
  const report = {
    score: 60, tally: tally(3, 1, 1), notes: [],
    functions: [
      tested("Solid", 100, [mutant(14, "killed", "a", "b")]),
      tested("Half", 50, [mutant(20, "killed", "a", "b"), mutant(21, "survived", "x > 0", "!(x > 0)")]),
      tested("Unreached", 0, [mutant(30, "noCoverage", "a +\n   b", "a - b")]),
    ],
  };
  const rows = model.findings("mutation", report);
  assert.deepEqual(rows.map(row => row.label), ["Unreached", "Half"]);
  assert.equal(rows[1].description, "50% · 1 missed · Calc.cs:13");
  assert.deepEqual(rows[1].children.map(row => [row.label, row.description, row.line]), [["21: x > 0 → !(x > 0)", "negate · survived", 21]]);
  assert.equal(rows[0].children[0].label, "30: a + b → a - b");
  assert.equal(rows[0].children[0].description, "negate · no test reaches it");
  assert.equal(model.summary("mutation", report), "60% · 1 survived · 3 killed · 1 not reached");
});

test("a mutation run with nothing to test gives dgf's reason", () => {
  const empty = { score: null, tally: tally(0, 0), functions: [] };
  assert.equal(model.summary("mutation", { ...empty, notes: ["no changed functions"] }), "no changed functions");
  assert.equal(model.summary("mutation", empty), "no functions to test");
});

test("every setting has the default package.json gives it", () => {
  const declared = require("../package.json").contributes.configuration.properties;
  const defaults = Object.fromEntries(Object.entries(declared).map(([name, property]) => [name.replace("dogFood.", ""), property.default]));
  assert.deepEqual(require("../lib/defaults"), defaults);
});
