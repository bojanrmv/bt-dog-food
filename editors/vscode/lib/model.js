// What each analysis is: its dgf command line, and its report as the rows
// the side bar shows. Plain data in and out, so it runs without VS Code.
"use strict";

const KINDS = {
  complexity: { title: "Complexity", command: "analyze", icon: "pulse", hint: "Cyclomatic and cognitive complexity of every function, and CRAP when dgf.toml lists coverage reports." },
  changes: { title: "Changes", command: "changes", icon: "git-compare", hint: "What this branch and its uncommitted edits did to the scores. Always the whole branch." },
  duplication: { title: "Duplication", command: "duplication", icon: "copy", hint: "Copied code that can be united." },
  mutation: { title: "Mutation tests", command: "mutate", icon: "beaker", hint: "Would the tests notice a bug? For the whole workspace, only the functions this branch changed are tested." },
  history: { title: "History", command: "history", icon: "graph-line", hint: "The trend of the runs saved with `dgf analyze --save-history`." },
};

/* The analyses that have findings to list; history is a dashboard only. */
const RUNNABLE = ["complexity", "changes", "duplication", "mutation"];
const MOST = 200;
const RANK = { fail: 0, warn: 1 };

/* The dgf arguments of one analysis. `paths` are the chosen files and folders: none is the whole workspace. */
function command(kind, paths, format, options = {}) {
  const args = [KINDS[kind].command, ...scope(kind, paths), "--format", format];
  if (options.cached && kind === "mutation") args.push("--cached");
  if (options.output) args.push("-o", options.output);
  return args;
}

function scope(kind, paths) {
  if (kind === "changes" || kind === "history") return [];
  if (kind !== "mutation") return paths;
  return paths.length ? [...paths, "--run"] : ["--changes", "--run"];
}

/* One line that says how the run went. */
function summary(kind, report) {
  return SUMMARIES[kind](report);
}

/* The rows under an analysis: what needs attention first. */
function findings(kind, report) {
  return FINDINGS[kind](report);
}

const count = (n, one, many) => `${n} ${n === 1 ? one : many}`;
const rank = f => (f.status in RANK ? RANK[f.status] : 2);
const baseName = file => file.slice(file.lastIndexOf("/") + 1);
const oneLine = text => {
  const flat = String(text).replace(/\s+/g, " ").trim();
  return flat.length > 70 ? `${flat.slice(0, 69)}…` : flat;
};

function complexitySummary(report) {
  const s = report.summary;
  if (!s.fail && !s.warn) return `${count(s.functions, "function", "functions")} within the limits`;
  return `${s.fail} fail · ${s.warn} warn · ${count(s.functions, "function", "functions")}`;
}

/* Functions above the limits, or the ten most complex when none is. */
function complexityFindings(report) {
  const sorted = [...report.functions].sort((a, b) => rank(a) - rank(b) || b.complexity - a.complexity);
  const above = sorted.filter(f => f.status in RANK);
  return (above.length ? above.slice(0, MOST) : sorted.slice(0, 10)).map(complexityRow);
}

function complexityRow(f) {
  return {
    label: f.name,
    description: `${f.complexity} · ${baseName(f.file)}:${f.startLine}`,
    tooltip: [scores(f), covered(f), decisions(f.breakdown), `${f.file}:${f.startLine}`].filter(Boolean).join("\n"),
    icon: f.status,
    file: f.file,
    line: f.startLine,
  };
}

const scores = f => `cyclomatic ${f.complexity} (warn above ${f.limits.warn}, fail above ${f.limits.fail}) · cognitive ${f.cognitive} · ${f.lines} lines`;
const covered = f => (f.coverage == null ? "" : `${Math.round(f.coverage * 100)}% covered · CRAP ${f.crap.toFixed(1)}`);

function decisions(breakdown) {
  const parts = Object.entries(breakdown || {}).map(([kind, n]) => `${kind} ×${n}`);
  return parts.length ? `decisions: ${parts.join(", ")}` : "";
}

function changesSummary(report) {
  const s = report.summary;
  if (!s.functions) return `nothing changed since ${report.base}`;
  return `${report.verdict} · ${s.worse} worse · ${s.new} new · ${s.better} better · since ${report.base}`;
}

function changesFindings(report) {
  return report.functions
    .filter(f => f.change !== "same" && f.change !== "removed")
    .sort((a, b) => rank(a) - rank(b) || b.after - a.after)
    .slice(0, MOST)
    .map(changesRow);
}

function changesRow(f) {
  const moved = f.before == null ? `${f.after}` : `${f.before} → ${f.after}`;
  return {
    label: f.name,
    description: `${f.change} ${moved} · ${baseName(f.file)}:${f.startLine}`,
    tooltip: [`${f.change}: ${moved}`, decisions(f.breakdown), `${f.file}:${f.startLine}`].filter(Boolean).join("\n"),
    icon: f.status in RANK ? f.status : f.change,
    file: f.file,
    line: f.startLine,
  };
}

function duplicationSummary(report) {
  if (!report.classes.length) return `no copies in ${count(report.files, "file", "files")}`;
  const share = report.lines ? ` (${((100 * report.duplicatedLines) / report.lines).toFixed(1)}%)` : "";
  return `${count(report.classes.length, "group", "groups")} · ${report.duplicatedLines} duplicated lines${share}`;
}

function duplicationFindings(report) {
  return report.classes.slice(0, MOST).map(group);
}

function group(c) {
  const names = [...new Set(c.copies.map(copy => copy.function || baseName(copy.file)))];
  return {
    label: `${c.copies.length} copies · ${count(c.lines, "line", "lines")}`,
    description: names.length > 3 ? `${names.slice(0, 3).join(", ")} +${names.length - 3}` : names.join(", "),
    tooltip: [c.unite, ...(c.cautions || [])].filter(Boolean).join("\n"),
    icon: "group",
    children: c.copies.map(copyRow),
  };
}

function copyRow(copy) {
  return {
    label: `${baseName(copy.file)}:${copy.startLine}-${copy.endLine}`,
    description: copy.function || "",
    tooltip: `${copy.file}:${copy.startLine}-${copy.endLine}`,
    icon: "copy",
    file: copy.file,
    line: copy.startLine,
    endLine: copy.endLine,
  };
}

const MISSED = { survived: "survived", noCoverage: "no test reaches it" };
const percent = score => (score == null ? "no score" : `${Math.round(score)}%`);

function mutationSummary(report) {
  const t = report.tally;
  if (!report.functions.length) return nothingTested(report);
  const rare = [[t.noCoverage, "not reached"], [t.untested, "untested"]].filter(([n]) => n).map(([n, what]) => `${n} ${what}`);
  return [percent(report.score), `${t.survived} survived`, `${t.killed} killed`, ...rare].join(" · ");
}

/* dgf's own note on why, such as that the branch changed no function. */
function nothingTested(report) {
  return (report.notes || [])[0] || "no functions to test";
}

/* Functions with mutants no test noticed, the lowest score first. */
function mutationFindings(report) {
  return report.functions
    .map(f => ({ f, missed: f.mutants.filter(m => m.outcome in MISSED) }))
    .filter(entry => entry.missed.length)
    .sort((a, b) => (a.f.score || 0) - (b.f.score || 0))
    .slice(0, MOST)
    .map(mutatedFunction);
}

function mutatedFunction({ f, missed }) {
  return {
    label: f.name,
    description: `${percent(f.score)} · ${missed.length} missed · ${baseName(f.file)}:${f.startLine}`,
    tooltip: `${f.file}:${f.startLine}\n${f.tally.killed} killed, ${f.tally.survived} survived, ${f.tally.noCoverage} not reached`,
    icon: "warn",
    file: f.file,
    line: f.startLine,
    children: missed.map(m => mutantRow(f, m)),
  };
}

function mutantRow(f, m) {
  const change = `${oneLine(m.mutant.original)} → ${oneLine(m.mutant.replacement)}`;
  return {
    label: `${m.mutant.line}: ${change}`,
    description: `${m.mutant.mutator} · ${MISSED[m.outcome]}`,
    tooltip: `${change}\n${f.file}:${m.mutant.line}:${m.mutant.column}`,
    icon: "mutant",
    file: f.file,
    line: m.mutant.line,
  };
}

const SUMMARIES = { complexity: complexitySummary, changes: changesSummary, duplication: duplicationSummary, mutation: mutationSummary };
const FINDINGS = { complexity: complexityFindings, changes: changesFindings, duplication: duplicationFindings, mutation: mutationFindings };

module.exports = { KINDS, RUNNABLE, command, summary, findings };
