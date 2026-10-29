# Reviewing in the dashboard

[Duplication](README.md) › Reviewing in the dashboard

```powershell
dgf dup --format html -o dup.html
```

![The duplication review dashboard: totals, groups by kind, a queue of groups, and the selected group's copies side by side](../images/duplication.png)

One self-contained page that works offline, in light and dark themes. From top to bottom:

- **Totals**: groups not reviewed, duplicated lines, lines a refactor would save, groups marked to fix, and review progress.
- **By kind of copy**: how many groups of each type.
- **Projects**: the projects (nearest `.csproj`, `package.json` or `Cargo.toml`) with the most duplicated lines, and the pairs of projects that share copies. Click one to filter the queue.
- **Queue**: every group, filtered by kind, project, review status, changed lines or text, sorted by lines saved, risk, copies or length.
- **Group**: the copies side by side with the lines that differ marked (pick any two copies to compare), then the template, holes, rules, risk, the C# signature and how to unite them.

## Deciding

Mark each group **To fix**, **Intentional** or **False positive**, with a note. Keys: `j`/`k` move, `f`/`i`/`x` decide, `u` clears. A link to one group is `dup.html#group=<id>`.

Decisions stay in the browser until **Export decisions** saves `dup-decisions.json`. Commit that file and pass it back:

```powershell
dgf dup --decisions dup-decisions.json --format html -o dup.html   # reviewed groups left out
dgf dup --changes --decisions dup-decisions.json --fail-on-copies  # a gate that only sees new copies
```

Groups marked intentional or false positive are left out and counted. Groups to fix stay, marked `review: to-fix` in the agent format.

A group's id comes from its kind and the functions its copies are in. It stays the same while the code moves around inside those functions, so decisions survive later scans.

## In a pull request

```powershell
dgf dup --changes --base origin/main --fail-on-copies
```

The whole repository is scanned, because a new copy is usually a copy of code the branch did not touch. Only groups with a copy in the branch's changed lines are reported, uncommitted edits and new files included. `--fail-on-copies` exits 1 when there is one. Repeated queries are advice and never fail it.
