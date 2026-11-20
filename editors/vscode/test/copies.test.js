"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const path = require("node:path");
const copies = require("../lib/copies");

const root = path.resolve("repo");
const at = file => path.join(root, file);
const copy = (file, startLine, endLine, name) => ({ file, startLine, endLine, function: name, code: "…" });
const fn = (name, startLine, endLine) => ({ name, startLine, endLine });

const report = {
  classes: [
    { unite: "move it to a shared helper", copies: [copy("src/a.rs", 12, 20, "load"), copy("src/b.rs", 40, 48, "read")] },
    { copies: [copy("src/a.rs", 31, 36, "save"), copy("src/b.rs", 5, 10, null), copy("src/c.rs", 7, 12, "keep"), copy("src/d.rs", 1, 6, "hold")] },
  ],
};
const byFile = copies.index(report, root);

test("a function is told where its code is duplicated in another file", () => {
  const [note] = copies.notes(byFile, at("src/a.rs"), [fn("load", 10, 22), fn("save", 30, 40)]);
  assert.equal(note.line, 10);
  assert.equal(note.title, "duplicated in b.rs:40 (read)");
  assert.equal(note.tooltip, "move it to a shared helper");
  assert.deepEqual(note.others, [{ file: at("src/b.rs"), startLine: 40, endLine: 48, function: "read" }]);
});

test("several copies are counted, and the first two named", () => {
  const [, note] = copies.notes(byFile, at("src/a.rs"), [fn("load", 10, 22), fn("save", 30, 40)]);
  assert.equal(note.line, 30);
  assert.equal(note.title, "duplicated in 3 places: b.rs:5, c.rs:7, …");
  assert.equal(note.others.length, 3);
});

test("the other side of a group points back", () => {
  const titles = copies.notes(byFile, at("src/b.rs"), [fn("read", 38, 50)]).map(note => note.title);
  assert.deepEqual(titles, ["duplicated in a.rs:12 (load)", "duplicated in 3 places: a.rs:31, c.rs:7, …"]);
});

test("a copy is noted on the innermost function around it", () => {
  const [note] = copies.notes(byFile, at("src/a.rs"), [fn("outer", 1, 60), fn("outer/inner", 11, 25)]);
  assert.equal(note.line, 11);
});

test("a copy that starts before a function is noted on the function it reaches", () => {
  const [note] = copies.notes(byFile, at("src/a.rs"), [fn("load", 14, 22)]);
  assert.equal(note.line, 14);
});

test("a copy outside every function is noted where it starts", () => {
  const [note] = copies.notes(byFile, at("src/a.rs"), [fn("far", 100, 120)]);
  assert.equal(note.line, 12);
});

test("a file without copies has no notes", () => {
  assert.deepEqual(copies.notes(byFile, at("src/z.rs"), [fn("alone", 1, 9)]), []);
});
