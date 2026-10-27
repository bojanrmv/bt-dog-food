# Packages editors/vscode as a .vsix and installs it into VS Code.
# Needs Node.js (for npx) and the `code` command. Remove the extension with:
#   code --uninstall-extension dog-food.dog-food
$ErrorActionPreference = "Stop"
$extension = Join-Path $PSScriptRoot "..\editors\vscode"
$vsix = Join-Path $env:TEMP "dog-food-vscode.vsix"

Push-Location $extension
try {
    npx --yes @vscode/vsce@3 package --no-dependencies --allow-missing-repository --skip-license -o $vsix
    if ($LASTEXITCODE -ne 0) { throw "packaging failed" }
} finally {
    Pop-Location
}
code --install-extension $vsix --force
Write-Host "Installed. Reload VS Code (Ctrl+Shift+P, 'Developer: Reload Window') to start it."
