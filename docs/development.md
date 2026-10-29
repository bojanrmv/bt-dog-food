# Development

## Build and test

```powershell
cargo test                       # unit, fixture and doc tests, plus end-to-end runs on small .NET projects
                                 # (needs a .NET 10 SDK; DGF_SKIP_E2E=1 skips them)
cargo clippy --all-targets       # lints (CI uses -D warnings)
cargo fmt
cargo run -p dog-food-cli -- analyze
```

Tools for working on the analysis:

```powershell
cargo run -p dog-food-core --example dump -- File.cs          # a file's syntax tree
cargo run --release -p dog-food-core --example bench -- <dir>  # parsing speed
cargo run -p dog-food-core --example schemata -- File.cs      # a file with every mutant switched in
cargo run -p dog-food-dup --bin dgf-dup-bench                 # duplication scores on the labelled corpus
```

The repository checks itself with its own [dgf.toml](../dgf.toml) (warn above 5, fail above 6), in CI ([.github/workflows/ci.yml](../.github/workflows/ci.yml)) and through `dgf changes` before merging.

## Layout

| Path | What it is |
| --- | --- |
| `crates/dog-food-core` | the library: walker (`src/walker.rs`), one rule file per language (`src/lang/`), data model, coverage reading, mutants |
| `crates/dog-food-cli` | `dgf`: settings, discovery, limits, git, baseline, history, mutation runs, reports (`src/report/`: HTML in `*.html`, `style.css`, `common.js`) |
| `crates/dog-food-dup` | duplication: the syntactic engine (`src/`) and the C# engine (`semantic/`, .NET) |
| `crates/dog-food-cli/assets/SKILL.md` | the Claude Code skill `dgf skill install` writes |
| `crates/dog-food-core/tests/fixtures` | sample files; `// dgf: 7 cognitive=9 nesting=3 params=2` states what the next function must score |
| `crates/dog-food-core/tests/mutants` | C# and TypeScript files; `// mutant: relational > -> >=` states each mutant the next line must get |
| `crates/dog-food-cli/tests/e2e/dotnet` | a small xUnit solution that the end-to-end test mutates |
| `crates/dog-food-dup/tests/corpus` | the labelled duplication corpus, and hand-labelled scans of real code |
| `editors/vscode` | the VS Code extension |

Design and decisions: [architecture](architecture.md).

## Adding a language or a rule

1. `cargo run -p dog-food-core --example dump -- sample.ext` shows the node names tree-sitter uses.
2. Add the grammar crate and implement `LanguageAnalyzer` in `crates/dog-food-core/src/lang/` (`function_kind`, `decision`, `nests`, `name`); map the extensions in `Language::from_path`.
3. Add a unit test per rule and a fixture file with `// dgf:` markers.
4. Run `dgf analyze` here: the new code has to pass the same limits.
