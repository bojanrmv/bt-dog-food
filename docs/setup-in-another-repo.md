# Set up dog-food (`dgf`) in this repository

You are setting up **dog-food**, a local CLI (`dgf`) that scores every function for cyclomatic and cognitive complexity, and with a coverage report also for CRAP (complex and untested). It reads **C#, TypeScript/JavaScript and Rust**. The steps below only read files: nothing is built, tested or changed. Source and docs: https://github.com/bojanrmv/bt-dog-food (start with `README.md`; every command is in `docs/commands.md`). Follow the steps in order and report back.

## 1. Check `dgf` runs

```powershell
dgf --version
```

If it is not recognized: `dotnet tool install -g dgf` (needs the .NET 10 SDK), then open a new terminal; other ways are in https://github.com/bojanrmv/bt-dog-food/blob/main/docs/install.md. If the repository has no `.cs`, `.ts`, `.tsx`, `.js`, `.jsx` or `.rs` files, stop and say so.

## 2. Install the skill, then use `--format agent`

```powershell
dgf skill install
```

This writes `.claude/skills/dog-food/SKILL.md`, which explains every command and column. Read it now. From here on, use `--format agent` whenever you read dgf output yourself.

## 3. Settings

From the repository root run `dgf init` (unless `dgf.toml` exists), look at the layout, and fill in:

- `[analyze] exclude`: anything not hand-written and not already in `.gitignore`: build output, generated code (EF `Migrations/`, API clients), vendored code, test fixtures. `.gitignore` syntax, relative to `dgf.toml`.
- `[limits]`: keep `warn = 10` and no `fail` unless the user says otherwise.
- `[changes] base`: the main branch, if it is not `main` or `master`.
- `[coverage] paths`: only if the project already produces coverage (step 5).

Run `dgf analyze --top 1` to check the file is valid (unknown keys are an error).

## 4. First analysis

```powershell
dgf analyze --format agent --top 30
dgf analyze --format html -o dgf-report.html
```

Exit code 1 only means a function is above `fail`; the output is complete.

## 5. Coverage (if the project has tests)

Run the tests with coverage the way the project does it, for example:

- .NET: `dotnet test --collect:"XPlat Code Coverage" --results-directory TestResults`
- Jest / Vitest: `npx jest --coverage --coverageReporters=lcov` or `npx vitest run --coverage --coverage.reporter=lcov`

Then `dgf analyze --coverage TestResults --format agent` (or `coverage/lcov.info`). If it works, add the path to `[coverage] paths`. Skip this step if the tests do not run locally.

## 6. Changes and history

```powershell
dgf changes --format agent                  # skip on the main branch with no edits
dgf analyze --save-history                  # first snapshot in .dgf/history/
```

## 7. Keep generated reports out of git

Add to `.gitignore` (but do NOT ignore all of `.dgf/`: `.dgf/history/` and `.dgf/baseline.json` are meant to be committed, only the mutation cache is not):

```
/dgf-*.html
/dgf-*.md
/dgf-*.json
/.dgf/mutation-cache/
```

## 8. Report back

1. Files and functions analyzed, time taken, ok / warn / fail counts.
2. The 10 most complex functions: cyclomatic and cognitive score, file and lines, and what drives each score.
3. With coverage: the functions with the highest CRAP scores.
4. What you excluded and why; any `warning:` lines (files with syntax errors).
5. Suggested `warn` / `fail` limits. If many functions would fail, suggest `dgf baseline` or path overrides rather than refactoring everything.
6. Optional next steps: `dgf hook install` (blocks commits that make code worse), the VS Code extension (`scripts/install-vscode-extension.ps1` in a clone of the dog-food repository).

Do not refactor code, add `dgf-ignore` comments, run `dgf baseline` or commit anything unless the user asks. When asked to commit: `dgf.toml`, the `.gitignore` change, `.claude/skills/dog-food/` and `.dgf/`.
