# Roadmap

dog-food is built for teams where coding agents write much of the code. Agents read `dgf`'s output and act on it. This page lists what comes next, in order of value for that loop.

Value is the expected gain for teams using dgf. Effort is S (a day or two), M (a week) or L (several weeks). Each idea names where its evidence comes from; where the evidence is weak, it says so.

## Mutation testing: what comes next

| What | Value | Effort | Notes |
| --- | --- | --- | --- |
| Mutate outside functions | high | M | Only code inside functions is mutated, so module-level constants and tables, property defaults and base-constructor arguments go untested. On a real repository these held most of the gaps an independent run found outside dgf's reach, including an error-code table no test pinned. The core already finds these mutants and the target step drops them, so this is mostly a target per file and a cache key for it. A separate score, so existing scores do not jump unexplained. |
| TypeScript kills that another test caused | high | S–M | A test can fail from what an earlier test in its file left behind, even though its own mutant took effect. Confirm kills in a second run where no neighbouring test carries a mutant, so a leak meets a clean test first. Measure against `--no-mix`. Target: the same verdicts every run. |
| Fewer TypeScript runs alone | medium | S | The checks that made verdicts reliable raised a 301-mutant run from about 21 test runs to 55, about 90 s to 170 s. Most mutants that never took effect could share a clean second pack before anything runs alone. |
| Why tests fail only sometimes in a baseline | medium | S–M | One block of tests failed together in 2 of 20 baselines. They now get a second run, but the cause is unknown, and a test that fails now and then can still cause a false kill. |
| Predict mutants that cannot compile | medium | S–M | Finding them by building takes up to a third of a C# run (about 80 s of 220 s on one project), at about 2.5 s a round. The commonest are knowable from the source: `required` members left out of an initializer, a `??` that changes a value type, `throw;` removed from a method that must return. |
| Is packing still worth it in C# | medium | M | With warm workers, running every mutant alone took about as long as packing on two of four projects (114 s and 125 s, 436 s and 412 s), and four times the runs cost little. Find what a pack spends its time on. Queueing each survivor's next tests at once, not in rounds, removes the wait for a round's slowest pack. |
| String patterns in `switch` arms | low | M | `"name" => ...` is a constant, so it needs a switch around the whole expression. |
| Clearer progress and defaults | low | S | "6 of 64 mutants come from the cache" also counts ignored mutants, even with `--no-cache`. A progress count can pass the mutant count without saying what the extra runs are. Files only end-to-end tests use could be left out by default. |
| NUnit and MSTest end-to-end samples | high | S | Both run through VSTest like xUnit, and test names are escaped for filters (NUnit puts arguments in names), but only xUnit has an end-to-end test. |
| Linux and macOS in CI | medium | S | The end-to-end tests only run on Windows today. Timeouts use CPU time through Windows job objects; elsewhere they fall back to wall time. |
| Cache notices other production code | medium | M | A cached killed mutant is not run again when a change elsewhere makes its killing test miss it. Record the files each killing test reaches and compare them. |

## AI-assisted engineering

Ranked from the research below. The first five are the next things to build.

| # | What | Value | Effort | One line |
| --- | --- | --- | --- | --- |
| 1 | `dgf mcp` | high | M | An MCP server over stdio: the commands an agent needs as tools, with structured results (design below). |
| 2 | Claude Code stop hook | high | S | `dgf hook install --claude` writes a Stop hook that runs `dgf changes --format agent` and blocks finishing while a new or worse function is above the limit. A hook that exits 2 sends its message back to the agent. |
| 3 | Survivors as work items | high | S | Each survivor with a stable id, the code before and after, the test file to extend and a hint; `dgf mutate --only <ids>` re-runs just those mutants, so "write a test, check it" takes seconds. |
| 4 | Pseudo-tested functions | high | S–M | Before full mutation, replace each chosen function's body (`{}` or `return default;`). A function whose tests still pass is run but never checked. This costs one mutant per function. |
| 5 | Churn × complexity hotspots | high | M | `dgf hotspots`: functions ranked by how often git changed them times their cognitive complexity (and CRAP). These are the functions worth refactoring and mutating first. |
| 6 | Test smells in changed tests | high | M | Tests with no assertion, assertions only on not-null, swallowed exceptions, sleeps, skipped tests. AI-written tests often satisfy coverage this way. |
| 7 | Named smells with fix hints | medium | S | Deep nesting, a complex condition, a "bumpy road" of several nested blocks, too many parameters, a "brain method". Each smell gets a rule id and a one-line fix, so the agent knows what to do and not just a number. |
| 8 | Stable fingerprints | medium | S | Path + qualified name + a hash of the normalized body, in JSON, agent output and SARIF `partialFingerprints`, so results match across runs. |
| 9 | Likely-equivalent survivors | medium | M | Flag survivors that probably cannot be killed, and let the agent propose a `dgf-disable` comment with its reason, for a person to approve. |
| 10 | Change coupling | medium | M | In `dgf changes`: "B usually changes together with A, and this change does not touch it". |
| 11 | Rust mutation runs | medium | M | Rust mutants are listed but not run; run them through `cargo test` with the same switches. |
| 12 | A lean mutator level | low–medium | S | Fewer relational replacements: three of seven are enough to find the same tests, with 57% fewer mutants. |
| 13 | LLM-proposed mutants | low for now | L | Mutants written by a model for the change at hand (Meta's ACH), checked by the same compile and reach steps. |

### `dgf mcp`: proposed tools

Every tool returns `structuredContent` with an `outputSchema`, plus the same result as compact text (the `agent` format). Every tool takes `detail: "concise" | "full"` (concise by default) and pages long lists (`cursor`, `limit`, about 20 rows). A cut-off list says how to narrow the query. Errors set `isError` and say what to change.

| Tool | Kind | Input | Output |
| --- | --- | --- | --- |
| `dgf_analyze` | read-only | `paths`, `coverage`, `at_least` (ok, warn, fail), `sort` | the verdict, limits, and functions with id, name, place, the metrics, status, smells and a hint |
| `dgf_changes` | read-only | `base`, `staged` | the verdict and the blocking functions with before, after, place and a hint |
| `dgf_function` | read-only | `id`, or `file` and `line` | everything about one function: decision points, smells with fixes, uncovered lines, history, survivors |
| `dgf_hotspots` | read-only | `since`, `top` | functions ranked by changes × complexity |
| `dgf_mutate` | writes the cache | `paths` or `changes`, `level`, `max_minutes`, `run` | score, counts and survivors (id, function, place, before and after, the tests that reach it, the test file, the hint); progress while it runs |
| `dgf_mutants_check` | writes the cache | `ids` | the outcome of those mutants only, after the agent added tests |

Two resources: `dgf://config` (the settings in effect) and `dgf://report/latest` (the last full JSON). Two prompts: `reduce-complexity` and `kill-survivors`, which carry the skill's rules (no ignores, no gaming the score).

## Languages

- **A guide to adding a language.** The language rules (`crates/dog-food-core/src/lang/`), the mutators (`src/mutate/`) and a runner are the three pieces. A short guide with a template would make Go, Java, Python and Kotlin contributions straightforward.
- **TypeScript 6 and 7.** TypeScript 7 is the Go port (`tsgo`); the Angular type-check step should accept it. React (TSX) and Angular (signals, `input()`, `output()`) parse today. A parse census on large public repositories would show what does not.
- **C#.** tree-sitter-c-sharp 0.23 predates C# 14 and 15. dgf rewrites what the grammar does not know at the same length (`closed`, labeled `break`/`continue`, `a?.B = c`, `extension` blocks, `#:` directives, and `#if` branches that only parse one at a time). Newer grammar releases should replace those rewrites one by one.

## The research behind this

- **Process metrics.** Change history predicts defects better than code metrics alone (Rahman and Devanbu, ICSE 2013, https://2013.icse-conferences.org/content/how-and-why-process-metrics-are-better.html). Code health and defects: https://arxiv.org/abs/2203.04374 (a vendor study on 39 codebases).
- **AI and complexity.** After teams adopted an AI editor, static-analysis warnings and code complexity rose and stayed up, and the rise explained the later slowdown (MSR 2026, https://arxiv.org/abs/2511.04427). Code health predicted whether an AI refactoring kept the code's meaning (FORGE 2026, by CodeScene's authors, https://arxiv.org/abs/2601.02200). Duplicated blocks in AI-era code grew eightfold in 2024 (GitClear, an industry report).
- **Tools for agents.** Anthropic's guidance on tools for agents covers few tools, meaningful ids, a concise mode, paging, and errors that say what to change (https://www.anthropic.com/engineering/writing-tools-for-agents). Structured tool output and annotations are in the MCP specification (https://modelcontextprotocol.io/specification/2025-06-18/server/tools). Comparable servers exist for other code-quality tools.
- **Mutation testing.**
  - Meta's ACH writes targeted mutants and tests that kill them; engineers accepted 73% of the tests (https://arxiv.org/abs/2501.12862).
  - Feeding survivors back into test generation works (MuTAP, https://arxiv.org/abs/2308.16557). At Google, showing survivors during code review led to more tests (https://arxiv.org/abs/2103.07189).
  - Pseudo-tested methods are found in every project studied (https://arxiv.org/abs/1807.05030).
- **Weak evidence:** the maintainability index, Halstead metrics and test-to-code ratios. They are left out on purpose.
