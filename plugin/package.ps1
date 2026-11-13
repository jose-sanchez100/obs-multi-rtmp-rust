# Builds the release plugin and packages it for distribution, the same way the
# original plugin's Package-Windows.ps1 does:
#   dist/obs-multi-rtmp-rust-<version>-windows-x64.zip            (manual install)
#   dist/obs-multi-rtmp-rust-<version>-windows-x64-Installer.exe  (NSIS installer)
#   dist/obs-multi-rtmp-rust-<version>-windows-x64-pdb.zip        (debug symbols)
# The installer is skipped with a warning if makensis.exe is not found.
param(
    [string]$MakeNsis = ""
)
$ErrorActionPreference = "Stop"

$root = Resolve-Path (Join-Path $PSScriptRoot "..")
$version = (Select-String -Path (Join-Path $PSScriptRoot "Cargo.toml") -Pattern '^version\s*=\s*"([^"]+)"').Matches[0].Groups[1].Value
$name = "obs-multi-rtmp-rust-$version-windows-x64"
$dist = Join-Path $root "dist"
# Staging trees live under target/ so dist/ only holds the deliverables.
$stageRoot = Join-Path $root "target\package"
$stage = Join-Path $stageRoot "obs-multi-rtmp"

Push-Location $PSScriptRoot
try { cargo build --release -p obs-multi-rtmp; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location }
$target = Join-Path $root "target\release"

# Staging tree = the layout under %ALLUSERSPROFILE%\obs-studio\plugins\obs-multi-rtmp.
if (Test-Path $stageRoot) { Remove-Item $stageRoot -Recurse -Force }
New-Item -ItemType Directory -Force $dist | Out-Null
Remove-Item (Join-Path $dist "obs-multi-rtmp-rust-*-windows-x64*") -ErrorAction SilentlyContinue
$bin = Join-Path $stage "bin\64bit"
$locale = Join-Path $stage "data\locale"
New-Item -ItemType Directory -Force $bin, $locale | Out-Null
Copy-Item (Join-Path $target "obs_multi_rtmp.dll") (Join-Path $bin "obs-multi-rtmp.dll")
Copy-Item (Join-Path $PSScriptRoot "data\locale\*.ini") $locale

Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
function New-Zip([string]$Path, [hashtable]$Dirs) {
    $zip = [System.IO.Compression.ZipFile]::Open($Path, [System.IO.Compression.ZipArchiveMode]::Create)
    try {
        foreach ($dest in $Dirs.Keys) {
            $src = (Resolve-Path $Dirs[$dest]).ProviderPath
            foreach ($f in Get-ChildItem $src -Recurse -File) {
                $rel = $f.FullName.Substring($src.Length).TrimStart('\') -replace '\\', '/'
                [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
                    $zip, $f.FullName, "$dest/$rel", [System.IO.Compression.CompressionLevel]::Optimal)
            }
        }
    } finally { $zip.Dispose() }
}

# Zip with the OBS install-folder layout (same as the original plugin's zip).
$zipPath = Join-Path $dist "$name.zip"
New-Zip $zipPath @{ "obs-plugins/64bit" = $bin; "data/obs-plugins/obs-multi-rtmp" = (Join-Path $stage "data") }
Write-Host "Created $zipPath"

$pdbDir = Join-Path $stageRoot "pdb"
New-Item -ItemType Directory -Force $pdbDir | Out-Null
Copy-Item (Join-Path $target "obs_multi_rtmp.pdb") (Join-Path $pdbDir "obs-multi-rtmp.pdb")
$pdbZip = Join-Path $dist "$name-pdb.zip"
New-Zip $pdbZip @{ "obs-plugins/64bit" = $pdbDir }
Write-Host "Created $pdbZip"

if (-not $MakeNsis) {
    $cmd = Get-Command makensis.exe -ErrorAction SilentlyContinue
    $MakeNsis = if ($cmd) { $cmd.Source } else {
        # Standard NSIS install, or the copy the Tauri bundler downloads.
        @("${env:ProgramFiles(x86)}\NSIS\makensis.exe", "$env:ProgramFiles\NSIS\makensis.exe", "$env:LOCALAPPDATA\tauri\NSIS\makensis.exe") |
            Where-Object { Test-Path $_ } | Select-Object -First 1
    }
}
if (-not $MakeNsis) {
    Write-Warning "makensis.exe not found: installer skipped. Install NSIS (https://nsis.sourceforge.io) or pass -MakeNsis <path>."
    exit 0
}
$exe = Join-Path $dist "$name-Installer.exe"
& $MakeNsis /V2 "/DPLUGIN_SOURCE_DIR=$stage" "/DOUT_FILE=$exe" "/DVERSION=$version" (Join-Path $root "installer.nsi")
if ($LASTEXITCODE) { throw "makensis failed" }
Write-Host "Created $exe"
