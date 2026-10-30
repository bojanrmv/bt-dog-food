<h1 align="center">dog-food</h1>

<p align="center">
  <b>Find the code that is hard to change safely, and why.</b><br>
  Complexity · CRAP · mutation testing · duplication — for C#, TypeScript/JavaScript and Rust, in one fast binary.
</p>

<p align="center">
  <a href="https://github.com/bojanrmv/bt-dog-food/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/bojanrmv/bt-dog-food/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="License: MIT" src="https://img.shields.io/badge/license-MIT-blue.svg"></a>
  <img alt="Languages: C#, TypeScript, JavaScript, Rust" src="https://img.shields.io/badge/reads-C%23%20%7C%20TypeScript%20%7C%20JavaScript%20%7C%20Rust-informational">
  <img alt="Written in Rust" src="https://img.shields.io/badge/written%20in-Rust-orange">
</p>

<p align="center">
  <a href="#install">Install</a> ·
  <a href="#quick-start">Quick start</a> ·
  <a href="#a-tour">Tour</a> ·
  <a href="#documentation">Documentation</a> ·
  <a href="docs/commands.md">Command reference</a>
</p>

---

**dog-food** is a code-health toolkit. Its command, **`dgf`**, scores every function in a codebase for how hard it is to change, shows which complex code your tests do not cover or would not notice breaking, finds copied code and says how to unite it, and can fail a build or a pull request when any of it gets worse. It runs locally and in CI, writes reports for people (HTML dashboards), for pipelines (Markdown, SARIF, JSON) and for AI assistants, and never needs a server or an account.

The name comes from *dogfooding*: this repository is checked by its own tool, with the limits it enforces on others.

## What's inside

| | Tool | What it answers | C# | TS / JS | Rust |
| --- | --- | --- | :-: | :-: | :-: |
| 📐 | **Complexity and CRAP**<br>`dgf analyze` | Which functions are too complex, and which of those does no test cover? Cyclomatic and cognitive complexity, nesting, size, and the CRAP score from your coverage reports. | ✓ | ✓ | ✓ |
| 🔀 | **Branch checks**<br>`dgf changes` | Did this branch or commit make anything worse? Before-and-after scores of only the changed functions, as a PR gate or a pre-commit hook. | ✓ | ✓ | ✓ |
| 🧬 | **Mutation testing**<br>`dgf mutate --run` | Would the tests notice a bug? Small deliberate bugs, run against your tests, packed many to a test run and cached, so a branch takes minutes, not hours. | ✓ | ✓ | listed |
| 🧩 | **Duplication**<br>`dgf dup` | What is copied, and how do I unite it? Exact and renamed copies, near misses, reordered statements; for C#, code that does the same thing written differently, and repeated database queries. | ✓ | ✓ | ✓ |
| 📊 | **Dashboards**<br>`--format html` | One self-contained page per report, with charts, filters and a light and dark theme. | ✓ | ✓ | ✓ |
| 🔌 | **Integrations** | CI summaries and gates, GitHub code scanning (SARIF), a VS Code extension, a Claude Code skill, and a Rust library. | ✓ | ✓ | ✓ |

**How it behaves**

- **Fast.** Files are parsed with tree-sitter on every core: 4,000 functions in under half a second, 40 MB of Rust in about 6 seconds ([benchmarks](docs/benchmarks.md)).
- **Read only.** `dgf` reads files and never changes them. Only `dgf mutate --run` builds and tests, and it does so in a copy of your code.
- **Exact, not guessed.** Every score and finding comes from the syntax tree (and, for C# duplication, the compiler), with the rule that produced it.
- **Adoptable.** A baseline accepts today's code so only new problems fail; `dgf changes` holds new code to a stricter limit than old code.

## Install

```powershell
dotnet tool install -g dgf
dgf --version
```

The native binary for Windows, Linux or macOS, with the C# duplication engine; it needs the .NET 10 SDK. Building from source with cargo, the per-platform downloads, pinning a version per repository, and the test tools mutation runs use are in **[docs/install.md](docs/install.md)**.

## Quick start

```powershell
cd your-repo
dgf init                                       # a commented dgf.toml: limits, excludes, coverage
dgf analyze                                    # every function, worst first
dgf analyze --format html -o dgf-report.html   # the same, as a dashboard
dgf changes                                    # only what your branch changed, before and after
dgf mutate --changes --run                     # would the tests catch a bug in what you changed?
dgf dup                                        # copied code, biggest wins first
```

Setting it up in a repository step by step, or handing the setup to a colleague or an AI assistant: [docs/setup-in-another-repo.md](docs/setup-in-another-repo.md).

## A tour

### Complexity and CRAP

```
$ dgf analyze src --coverage TestResults
COMPLEXITY  COGNITIVE  STATUS   COVERAGE    CRAP  FUNCTION          LOCATION
        11         15  FAIL           0%   132.0  Pricing.Shipping  src/Shop/Pricing.cs:20-34
         8          5  warn          91%     8.0  Pricing.Discount  src/Shop/Pricing.cs:5-18
```

| Metric | What it tells you |
| --- | --- |
| **Cyclomatic complexity** | the number of independent paths: 1 + every `if`, loop, `case`, `catch`, `?:`, `&&`, `\|\|`. The limits apply to this one. |
| **Cognitive complexity** | how hard it is to read: branches cost more the deeper they are nested |
| **Nesting, lines, parameters** | the function's shape |
| **CRAP** | complexity² × (1 − coverage)³ + complexity, from the coverage reports your tests already write (Cobertura, OpenCover, LCOV). High means complex *and* untested. |

![The complexity dashboard: totals, score distribution, what drives complexity, size versus complexity](docs/images/complexity.png)

![How dgf scores code: read, parse, score every function, judge against the limits, report](docs/images/how-dgf-works.svg)

More: [metrics](docs/metrics.md) · [coverage and CRAP](docs/coverage.md) · [limits and dgf.toml](docs/configuration.md)

### Branch checks

`dgf changes` compares your branch and uncommitted edits with its base branch, and fails only on functions that are new or got worse, so old code can stay as it is.

```powershell
dgf changes                       # what this branch did
dgf hook install                  # check every commit before it is made
dgf baseline                      # accept today's code, so only new problems fail
```

![The changes dashboard: each changed function's score before and after](docs/images/changes.png)

More: [workflows: branches, hooks, baselines, history, CI](docs/workflows.md)

### Mutation testing

`dgf mutate` makes small bugs in your code (`>=` → `>`, `true` → `false`, a removed statement) and runs your tests against each. A bug no test notices is a test to write. It records which test reaches which mutant, packs many mutants into one test run, runs only the tests that reach them, and caches every result, so re-running a branch costs seconds. C# with xUnit, NUnit or MSTest; TypeScript and JavaScript with Vitest, Jest or Angular's test builder.

```powershell
dgf mutate --changes              # which functions, how many mutants (instant)
dgf mutate --changes --run        # build, test, list what the tests miss
dgf mutate --run --format agent   # the survivors, with the tests to extend, for an AI assistant
```

![The mutation dashboard: the score, each mutant no test caught, results by function](docs/images/mutation.png)

More: [mutation testing](docs/mutation/README.md): choosing what to mutate, the mutators, turning mutants off, settings, how it works.

### Duplication

`dgf dup` finds copied code and says how to unite it: the shared template, what each copy fills in, where a shared version goes, and the risk of leaving it (the CRAP score of the riskiest copy). It finds exact and renamed copies, near misses, the same statements in another order or mixed with other code, and long expressions copied into different statements. For C#, `--semantic` adds methods that do the same thing written differently (a loop and its LINQ, `if` and `switch`), repeated database queries with the extension method that replaces them, and `--confirm` runs each such claim on generated inputs.

```powershell
dgf dup                           # biggest wins first
dgf dup --format html -o dup.html # review and mark each group
dgf dup --changes --fail-on-copies # fail a pull request that adds a copy
```

![The duplication review dashboard: groups by kind, a queue of groups, and the selected group side by side](docs/images/duplication.png)

More: [duplication](docs/duplication/README.md): reading a finding, the review dashboard, how the engines work, the C# engine, accuracy.

### Reports and integrations

| Format | For |
| --- | --- |
| `table` | the terminal (default) |
| `html` | people: [the dashboards](docs/dashboards.md) |
| `markdown` | PR comments and CI job summaries |
| `sarif` | GitHub code scanning and editors |
| `json` | other tools; carries a `schemaVersion` |
| `agent` | AI assistants: only what needs attention, with what to do |

Exit codes: 0 within the limits, 1 above them, 2 a usage error, 3 a missing path, 4 a mutation run out of time.

The [VS Code extension](docs/integrations.md#vs-code) shows scores in the editor; `dgf skill install` teaches [Claude Code](docs/integrations.md#ai-assistants) to use `dgf`; the analysis is also a [Rust library](docs/integrations.md#rust-library).

## Documentation

| Start here | Use it | Understand it |
| --- | --- | --- |
| [Install](docs/install.md) | [Command reference](docs/commands.md) | [How metrics are counted](docs/metrics.md) |
| [Set up a repository](docs/setup-in-another-repo.md) | [Configuration (`dgf.toml`)](docs/configuration.md) | [Architecture](docs/architecture.md) |
| [Dashboards](docs/dashboards.md) | [Workflows: branches, hooks, CI](docs/workflows.md) | [Benchmarks](docs/benchmarks.md) |
| | [Coverage and CRAP](docs/coverage.md) | [Duplication research](docs/research/csharp-semantic-duplication.md) |
| | [Mutation testing](docs/mutation/README.md) | [Roadmap](docs/roadmap.md) |
| | [Duplication](docs/duplication/README.md) | [Development](docs/development.md) |
| | [Integrations](docs/integrations.md) | |

## Repository

| Path | What it is |
| --- | --- |
| [`crates/dog-food-core`](crates/dog-food-core) | the analysis library: parsing, per-language rules, metrics, coverage, mutants |
| [`crates/dog-food-cli`](crates/dog-food-cli) | `dgf`: commands, settings, git, mutation runs, reports and dashboards |
| [`crates/dog-food-dup`](crates/dog-food-dup) | duplication: the syntactic engine, and the C# engine (`semantic/`, .NET) |
| [`editors/vscode`](editors/vscode) | the VS Code extension |
| [`docs`](docs) | the guides above |

Building, testing and adding a language: [docs/development.md](docs/development.md). Versions and making a release: [docs/releasing.md](docs/releasing.md). The repository holds itself to its own [dgf.toml](dgf.toml) (warn above 5, fail above 6) in [CI](.github/workflows/ci.yml).

## License

[MIT](LICENSE): free to use, change and share, commercially too. dog-food is maintained in spare time, on a best-effort basis, and comes without any warranty.
