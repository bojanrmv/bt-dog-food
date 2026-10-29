# Command reference

Every `dgf` command, every option, and how they combine. `dgf <command> --help` prints the same options from the program itself.

- [How options combine](#how-options-combine)
- [`dgf analyze`](#dgf-analyze): score every function
- [`dgf changes`](#dgf-changes): only what your branch changed
- [`dgf baseline`](#dgf-baseline): accept today's problems, fail only on new ones
- [`dgf history`](#dgf-history): trends from saved runs
- [`dgf mutate`](#dgf-mutate): mutation testing
- [`dgf init`](#dgf-init), [`dgf hook`](#dgf-hook), [`dgf skill`](#dgf-skill): setup
- [Output formats](#output-formats), [exit codes](#exit-codes)

## How options combine

- **Every option can be combined with every other option of the same command**, in any order. The exceptions are named under each command (for example, `--scope lines` for `mutate` only means something with `--changes`).
- **Command line, then `dgf.toml`, then the default.** An option given on the command line wins over the same setting in `dgf.toml`, which wins over the built-in default. `--no-config` ignores `dgf.toml` entirely.
- **Lists add up.** `--exclude` adds to the `exclude` list in `dgf.toml`, and can be repeated. `--warn` and `--fail` replace the limits from `dgf.toml` for this run.
- **Paths** on the command line are relative to the current folder; paths in `dgf.toml` are relative to the file. `dgf` finds `dgf.toml` in the current folder or the nearest parent.
- **Always skipped:** files ignored by `.gitignore` (also outside a git repository), and generated `*.d.ts`, `*.g.cs` and `*.Designer.cs` files.
- **Output:** every report command takes `--format <FORMAT>` and `-o, --output <FILE>`. Without `-o`, the report goes to the terminal; with it, to the file, and a one-line note to the terminal.

## Common options

These appear on `analyze`, `changes`, `baseline` and `mutate` (`baseline` writes a file instead of a report, so it has no `--format` or `-o`).

| Option | Meaning | Default |
| --- | --- | --- |
| `--warn <N>` | Functions above this cyclomatic complexity are flagged `warn` | `warn` in `dgf.toml`, else 10 |
| `--fail <N>` (also `--threshold`) | Functions above this make the run exit with code 1 | `fail` in `dgf.toml`, else no fail limit |
| `--exclude <PATTERN>` | Skip files or folders, `.gitignore` syntax; repeatable | none beyond `dgf.toml` |
| `--no-config` | Ignore `dgf.toml` | off |
| `--format <FORMAT>` | Report format, see [Output formats](#output-formats) | `table` |
| `-o, --output <FILE>` | Write the report to a file | the terminal |

## `dgf analyze`

Scores every function in the given files and folders, worst first.

```
dgf analyze [OPTIONS] [PATHS]...
```

| Option | Meaning | Default |
| --- | --- | --- |
| `[PATHS]...` | Files or folders to analyze | `.` |
| `--top <N>` | Rows in the table, Markdown and agent output; 0 = all | `top` in `dgf.toml`, else 20 |
| `--coverage <PATH>` | A coverage report (Cobertura, OpenCover or LCOV) or a folder with reports; adds coverage and CRAP per function; repeatable | `[coverage] paths` in `dgf.toml` |
| `--no-baseline` | Do not apply `.dgf/baseline.json`; functions it accepted count again | baseline applied |
| `--save-history` | Also save this run as a snapshot for `dgf history` | off |
| plus the [common options](#common-options) | | |

Formats: `table`, `json`, `markdown`, `html`, `sarif`, `agent`.

```powershell
dgf analyze                                          # this folder, top 20 by complexity
dgf analyze src tests/Shop.Tests                     # several folders
dgf analyze src/Orders/OrderService.cs               # one file
dgf analyze --top 0                                  # every function
dgf analyze --exclude "**/Migrations/" --exclude "dist/"
dgf analyze --warn 8 --fail 15                       # stricter or looser than dgf.toml for this run
dgf analyze --no-config --warn 10                    # ignore dgf.toml entirely
dgf analyze --coverage TestResults                   # add CRAP: complex and untested first
dgf analyze --coverage TestResults --coverage web/coverage/lcov.info   # C# and TypeScript reports together
dgf analyze --no-baseline                            # see what the baseline hides
dgf analyze --save-history                           # also keep a snapshot for trends
dgf analyze --format html -o dgf-report.html         # charts in the browser
dgf analyze --format markdown >> "$GITHUB_STEP_SUMMARY"   # CI job summary
dgf analyze --format sarif -o dgf.sarif              # GitHub code scanning
dgf analyze --format json -o dgf.json                # for other tools
dgf analyze --format agent                           # for an AI assistant
dgf analyze src --coverage TestResults --fail 15 --format html -o report.html   # all at once
```

## `dgf changes`

Compares the functions your branch and uncommitted edits touched with the same functions on a base branch: new, worse, better, same, removed. Needs a git repository.

```
dgf changes [OPTIONS]
```

| Option | Meaning | Default |
| --- | --- | --- |
| `--base <REF>` | Branch or commit to compare with | `[changes] base` in `dgf.toml`, else `main`, else `master` |
| `--staged` | Compare only what is staged for the next commit, with `HEAD` (the pre-commit hook uses this) | off |
| `--scope lines` | Report functions that contain a changed line | default |
| `--scope files` | Report every function in a changed file | |
| `--fail-on new-or-worse` | Only new functions and functions whose score went up can fail the run | default |
| `--fail-on touched` | Every reported function above `--fail` fails the run, changed score or not | |
| plus the [common options](#common-options) | | |

Formats: `table`, `json`, `markdown`, `html`, `sarif`, `agent`.

```powershell
dgf changes                                         # branch + uncommitted edits vs main
dgf changes --base develop
dgf changes --base origin/main                      # in CI, after fetching the base
dgf changes --base HEAD~3                           # the last three commits
dgf changes --staged                                # exactly what you are about to commit
dgf changes --scope files                           # every function in each changed file
dgf changes --fail-on touched --fail 10             # touching a complex function is enough to fail
dgf changes --format html -o dgf-changes.html       # before/after chart
dgf changes --base "origin/$GITHUB_BASE_REF" --format markdown >> "$GITHUB_STEP_SUMMARY"
dgf changes --format agent                          # what an AI assistant has to fix before finishing
```

## `dgf baseline`

Records every function that is above the limits today in `.dgf/baseline.json`. Later `analyze` and `changes` runs report those functions as `ignored` until their score goes up, so only new problems fail. Commit the file.

```
dgf baseline [OPTIONS] [PATHS]...
```

| Option | Meaning | Default |
| --- | --- | --- |
| `[PATHS]...` | Files or folders to record | `.` |
| `--file <FILE>` | Where to write the baseline | `.dgf/baseline.json` next to `dgf.toml` |
| `--warn`, `--fail`, `--exclude`, `--no-config` | As in the [common options](#common-options) | |

```powershell
dgf baseline                         # accept today's state
dgf baseline --fail 15               # accept only what is above 15
dgf analyze                          # accepted functions show as "ignored"
dgf analyze --no-baseline            # everything, as if there were no baseline
```

To excuse one function for good instead, put `// dgf-ignore: <reason>` on the line above it.

## `dgf history`

Shows the snapshots saved with `dgf analyze --save-history`: how totals moved, and which functions moved most.

```
dgf history [OPTIONS]
```

| Option | Meaning | Default |
| --- | --- | --- |
| `--dir <DIR>` | Folder with the snapshots | `[history] dir` in `dgf.toml`, else `.dgf/history` |
| `--top <N>` | Movers listed in the table and Markdown; 0 = all | 20 |
| `--no-config`, `--format`, `-o` | As in the [common options](#common-options) | |

Formats: `table`, `json`, `markdown`, `html`, `agent` (not `sarif`).

```powershell
dgf analyze --save-history                         # once per merge, release or week
dgf history
dgf history --top 0
dgf history --format html -o dgf-history.html      # trend lines
```

## `dgf mutate`

Mutation testing: changes the code on purpose (`>` becomes `>=`, a condition becomes `false`) and checks that a test fails. Without `--run` it only chooses functions and lists their mutants, which takes a second. With `--run` it builds and tests them. C#, TypeScript and JavaScript are run; Rust mutants are listed only. The full guide is [mutation-testing.md](mutation-testing.md).

```
dgf mutate [OPTIONS] [PATHS]...
```

**Choosing what to mutate**

| Option | Meaning | Default |
| --- | --- | --- |
| `[PATHS]...` | Files or folders (not used with `--changes`) | `.` |
| `--changes` | Only functions this branch and its uncommitted edits changed, plus functions whose tests changed | off |
| `--base <REF>` | Base for `--changes` | as for `dgf changes` |
| `--scope functions` | With `--changes`: every mutant of a changed function | default |
| `--scope lines` | With `--changes`: only mutants on changed lines | |
| `--min-complexity <N>` | Only functions at least this complex | 1 |
| `--level basic\|standard\|advanced` | How many kinds of mutation | `level` in `dgf.toml`, else `standard` |
| `--show` | List every mutant, not only the count per function | off |
| `--exclude`, `--warn`, `--fail`, `--no-config` | As in the [common options](#common-options); `[mutate] exclude` in `dgf.toml` also applies | |

**Running** (these need `--run`)

| Option | Meaning | Default |
| --- | --- | --- |
| `--run` | Build and test the mutants | off |
| `--test-filter <FILTER>` | `dotnet test --filter` for the runs: `"FILTER"` for every test project, `"Project.Tests:FILTER"` for one; repeatable; replaces `test_filter` in `dgf.toml` | all tests |
| `--exclude-test-project <GLOB>` | Leave out test projects matching this glob; repeatable | none |
| `--jobs <N>` | Test runs at the same time | `jobs` in `dgf.toml`, else CPU cores − 1 |
| `--build-jobs <N>` | Projects `dotnet build` builds at the same time (`-m:N`) | all cores |
| `--low-priority` | Run dgf, its builds and tests below normal priority | off |
| `--max-minutes <N>` | Stop after this long, keep what was tested, exit with code 4 | no limit |
| `--fail-under <PERCENT>` (also `--break-at`) | Exit with code 1 when the mutation score is lower | `thresholds.break` in `dgf.toml` |
| `--timeout-factor <N>`, `--timeout-ms <MS>` | A test run counts as hanging after (startup + its tests' time) × N + MS | 1.5 and 5000 |
| `--sandbox closure\|full` | Copy only the projects the tests need, or the whole repository | `closure` |
| `--no-mix` | One mutant per test run instead of packing several | packing on |
| `--rerun-survivors` | Test survivors again even if nothing changed; kills still come from the cache, and so do survivors already tested again against the same tests | off |
| `--cached` | With `--run`: report the cached results only and test nothing, such as to render another format of the last run; mutants without a cached result are listed as not in the cache | off |
| `--neighbours` | With `--changes`: also mutate the lines of other functions (same language, not tests) that hold a string key of a changed function, such as `"MemberPlanId"` or a constant holding it; their survivors are listed under "next to your change" and left out of the score and exit code | off |
| `--no-cache` | Test everything again, ignoring cached results | cache on |

Formats: without `--run`, `table`, `json`, `agent`; with `--run`, `table`, `json`, `html`, `agent`, `mutation-report`.

```powershell
# List only: which functions, how many mutants (a second, nothing is built)
dgf mutate src/Orders
dgf mutate --changes                                  # what this branch changed
dgf mutate --changes --show                           # every mutant, before and after
dgf mutate --min-complexity 5 --level advanced

# Run: build, test, score
dgf mutate --changes --run                            # the everyday use
dgf mutate --changes --scope lines --run              # only mutants on lines you changed
dgf mutate src/Orders --run --format html -o mutation.html
dgf mutate --run --test-filter "Shop.Tests:FullyQualifiedName~Pricing" --exclude-test-project "**/*.Acceptance.Tests.csproj"
dgf mutate --changes --run --max-minutes 30 --jobs 2 --low-priority   # on a shared or busy machine
dgf mutate --changes --run --fail-under 70            # gate: exit 1 below 70%
dgf mutate --changes --run --format agent             # survivors and how to kill them, for an AI assistant
dgf mutate --changes --run --rerun-survivors          # after adding tests
dgf mutate --run --no-cache                           # everything again
dgf mutate --run --format mutation-report -o mutation.json   # mutation-testing-elements JSON
```

Turning single mutants off is done in code: `// dgf-disable-once <mutator>: <reason>`, `// dgf-disable-next-line`, `// dgf-disable` ... `// dgf-restore` (see [mutation-testing.md](mutation-testing.md#turning-mutants-off)).

## `dgf duplication`

Code that can be united: copies, near misses, reordered or interleaved statements, long expressions copied into different statements, and with `--semantic` C# that does the same thing written differently and repeated queries. `dgf dup` is short for it, and `dgf-dup` is the same tool on its own. It reads the coverage reports `dgf.toml` names, to give each group a CRAP score.

```powershell
dgf dup                                   # the current folder: the groups that save most first
dgf dup --coverage TestResults            # with CRAP scores, the riskiest first (--rank saves|risk)
dgf dup --semantic --confirm              # C# rewrites, each confirmed by running it on generated inputs
dgf dup --changes --fail-on-copies        # a PR gate: exit 1 when the branch adds a copy
dgf dup --format html -o dup.html         # the review dashboard
```

All of it is in [duplication.md](duplication.md).

## `dgf init`

Writes a commented `dgf.toml` in the current folder with every setting explained. It refuses to overwrite an existing one.

```powershell
dgf init
```

## `dgf hook`

```powershell
dgf hook install            # .git/hooks/pre-commit runs `dgf changes --staged` (honors core.hooksPath)
dgf hook install --force    # replace a pre-commit hook dgf did not write
dgf hook uninstall          # remove the hook dgf wrote
```

The hook blocks a commit that adds a function above the `fail` limit or makes one worse. `git commit --no-verify` skips it once.

## `dgf skill`

The Claude Code skill teaches the agent to run `dgf` and act on what it reports.

```powershell
dgf skill install           # .claude/skills/dog-food/SKILL.md in this repository
dgf skill install --global  # ~/.claude/skills, for every project of this user
dgf skill show              # print it
```

## Output formats

| Format | `analyze` | `changes` | `history` | `mutate` | `mutate --run` | For |
| --- | :-: | :-: | :-: | :-: | :-: | --- |
| `table` | ✓ | ✓ | ✓ | ✓ | ✓ | the terminal (default) |
| `json` | ✓ | ✓ | ✓ | ✓ | ✓ | other tools; carries a `schemaVersion` |
| `markdown` | ✓ | ✓ | ✓ | | | PR comments, CI job summaries |
| `html` | ✓ | ✓ | ✓ | | ✓ | one self-contained page with charts |
| `sarif` | ✓ | ✓ | | | | GitHub code scanning, editors |
| `agent` | ✓ | ✓ | ✓ | ✓ | ✓ | AI assistants: only what needs attention |
| `mutation-report` | | | | | ✓ | mutation-testing-elements JSON, for its report viewers |

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Nothing above the `fail` limit (for `changes`: no new or worse function above it) |
| 1 | Something is above the `fail` limit, or the mutation score is below `--fail-under`. The report is still complete. |
| 2 | Usage or settings error: unknown option, invalid `dgf.toml`, not a git repository for `changes`, a format the command does not support |
| 3 | A path does not exist or a file could not be written |
| 4 | `dgf mutate --max-minutes` ran out before every mutant was tested |
