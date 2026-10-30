# Workflows

## Changes

`dgf changes` compares your branch, including uncommitted edits and new files, with the commit where it left `main`. Only changed files are analyzed, and every touched function is paired with its version in the base:

```
$ dgf changes
Compared with main (merge base b02a022), changed lines in 20 changed source files

CHANGE       SCORE  STATUS  FUNCTION              LOCATION
new          – → 6  warn    member_owner          crates/dog-food-core/src/lang/csharp.rs:148-174
worse        4 → 5  ok      Language::from_path   crates/dog-food-core/src/lang/mod.rs:37-46
better       4 → 3  ok      analyze               crates/dog-food-cli/src/main.rs:168-204
removed      5 → –  –       print_summary         crates/dog-food-cli/src/main.rs:307-335

324 functions: 295 new · 3 worse · 2 better · 13 same · 11 removed · net +410
result: Passed with warnings. 1 new or worse function is above the warn limit (5).
```

- **Scope**: `--scope lines` (default) reports functions that contain a changed line; `--scope files` every function in a changed file.
- **Fair failing**: by default only *new* functions and functions whose score *went up* can fail (`--fail-on new-or-worse`). Old debt you only touched does not block you. `--fail-on touched` counts every reported function.
- **Matching**: by name, with overloads and lambdas matched in order, so moving code does not count as a change. A renamed function shows as removed plus new.
- **Base**: `--base <branch or commit>`, or `[changes] base` in `dgf.toml`, else `main`, else `master`.
- **Formats**: `markdown` for a PR comment, `html -o dgf-changes.html` for a before-and-after chart (worse functions first), `sarif` for annotations, `agent` for AI assistants.

## Pre-commit hook

```powershell
dgf hook install      # writes .git/hooks/pre-commit (honors core.hooksPath)
dgf hook uninstall
```

The hook runs `dgf changes --staged`: it compares exactly what you are committing with `HEAD` and blocks the commit when a new or worse function is above the `fail` limit. It takes milliseconds because only staged files are analyzed. Skip it once with `git commit --no-verify`.

`dgf` refuses to overwrite a pre-commit hook it did not write; add `exec dgf changes --staged` to yours, or pass `--force`.

## Adopting in existing code

Three tools, from blunt to precise:

1. **Path overrides**: looser limits for legacy folders.

   ```toml
   [[limits.overrides]]
   paths = ["src/Legacy/"]
   fail = 25
   ```

2. **Baseline**: accept every function that is over a limit *today*.

   ```powershell
   dgf baseline          # writes .dgf/baseline.json; commit it
   ```

   Later runs mark those functions `ignored` as long as their score does not go up. When one gets worse, or a new function crosses a limit, it fails as usual. Re-run `dgf baseline` after paying debt down to tighten it. `--no-baseline` shows the full picture.

3. **`dgf-ignore`**: excuse one function, with a reason, right above it (attributes and decorators may sit in between):

   ```csharp
   // dgf-ignore: generated state machine, see #142
   [GeneratedCode("parser")]
   public Token Next() { ... }
   ```

   ```rust
   // dgf-ignore: one match arm per subcommand; splitting it would only hide the list
   fn main() -> ExitCode { ... }
   ```

   The comment must start with `dgf-ignore`. Ignored functions stay in every report with their reason, so they remain visible.

## History

```powershell
dgf analyze --save-history                       # snapshot to .dgf/history/<time>_<commit>.json
dgf history                                      # runs and biggest movers
dgf history --format html -o dgf-history.html    # trends page
```

A snapshot holds the time, commit, branch, summary numbers and every function's score (a few KB). Commit `.dgf/history/` so history travels with the code, and save snapshots from one place, for example on `main` after each merge or on a schedule. The page shows trend lines (functions above the limits, highest and median score, function count, total score), the functions that got more complex or simpler since the first run, and every run.

## CI

A job that checks a repository with the released `dgf`:

```yaml
- uses: actions/checkout@v4
  with: { fetch-depth: 0 }                       # dgf changes needs the base branch
- uses: actions/setup-dotnet@v4
  with: { dotnet-version: 10.0.x }
- run: dotnet tool install -g dgf --version 0.0.3   # pin it: a new release cannot change the gate
- run: dgf analyze --format markdown >> "$GITHUB_STEP_SUMMARY"          # fails above the limits
- run: dgf changes --base "origin/${{ github.base_ref }}" --format markdown >> "$GITHUB_STEP_SUMMARY"
  if: github.event_name == 'pull_request'
- run: dgf analyze --format sarif -o dgf.sarif
- uses: github/codeql-action/upload-sarif@v3      # functions above a limit as PR annotations
  with: { sarif_file: dgf.sarif, category: dog-food }
```

This repository's own workflow, [.github/workflows/ci.yml](../.github/workflows/ci.yml), does the same with `dgf` built from its source. In a repository with a [tool manifest](install.md#as-a-net-tool), `dotnet tool restore` installs the pinned version instead.

The exit code does the gating: 1 when something is above `fail`. `-o` writes a file and still sets the exit code. Upload `dgf-report.html` as an artifact to keep the full picture per build.

Other CI systems: Azure DevOps and GitLab read SARIF too, and the Markdown works anywhere that renders it.
