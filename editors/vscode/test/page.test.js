"use strict";

const test = require("node:test");
const assert = require("node:assert/strict");
const page = require("../lib/page");
const { KINDS } = require("../lib/model");

const REPORT = `<!doctype html>\n<html lang="en">\n<head>\n<title>report</title>\n</head>\n<body class="wide">\n<main>scores</main>\n<script>draw()</script>\n</body>\n</html>`;
const options = { dark: true, kind: "duplication", kinds: KINDS };

test("a report gets the editor's theme", () => {
  assert.match(page.prepare(REPORT, options), /<html data-theme="dark" lang="en">/);
  assert.match(page.prepare(REPORT, { ...options, dark: false }), /<html data-theme="light" lang="en">/);
});

test("a report may load nothing from anywhere", () => {
  assert.match(page.prepare(REPORT, options), /<head><meta http-equiv="Content-Security-Policy" content="default-src 'none';/);
});

test("the bar comes before the report, which stays whole", () => {
  const html = page.prepare(REPORT, options);
  assert.ok(html.indexOf('<body class="wide">') < html.indexOf('<nav id="dgf-bar">'));
  assert.ok(html.indexOf('<nav id="dgf-bar">') < html.indexOf("<main>scores</main>"));
  assert.ok(html.endsWith("<main>scores</main>\n<script>draw()</script>\n</body>\n</html>"));
});

test("the bar has a tab per analysis and marks the one shown", () => {
  const html = page.prepare(REPORT, options);
  for (const kind of Object.keys(KINDS)) assert.ok(html.includes(`data-send="{&quot;type&quot;:&quot;show&quot;,&quot;kind&quot;:&quot;${kind}&quot;}"`), kind);
  assert.equal(html.match(/aria-current/g).length, 2); // the style rule and one tab
  assert.match(html, /<button type="button" aria-current="page" data-send="[^"]*duplication[^"]*">Duplication<\/button>/);
  assert.match(html, /data-send="\{&quot;type&quot;:&quot;run&quot;,&quot;kind&quot;:&quot;duplication&quot;\}">Run again</);
});

test("a message page shows dgf's words as text, not as markup", () => {
  const html = page.message("no runs saved: <use> `dgf analyze --save-history` & retry", options);
  assert.match(html, /<div id="dgf-message">no runs saved: &lt;use&gt; `dgf analyze --save-history` &amp; retry<\/div>/);
  assert.match(html, /<nav id="dgf-bar">/);
});
