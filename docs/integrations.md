# Integrations

## VS Code

`editors/vscode` is a small extension (plain JavaScript, no dependencies) that runs `dgf` on save:

- **scores above each function**: `complexity 7 · cognitive 9 · warn`, plus coverage and CRAP when `dgf.toml` lists coverage reports;
- **Problems panel** entries for functions above `warn` (warning) or `fail` (error), with the decision breakdown;
- **status bar**: the most complex function in the open file;
- **commands** (Ctrl+Shift+P, "dog-food"): open the workspace, changes or history report in the browser.

Install from this repository:

```powershell
powershell -ExecutionPolicy Bypass -File scripts/install-vscode-extension.ps1
```

It packages the folder with `vsce` (through `npx`) and runs `code --install-extension`. Reload VS Code afterwards. Remove it with `code --uninstall-extension dog-food.dog-food`. Settings: `dogFood.path` (the `dgf` executable), `dogFood.codeLens`, `dogFood.codeLensMinimum`.

The extension uses the workspace folder's `dgf.toml`, so editor, CLI and CI agree.

## AI assistants

### `--format agent`

Made for a language model reading the output: a one-line verdict, the limits, a column legend, then one line per function, grouped by what needs attention. No tables, colors or padding.

```
dgf analyze: FAIL. 2 files, 2 functions, 7 ms. fail=1 warn=1 ok=0 ignored=0
limits: cyclomatic warn above 5, fail above 10
limits: CRAP warn above 30, no fail limit
columns: status cyclomatic cognitive nesting lines params coverage% crap location name | decisions

## above limits (2)
FAIL 11 15 3 15 3 0 132.0 src/Shop/Pricing.cs:20-34 Pricing.Shipping | &&=1 ?:=3 if=5 ||=1
WARN 8 5 1 14 3 91 8.0 src/Shop/Pricing.cs:5-18 Pricing.Discount | &&=1 case=3 if=3

## most complex within limits (top 0 of 0)
```

`dgf changes --format agent` lists what can fail the run, then other score changes as `change before->after status location name | decisions`, and `dgf history --format agent` gives the trend in one line followed by runs and movers. Every function is above-limit first, so an agent can stop reading early; `--top` bounds the rest.

### Claude Code skill

```powershell
dgf skill install            # .claude/skills/dog-food/SKILL.md in this repository
dgf skill install --global   # ~/.claude/skills, for every project
dgf skill show               # print it
```

The skill tells Claude Code when to use `dgf` (complexity questions, refactoring, before finishing a change in a repo with `dgf.toml`), which commands to run with `--format agent`, how to read every column, and how to reduce complexity without gaming the score: guard clauses, extracting helpers, lookup tables, verifying with `dgf changes` and the tests, and never adding `dgf-ignore` or raising limits without asking. The text is [crates/dog-food-cli/assets/SKILL.md](../crates/dog-food-cli/assets/SKILL.md); it is compiled into `dgf`, so the installed skill always matches the installed tool.

Other agents (Copilot, Cursor, Codex) can be pointed at the same file or at `dgf --help` and `--format agent`.

## Rust library

The analysis is the `dog-food-core` crate; `dgf` is a wrapper around it.

```toml
[dependencies]
dog-food-core = { git = "https://github.com/bojanrmv/bt-dog-food" }
```

```rust
use dog_food_core::{analyze_file, analyze_source, Language};

let functions = analyze_source("int Abs(int x) => x < 0 ? -x : x;", Language::CSharp)?;
let f = &functions[0];
println!("{}: complexity {}, cognitive {}, {} params", f.name, f.complexity, f.cognitive, f.parameters);

let report = analyze_file(std::path::Path::new("src/Orders/OrderService.cs"))?;
```

| API | Returns |
| --- | --- |
| `analyze_source(&str, Language)` | `Vec<FunctionMetrics>` |
| `analyze_file(&Path)` | `FileReport { path, language, functions, has_errors }` |
| `Language::from_path(&Path)` | `Some(Language)` for `.cs`, `.ts`/`.tsx`/`.js`/`.jsx`/`.mts`/`.cts`/`.mjs`/`.cjs`, `.rs` |
| `FunctionMetrics` | `name`, `kind`, `start_line`, `end_line`, `complexity`, `breakdown`, `cognitive`, `max_nesting`, `lines`, `parameters`, `ignored` |

A file with syntax errors still returns results, with `has_errors` set. Discovery, settings, limits, coverage, git and reports live in the CLI crate.
