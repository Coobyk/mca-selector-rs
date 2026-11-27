param(
    [string]$Binary = "target/release/mca-selector.exe",
    [string]$OutDir = "dist/windows"
)

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
if (-not [System.IO.Path]::IsPathRooted($Binary)) { $Binary = Join-Path $root $Binary }
if (-not [System.IO.Path]::IsPathRooted($OutDir)) { $OutDir = Join-Path $root $OutDir }
if (-not (Test-Path $Binary)) { throw "missing executable: $Binary" }

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$stage = Join-Path ([System.IO.Path]::GetTempPath()) ("mca-selector-package-" + [System.IO.Path]::GetRandomFileName())
try {
    $packageRoot = Join-Path $stage "mca-selector-windows-x86_64"
    New-Item -ItemType Directory -Force -Path $packageRoot | Out-Null
    Copy-Item $Binary (Join-Path $packageRoot "mca-selector.exe")
    Copy-Item (Join-Path $root "README.md") $packageRoot
    Copy-Item (Join-Path $root "LICENSE") $packageRoot

    $zipPath = Join-Path $OutDir "mca-selector-windows-x86_64-portable.zip"
    if (Test-Path $zipPath) { Remove-Item $zipPath -Force }
    Compress-Archive -Path $packageRoot -DestinationPath $zipPath
} finally {
    Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
}
