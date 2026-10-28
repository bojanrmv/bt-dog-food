---
name: dog-food
description: Measure and reduce function complexity with the dog-food CLI (`dgf`) for C#, TypeScript/JavaScript and Rust. Use when asked about code complexity, CRAP scores, refactoring hotspots, or whether a change made code harder to maintain; when a repo has a dgf.toml or .dgf/ folder; and before finishing a change in such a repo.
---

# dog-food (`dgf`)

`dgf` scores every function from its syntax alone (it never builds or runs code):

- **cyclomatic** = 1 + each `if`, loop, `case` label, `catch`, `?:`, `&&`, `||`, `??`, C# pattern `and`/`or`, match/switch-expression arm after the first. This is the number the limits apply to.
- **cognitive** = branches and loops cost 1 + their nesting depth; `else`/`else if` and each run of `&&`/`||` add 1. Higher means harder to read.
- **nesting** = deepest nesting of branches/loops; **lines**, **params** as named.
- **CRAP** (only with `--coverage`) = cyclomatic² × (1 − coverage)³ + cyclomatic. High CRAP = complex and untested; above 30 is risky.

Limits live in `dgf.toml` (`[limits] warn`, `fail`, per-path `[[limits.overrides]]`). A function scores `FAIL` above `fail`, `WARN` above `warn`. `ignored` means a `// dgf-ignore: reason` comment or the baseline (`.dgf/baseline.json`) excuses it.

## Commands (always use `--format agent`; it is built for you)

| Goal | Command |
|---|---|
| Whole repo, worst first | `dgf analyze --format agent` |
| One folder or file | `dgf analyze src/Orders --format agent --top 50` |
| What this branch + uncommitted edits changed vs main | `dgf changes --format agent` |
| Only what is staged for commit | `dgf changes --staged --format agent` |
| Complex and untested code | `dgf analyze --coverage <report or folder> --format agent` |
| Trend over saved runs | `dgf history --format agent` |

If `dgf` is not found in PowerShell, run `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`. Exit code 1 only means something is above `fail`; the output is still complete. Exit code 2 is a usage or `dgf.toml` error: read the message. With `--format agent` the last line says the same (`exit: N (meaning)`), and `warning:` lines under the first lines (base fallback, a dgf.toml that differs from the base branch's) change what the counts mean: read them before trusting the result. `dgf mutate --format agent` starts with a `scope:` block (the dgf.toml it follows and what its excludes left out, the base and how it was chosen, how far HEAD is behind it, and after `--run` the test projects that ran): check it first.

## Reading `--format agent` output

```
dgf analyze: FAIL. 13 files, 229 functions, 14 ms. fail=1 warn=3 ok=225 ignored=0
limits: cyclomatic warn above 5, fail above 6
columns: status cyclomatic cognitive nesting lines params location name | decisions

## above limits (4)
FAIL 8 6 2 31 2 src/Orders/OrderService.cs:28-58 OrderService.CalculateDiscount | &&=1 ??=1 case=2 if=2 loop=1
```

- Line 1 is the verdict. Sections: `above limits` (act on these), `most complex within limits` (watch list), `ignored`.
- `location` is `file:startLine-endLine`; open exactly that range.
- `decisions` explains the score: which constructs, how many. `case=5` suggests a lookup table or polymorphism; many `if` with high `nesting` suggests guard clauses; many `&&`/`||` suggests named boolean helpers.
- `dgf changes` lines read `change before->after status location name`. Changes are `new`, `worse`, `better`, `same`, `removed`. By default only `new` and `worse` functions can fail the run, so existing debt you merely touched does not block.

## Reducing complexity

1. Run `dgf analyze --format agent` (or `dgf changes --format agent` for the current work). Start with `FAIL`, then `WARN`; with coverage, start with the highest CRAP.
2. Read the function and its `decisions`. Prefer, in order: guard clauses and early returns (removes nesting), extracting a well-named helper per responsibility, replacing `switch`/`if` chains on data with a lookup table or dictionary, replacing type switches with polymorphism, naming compound conditions.
3. Keep behavior identical. Run the project's tests after each change.
4. Re-run `dgf changes --format agent` and confirm the function is `better` and nothing new crossed a limit. Report before -> after scores to the user.

Rules:
- Do not add `dgf-ignore` comments, edit the baseline or raise limits unless the user asks. Suggest it when code is generated, a parser/state machine, or otherwise clearer as one function, and say why.
- Do not game the score (moving logic into macros, cramming conditions into one expression, or splitting a function into pieces that only make sense together). The goal is code a person can follow.
- Lambdas and closures are scored separately from the function that contains them.

## Other commands

- `dgf baseline` records today's over-limit functions in `.dgf/baseline.json`; later runs ignore them until their score goes up. For adopting dgf in old code.
- `dgf hook install` adds a git pre-commit hook running `dgf changes --staged`.
- `dgf analyze --save-history` saves a snapshot for `dgf history`.
- `dgf mutate --changes --run --format agent` (C# with dotnet test; TypeScript/JavaScript with Vitest or Jest) tests whether the tests notice small deliberate bugs (mutants) in the functions this branch changed. Each `survivors` line is a change no test caught: `no_test_runs_this` means write a test that calls the function, `tests_still_pass` means add an assertion that would fail for that exact change (for example a boundary value for `<` -> `<=`). The `tests:` line under it names the tests that reach the mutant (put the assertion there) and `assert:` says what kind of assertion catches it; `boundary-risk` marks a date comparison, the off-by-one-day bug, which is never equivalent. After adding tests, `--rerun-survivors` tests just the survivors again. `--neighbours` also mutates code next to the change: other functions holding a string key the changed functions use (a workflow input key, a header name); its survivors come under "next to your change", outside the score, for you to judge. It needs passing tests, takes seconds to minutes, and never touches the working tree. On a large solution, scope it like the tests you would run by hand: `--test-filter "Project.Tests:FullyQualifiedName~ClassTests"`, `--exclude-test-project "**/*.Acceptance.Tests.csproj"`, and `--max-minutes N` (exit code 4 means the budget ran out; run it again to continue). Results are cached per mutant in `.dgf/mutation-cache/`, so a rerun only tests what changed (`--no-cache` to force); `dgf analyze` then shows each function's mutation score. Do not hide a survivor with a `// dgf-disable-once <mutator>: reason` comment unless the user agrees it cannot matter; the reason is required and every report lists the comment.
- `--format html -o file.html` (humans), `markdown` (PR comments), `json` (everything), `sarif` (code scanning).
- `dgf init` creates a commented `dgf.toml`.
