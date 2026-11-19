# What security software sees of a dgf package, before it is published and
# after: for every program file in it, who it says it is, whether it is
# signed, its SHA-256, and what Microsoft Defender makes of it.
#
#   ./scripts/pack-nuget.ps1
#   ./scripts/check-package.ps1                      # the package just packed
#   ./scripts/check-package.ps1 -Version 0.1.4       # a published one, from nuget.org
#   ./scripts/check-package.ps1 -Path C:\some\folder # any folder or .nupkg
#
# -NoScan leaves Defender out (the release workflow, which only asks whether
# the files say what they are).
# Exit code 1: Defender found something, or a file does not say what it is.
param(
    [string]$Path,
    [string]$Version,
    [switch]$NoScan,
    [string]$Rid = "win-" + $(if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64") { "arm64" } else { "x64" })
)
$ErrorActionPreference = "Stop"
$repo = Split-Path $PSScriptRoot -Parent
# A folder of this run's own, removed at the end.
$work = Join-Path ([IO.Path]::GetTempPath()) "dgf-check-package-$PID"
New-Item -ItemType Directory -Force $work | Out-Null

if ($Version) {
    $Path = Join-Path $work "dgf.$Rid.$Version.nupkg"
    # Windows PowerShell 5.1 does not offer TLS 1.2 unless asked.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    Invoke-WebRequest "https://api.nuget.org/v3-flatcontainer/dgf.$Rid/$Version/dgf.$Rid.$Version.nupkg" -OutFile $Path -UseBasicParsing
}
elseif (-not $Path) {
    $Path = Get-ChildItem (Join-Path $repo "packaging/nuget/nupkg") -Filter "dgf.$Rid.*.nupkg" |
        Sort-Object LastWriteTime | Select-Object -Last 1 -ExpandProperty FullName
    if (-not $Path) { throw "no package in packaging/nuget/nupkg: run scripts/pack-nuget.ps1 first, or pass -Path or -Version" }
}
Write-Output "Checking $Path"

$folder = $Path
if (Test-Path $Path -PathType Leaf) {
    $folder = Join-Path $work "files"
    Copy-Item $Path (Join-Path $work "package.zip")
    Expand-Archive (Join-Path $work "package.zip") -DestinationPath $folder
}

# Ours are the files this repository builds; the rest come from Microsoft.
$ours = "dgf.exe", "dgf-dup-cs.dll", "DgfWorker.dll", "DgfMtp.dll", "dgf-launcher.dll"
$anonymous = 0
$files = Get-ChildItem $folder -Recurse -File | Where-Object { $_.Extension -in ".exe", ".dll" }
$report = foreach ($file in $files) {
    $info = $file.VersionInfo
    $signature = Get-AuthenticodeSignature $file.FullName
    $own = $ours -contains $file.Name
    if ($own -and -not ($info.ProductName -and $info.CompanyName -and $info.FileVersion)) { $anonymous++ }
    [pscustomobject]@{
        File    = $file.Name
        Product = $info.ProductName
        Company = $info.CompanyName
        Version = $info.FileVersion
        Signed  = if ($signature.Status -eq "Valid") { $signature.SignerCertificate.Subject -replace '^CN=([^,]+).*', '$1' } else { "no" }
        SHA256  = (Get-FileHash $file.FullName -Algorithm SHA256).Hash.ToLower()
    }
}
$report | Format-Table File, Product, Company, Version, Signed -AutoSize
$report | Where-Object { $ours -contains $_.File } | ForEach-Object { "{0}  {1}" -f $_.SHA256, $_.File }

$dgf = $report | Where-Object File -eq "dgf.exe"
if ($dgf) {
    Write-Output ""
    Write-Output "What other antivirus engines say, once someone has uploaded the file there:"
    Write-Output "  https://www.virustotal.com/gui/file/$($dgf.SHA256)"
}

# Defender: its newest version when it has updated itself, else the one Windows came with.
$platform = Get-ChildItem "$env:ProgramData\Microsoft\Windows Defender\Platform" -Directory -ErrorAction SilentlyContinue |
    Sort-Object Name | Select-Object -Last 1
$scanner = if ($platform) { Join-Path $platform.FullName "MpCmdRun.exe" } else { "$env:ProgramFiles\Windows Defender\MpCmdRun.exe" }
$found = $false
Write-Output ""
if ($NoScan) {
    Write-Output "Not scanned (-NoScan)."
}
elseif (Test-Path $scanner) {
    $signatures = (Get-MpComputerStatus -ErrorAction SilentlyContinue).AntivirusSignatureVersion
    Write-Output "Microsoft Defender (signatures $signatures):"
    $scan = & $scanner -Scan -ScanType 3 -File $folder -DisableRemediation
    $found = $LASTEXITCODE -ne 0
    $scan | Where-Object { $_ -match "found|Threat|Resources|file" } | ForEach-Object { "  $_" }
}
else {
    Write-Output "Microsoft Defender is not on this machine: nothing was scanned."
}

Remove-Item -Recurse -Force $work -ErrorAction SilentlyContinue
if ($anonymous) { Write-Output "$anonymous of our files do not name their product, company and version." }
if ($found -or $anonymous) { exit 1 }
