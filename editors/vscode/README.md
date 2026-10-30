# dog-food for VS Code

Shows the complexity of every function while you work on C#, TypeScript/JavaScript and Rust:

- **Scores above each function** (CodeLens): `complexity 7 · cognitive 9 · warn`, plus coverage and CRAP when `dgf.toml` lists coverage reports.
- **Problems** for functions above the `warn` (warning) or `fail` (error) limits in your `dgf.toml`.
- **Status bar**: the most complex function in the open file; click it for the workspace report.
- **Commands** (Ctrl+Shift+P, "dog-food"): open the workspace, changes (this branch vs main) or history report in the browser, or re-analyze the current file.

Files are analyzed when opened and saved. The extension runs the `dgf` command line tool, which must be installed (`dotnet tool install -g dgf`, or see [install](../../docs/install.md)) or configured with the `dogFood.path` setting.

## Install

From the dog-food repository: `powershell -File scripts/install-vscode-extension.ps1`, which packages this folder and runs `code --install-extension`. Remove it with `code --uninstall-extension dog-food.dog-food`.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `dogFood.path` | `dgf` | The dgf executable |
| `dogFood.codeLens` | `true` | Show scores above functions |
| `dogFood.codeLensMinimum` | `1` | Only show scores for functions with at least this complexity |
