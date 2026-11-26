# The release notes of a version: its section in CHANGELOG.md, printed.
#
#   ./scripts/release-notes.ps1                  # the version in Cargo.toml
#   ./scripts/release-notes.ps1 -Version 0.2.0
#
# A prerelease (0.3.0-rc.1) has the notes of the version it leads to (0.3.0).
# Exit code 1: CHANGELOG.md has no section for the version, or an empty one.
# CI and the release workflow run this, so no version is released without notes.
param([string]$Version)
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent

if (-not $Version) {
    $found = Select-String -Path (Join-Path $repo "Cargo.toml") -Pattern '^version = "(.*)"' | Select-Object -First 1
    if (-not $found) { throw "no version in Cargo.toml" }
    $Version = $found.Matches[0].Groups[1].Value
}
$release = ($Version -split "-")[0]
$heading = "## $release"

# The lines from the version's heading to the next version's.
$notes = @()
$inside = $false
foreach ($line in Get-Content (Join-Path $repo "CHANGELOG.md") -Encoding UTF8) {
    if ($line.StartsWith("## ")) {
        if ($inside) { break }
        $inside = $line.TrimEnd() -eq $heading -or $line.StartsWith("$heading ")
    }
    elseif ($inside) { $notes += $line }
}

$text = ($notes -join "`n").Trim()
if (-not $text) {
    [Console]::Error.WriteLine("CHANGELOG.md has no notes for ${release}: add a `"$heading`" section that says what changed for users")
    exit 1
}
Write-Output $text
