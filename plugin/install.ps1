# Builds the Rust plugin and installs it into OBS (C:\ProgramData\obs-studio\plugins).
# Backs up the installed DLL before replacing it. OBS must be closed.
param(
    [string]$PluginDir = "C:\ProgramData\obs-studio\plugins\obs-multi-rtmp",
    [string]$BackupDir = "$PSScriptRoot\..\install-backups"
)
$ErrorActionPreference = "Stop"
if (Get-Process obs64 -ErrorAction SilentlyContinue) { throw "OBS is running: close it before installing." }

Push-Location $PSScriptRoot
try { cargo build --release -p obs-multi-rtmp; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location }

$target = Join-Path $PSScriptRoot "..\target\release"
$bin = Join-Path $PluginDir "bin\64bit"
$locale = Join-Path $PluginDir "data\locale"
New-Item -ItemType Directory -Force $bin, $locale, $BackupDir | Out-Null

$installed = Join-Path $bin "obs-multi-rtmp.dll"
if (Test-Path $installed) {
    $hash = (Get-FileHash $installed -Algorithm SHA256).Hash.Substring(0, 8)
    $backup = Join-Path $BackupDir "obs-multi-rtmp.previous.$hash.dll.bak"
    Copy-Item $installed $backup
    Write-Host "Backup of the previous DLL: $backup"
}
Copy-Item (Join-Path $target "obs_multi_rtmp.dll") $installed -Force
Copy-Item (Join-Path $target "obs_multi_rtmp.pdb") (Join-Path $bin "obs-multi-rtmp.pdb") -Force
Copy-Item (Join-Path $PSScriptRoot "data\locale\*.ini") $locale -Force
$new = (Get-FileHash $installed -Algorithm SHA256).Hash
Write-Host "Installed obs-multi-rtmp.dll (Rust) SHA256 $new"
