# dog-food

**dog-food** scores every function in a codebase for how hard it is to change safely. It reads **C#, TypeScript/JavaScript and Rust**, runs as one fast binary called **`dgf`**, and can fail a build when a function gets too complex.

```
$ dgf analyze src --coverage TestResults
COMPLEXITY  COGNITIVE  STATUS   COVERAGE    CRAP  FUNCTION          LOCATION
        11         15  FAIL           0%   132.0  Pricing.Shipping  src/Shop/Pricing.cs:20-34
         8          5  warn          91%     8.0  Pricing.Discount  src/Shop/Pricing.cs:5-18

2 functions in 2 files, 0.014 s
limits: warn above 5, fail above 10; CRAP warn above 30, no fail limit
result: 0 ok, 1 warn, 1 fail, 0 ignored
```

Per function it reports:

| Metric | What it tells you |
| --- | --- |
| **Cyclomatic complexity** | The number of independent paths: 1 + every `if`, loop, `case`, `catch`, `?:`, `&&`, `\|\|`. The limits apply to this one. |
| **Cognitive complexity** | How hard it is to read: branches cost more the deeper they are nested. |
| **Nesting, lines, parameters** | Shape of the function. |
| **Coverage and CRAP** | With a coverage report from your tests: complexity² × (1 − coverage)³ + complexity. High means complex *and* untested. |

Apart from `dgf mutate --run`, which builds and tests a copy of your code, `dgf` only reads files. It never changes code, which is also why it is fast: this repository (650 functions) takes about 50 ms, and 40 MB of real Rust code about 6 s on a 4-core laptop ([benchmarks](docs/benchmarks.md)). The name comes from *dogfooding*: the tool checks its own source with the same limits it enforces on others.

## Install

There are no prebuilt downloads or registry packages yet, so `dgf` is built from source. That takes a few minutes once.

**1. Tools, once per machine**

| System | Rust | C compiler (the language grammars are C) |
| --- | --- | --- |
| Windows | `winget install Rustlang.Rustup` | `winget install Microsoft.VisualStudio.2022.BuildTools`, then pick "Desktop development with C++" |
| macOS | [rustup.rs](https://rustup.rs) | `xcode-select --install` |
| Linux | [rustup.rs](https://rustup.rs) | `build-essential` (Debian, Ubuntu) or `gcc` |

**2. Install `dgf` from GitHub**

```powershell
cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-cli --locked
dgf --version
```

`dog-food-cli` is the package; the command it installs is `dgf`. `--locked` uses the exact library versions the repository was tested with. Add `--rev <commit>` for a particular version.

**From a clone** (to change the code, or offline):

```powershell
git clone https://github.com/bojanrmv/bt-dog-food
cd bt-dog-food
cargo install --path crates/dog-food-cli --locked
```

**Update and remove**

```powershell
cargo install --git https://github.com/bojanrmv/bt-dog-food dog-food-cli --locked --force   # update
cargo uninstall dog-food-cli                                                                # remove
```

`dgf` lands in `~/.cargo/bin`, which the Rust installer puts on the PATH. If a terminal says `dgf` is not recognized, it was opened before Rust was installed: open a new one, or on Windows run `$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"`.

For `dgf mutate --run` you also need the test tools of the code you mutate: the .NET SDK (`dotnet`) for C#, Node.js 22.3 or newer with the project's `node_modules` for TypeScript and JavaScript. Everything else in `dgf` needs nothing but the binary.

## Quick start

```powershell
cd your-repo
dgf init                                       # commented dgf.toml: limits, excludes, coverage
dgf analyze                                    # worst functions first
dgf analyze --format html -o dgf-report.html   # the full picture in a browser
dgf changes                                    # only what your branch changed, before vs after
```

[docs/setup-in-another-repo.md](docs/setup-in-another-repo.md) is a step-by-step setup you can hand to a colleague or an AI assistant.

## What you can do with it

| Task | Command | Guide |
| --- | --- | --- |
| Look up any command, option or format | `dgf <command> --help` | [Command reference](docs/commands.md) |
| Find the most complex functions | `dgf analyze` | [Metrics](docs/metrics.md) |
| Find complex code nobody tests | `dgf analyze --coverage <report or folder>` | [Coverage and CRAP](docs/coverage.md) |
| Check only what your branch or PR changed | `dgf changes` | [Workflows](docs/workflows.md#changes) |
| Block a commit that makes code worse | `dgf hook install` | [Workflows](docs/workflows.md#pre-commit-hook) |
| Adopt it in old code without fixing everything first | `dgf baseline`, `// dgf-ignore: reason` | [Workflows](docs/workflows.md#adopting-in-existing-code) |
| Track complexity over time | `dgf analyze --save-history`, `dgf history` | [Workflows](docs/workflows.md#history) |
| Check that tests would catch bugs (mutation testing, C# and TypeScript/JavaScript) | `dgf mutate --changes --run` | [Mutation testing](docs/mutation-testing.md) |
| Find copied code and how to unite it | `dgf-dup` | [Duplication](docs/duplication.md) |
| Gate CI, annotate PRs | `--format markdown`, `--format sarif`, exit codes | [Workflows](docs/workflows.md#ci) |
| See scores in the editor | VS Code extension in `editors/vscode` | [Integrations](docs/integrations.md#vs-code) |
| Let Claude Code (or another agent) use it | `--format agent`, `dgf skill install` | [Integrations](docs/integrations.md#ai-assistants) |
| Use the analysis from Rust code | the `dog-food-core` crate | [Integrations](docs/integrations.md#rust-library) |
| See what comes next (MCP server, agent hooks, hotspots, more test runners) | – | [Roadmap](docs/roadmap.md) |

## Commands

| Command | What it does |
| --- | --- |
| `dgf analyze [PATHS]` | Score every function, worst first; with `--coverage`, also coverage and CRAP |
| `dgf changes` | Only the functions your branch and uncommitted edits changed, before and after |
| `dgf baseline` | Accept today's functions above the limits, so only new problems fail |
| `dgf history` | Trends from runs saved with `dgf analyze --save-history` |
| `dgf mutate [PATHS]` | Mutation testing: list mutants, or `--run` them against your tests |
| `dgf init` | Write a commented `dgf.toml` |
| `dgf hook install` / `uninstall` | A git pre-commit hook that runs `dgf changes --staged` |
| `dgf skill install` / `show` | The Claude Code skill that teaches the agent to use `dgf` |

Every option of every command, its default, which formats each command supports, and examples: **[docs/commands.md](docs/commands.md)**. The program prints the same with `dgf <command> --help`.

Options combine freely. A command-line option wins over `dgf.toml`, which wins over the default. Every report command takes `--format table|json|markdown|html|sarif|agent` (and `mutation-report` for `mutate --run`) and `-o <file>`.

### Recipes

```powershell
# Day to day
dgf analyze                                            # worst functions in this folder
dgf analyze src --format html -o dgf-report.html       # the full picture in a browser
dgf changes                                            # did my branch make anything worse?

# Find complex code nobody tests
dotnet test --collect:"XPlat Code Coverage"            # or: npx vitest run --coverage
dgf analyze --coverage TestResults                     # highest CRAP first

# Before every commit
dgf hook install                                       # blocks commits that add or worsen a function above the limit

# Adopt it in an old codebase
dgf init                                               # set warn / fail in dgf.toml
dgf baseline                                           # accept today's state; commit .dgf/baseline.json
dgf analyze                                            # from now on only new problems fail

# Check that the tests would catch bugs
dgf mutate --changes                                   # which functions, how many mutants (instant)
dgf mutate --changes --run                             # build, test, list the survivors
dgf mutate --changes --scope lines --run --max-minutes 20 --low-priority

# Pull request in CI (fetch-depth: 0)
dgf analyze --format markdown >> "$GITHUB_STEP_SUMMARY"
dgf changes --base "origin/$GITHUB_BASE_REF" --format markdown >> "$GITHUB_STEP_SUMMARY"
dgf analyze --format sarif -o dgf.sarif                # upload to GitHub code scanning

# With an AI assistant
dgf skill install                                      # Claude Code learns the commands and rules
dgf changes --format agent                             # what to fix before finishing
dgf mutate --changes --run --format agent              # survivors, with the tests to extend

# Track over time
dgf analyze --save-history                             # on every merge to main
dgf history --format html -o dgf-history.html
```

### Exit codes

| Code | Meaning |
| --- | --- |
| 0 | Nothing above the `fail` limit (for `changes`: no new or worse function above it) |
| 1 | Something is above the `fail` limit, or the mutation score is below `--fail-under`. The report is still complete. |
| 2 | Usage or settings error: unknown option, invalid `dgf.toml`, not a git repository for `changes`, a format the command does not support |
| 3 | A path does not exist or a file could not be written |
| 4 | `dgf mutate --max-minutes` ran out before every mutant was tested |

With `--format agent`, the last line repeats the code and what it means, such as `exit: 4 (budget: ... run the same command again ...)`.

## Settings: `dgf.toml`

`dgf` finds `dgf.toml` in the current folder or a parent, like cargo finds `Cargo.toml`. Paths in it are relative to the file. Every section is optional; unknown keys are an error, so typos show up.

```toml
[analyze]
exclude = ["dist/", "**/generated/", "tests/fixtures/"]   # .gitignore syntax
top = 20

[limits]
warn = 5            # above: "warn", still passes
fail = 6            # above: "FAIL", exit code 1
crap_warn = 30      # with coverage: CRAP above this is a warning (default 30)
# crap_fail = 60

[[limits.overrides]]   # looser limits for some paths; the last match wins
paths = ["src/Legacy/", "**/tests/"]
warn = 10
fail = 15

[changes]
base = "main"
scope = "lines"           # or "files"
fail_on = "new-or-worse"  # or "touched"
fail = 10                 # dgf changes only: hold new code to more than [limits] holds old code

[coverage]
paths = ["TestResults", "coverage/lcov.info"]   # used when --coverage is not given

[history]
dir = ".dgf/history"

[mutate]                  # every key: docs/mutation-testing.md#settings
exclude = ["**/Migrations/"]
exclude_test_projects = ["**/*.Acceptance.Tests.csproj"]
test_filter = ["Shop.Tests:FullyQualifiedName~Pricing"]
max_minutes = 45
ignore_methods = ["*Log*", "ToString"]   # calls whose arguments are never mutated
thresholds = { high = 80, low = 60, break = 50 }
```

`dgf init` writes every setting with a comment. Always applied: `.gitignore` (even outside git), and generated files `*.d.ts`, `*.g.cs`, `*.Designer.cs` are skipped. Without a `dgf.toml`: warn above 10, no fail limit.

## Output formats

| Format | For | Notes |
| --- | --- | --- |
| `table` | the terminal | worst first; `--top` rows |
| `html` | people | one self-contained page with charts; works offline, light and dark theme |
| `markdown` | PR comments, CI job summaries | verdict, summary, everything above a limit, top N |
| `json` | other tools | everything, stable `schemaVersion` |
| `sarif` | GitHub code scanning, editors | functions above a limit as annotations |
| `agent` | AI assistants | compact text: verdict, legend, one line per function that needs attention |

The HTML report has: summary numbers, score distribution, decision drivers, size versus complexity, complex versus untested (with coverage), hotspots per file and a searchable table. `dgf changes` and `dgf history` have their own pages: a before-and-after chart of every changed function, and trend lines with the biggest movers.

## How it works

```
source files ─► tree-sitter parses each file ─► one walk per tree scores every function ─► report
                (all CPU cores in parallel)        (cyclomatic, cognitive, nesting, ...)
```

The `dog-food-core` library holds the walker and one small rule file per language; the `dgf` binary adds discovery, settings, limits, git, coverage and reports. Design and decisions: [docs/architecture.md](docs/architecture.md). Exact counting rules: [docs/metrics.md](docs/metrics.md).

## Development

```powershell
cargo test                       # unit, fixture and doc tests, plus an end-to-end mutation run
                                 # on a small xUnit solution (needs a .NET 10 SDK; DGF_SKIP_E2E=1 skips it)
cargo clippy --all-targets       # lints (CI uses -D warnings)
cargo fmt
cargo run -p dog-food-cli -- analyze
cargo run -p dog-food-core --example dump -- File.cs          # a file's syntax tree
cargo run --release -p dog-food-core --example bench -- <dir>  # parsing speed
cargo run -p dog-food-core --example schemata -- File.cs      # a file with every mutant switched in
```

This repository checks itself with its own [dgf.toml](dgf.toml) (warn above 5, fail above 6), in CI ([.github/workflows/ci.yml](.github/workflows/ci.yml)) and through `dgf changes` before merging.

| Path | What it is |
| --- | --- |
| `crates/dog-food-core` | Library: walker (`src/walker.rs`), language rules (`src/lang/`), data model |
| `crates/dog-food-cli` | `dgf`: settings, discovery, limits, git, coverage, baseline, history, hook/skill setup, reports (`src/report/`, HTML in `*.html` + `style.css` + `common.js`) |
| `crates/dog-food-cli/assets/SKILL.md` | The Claude Code skill installed by `dgf skill install` |
| `crates/dog-food-core/tests/fixtures` | Sample files; `// dgf: 7 cognitive=9 nesting=3 params=2` states what the next function must score |
| `crates/dog-food-core/tests/mutants` | C# (`Mutators.cs`, and `Modern.cs` for C# 12-15) and TypeScript files; `// mutant: relational > -> >=` states each mutant the next line must get |
| `crates/dog-food-cli/tests/e2e/dotnet` | A small xUnit solution that `tests/mutate_dotnet.rs` runs `dgf mutate --run` on |
| `crates/dog-food-cli/tests/schema` | The schema `--format mutation-report` is checked against |
| `editors/vscode` | VS Code extension |
| `docs/` | Command reference, guides, architecture, benchmarks, setup for other repos, roadmap |

### Adding a language or a rule

1. `cargo run -p dog-food-core --example dump -- sample.ext` shows the node names tree-sitter uses.
2. Add the grammar crate and implement `LanguageAnalyzer` in `crates/dog-food-core/src/lang/` (`function_kind`, `decision`, `nests`, `name`); map the extensions in `Language::from_path`.
3. Add a unit test per rule and a fixture file with `// dgf:` markers.
4. Run `dgf analyze` here: the new code has to pass the same limits.

## License

[MIT](LICENSE): free to use, change and share, commercially too. dgf is maintained in spare time, on a best-effort basis, and comes without any warranty.
