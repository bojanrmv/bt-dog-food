# Packs the `dgf` .NET tool for this machine's platform only, to try the
# package before a release (the release workflow packs every platform):
#
#   ./scripts/pack-nuget.ps1
#   dotnet tool install -g dgf --add-source packaging/nuget/nupkg --version <version>
#
# The version is the one in Cargo.toml.
param(
    [string]$Rid = "win-" + $(if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" })
)
$repo = Split-Path $PSScriptRoot -Parent
$package = Join-Path $repo "packaging/nuget"
$version = (Select-String -Path (Join-Path $repo "Cargo.toml") -Pattern '^version = "(.+)"').Matches[0].Groups[1].Value

cargo build --release --locked -p dog-food-cli
if ($LASTEXITCODE) { exit $LASTEXITCODE }

foreach ($dir in "native", "engine", "nupkg") {
    Remove-Item -Recurse -Force (Join-Path $package $dir) -ErrorAction SilentlyContinue
}
$native = New-Item -ItemType Directory -Force (Join-Path $package "native/$Rid")
Copy-Item (Join-Path $repo "target/release/dgf.exe") $native -ErrorAction Stop

dotnet publish (Join-Path $repo "crates/dog-food-dup/semantic") -c Release -o (Join-Path $package "engine") -p:SatelliteResourceLanguages=en -p:UseAppHost=false
if ($LASTEXITCODE) { exit $LASTEXITCODE }

dotnet pack $package -c Release "-p:Version=$version" "-p:ToolPackageRuntimeIdentifiers=$Rid"
if ($LASTEXITCODE) { exit $LASTEXITCODE }
Get-ChildItem (Join-Path $package "nupkg") | Select-Object Name, Length
