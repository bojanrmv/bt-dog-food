# dog-food for VS Code

Runs `dgf` from the editor, for C#, TypeScript/JavaScript and Rust:

- **Side bar** (the dog-food icon): run complexity, changes, duplication or mutation tests, and see what each found. A click on a finding opens the code. **Scope** chooses what they look at: the whole workspace, or files and folders.
- **Dashboard**: `dgf`'s report pages in an editor tab, one tab per analysis. A click on a location opens the code.
- **Explorer**: right-click files or folders, then **dog-food**, to analyze just those.
- **In the editor**: above each function, its scores and where its code is duplicated (a click opens the copy); Problems for functions above the limits in `dgf.toml`; and the file's most complex function in the status bar.

Mutation tests of the whole workspace test only the functions the branch changed; choose files or folders to test all of theirs. A running analysis has a stop button on its row.

The extension runs the `dgf` command line tool, which must be installed (`dotnet tool install -g dgf`, or see [install](../../docs/install.md)) or configured with the `dogFood.path` setting. It uses the workspace folder's `dgf.toml`, so editor, CLI and CI agree.

## Install

```powershell
dgf vscode      # writes the extension as a .vsix file and prints the command that installs it
```

Run the printed `code --install-extension` command, then reload VS Code. The extension is built into `dgf` and has its version, so the two always match. Remove it with `code --uninstall-extension dog-food.dog-food`.

When working on the extension, `./scripts/install-vscode-extension.ps1` does both steps with the `dgf` it builds from the repository.

## Settings

| Setting | Default | |
| --- | --- | --- |
| `dogFood.path` | `dgf` | The dgf executable |
| `dogFood.codeLens` | `true` | Show scores above functions |
| `dogFood.codeLensMinimum` | `1` | Only show scores for functions with at least this complexity |
| `dogFood.duplicationLens` | `true` | Show where a function's code is duplicated. Copies are looked for in the whole workspace folder, when it is opened and when a source file changes |

## Tests

`node --test` in this folder.
