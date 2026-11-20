# For work on the extension: builds dgf from this repository, has it write
# the VS Code extension (`dgf vscode`) and installs that into VS Code.
# Needs the `code` command. Remove the extension with:
#   code --uninstall-extension dog-food.dog-food
$ErrorActionPreference = "Stop"
$vsix = Join-Path ([IO.Path]::GetTempPath()) "dog-food-vscode.vsix"

cargo run --quiet --manifest-path (Join-Path $PSScriptRoot "../Cargo.toml") -p dog-food-cli -- vscode --output $vsix
if ($LASTEXITCODE -ne 0) { throw "writing the extension failed" }
code --install-extension $vsix --force
if ($LASTEXITCODE -ne 0) { throw "installing failed" }
