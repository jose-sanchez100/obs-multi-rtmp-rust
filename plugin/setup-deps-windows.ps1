# Prepares the OBS development files for building the plugin on Windows, the same way
# the original plugin's buildspec does: downloads the OBS sources, obs-deps and Qt 6
# (SHA-256 checked) into .deps-windows\, builds libobs + obs-frontend-api from the OBS
# sources and installs their headers and import libraries there.
# Requires Visual Studio 2022 or newer (C++ workload) and CMake.
#
# build.rs uses .deps-windows\ automatically when it exists (OBS_DEPS_DIR overrides it).
param(
    [string]$DepsDir = ""
)
$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"

$ObsVersion = "33.0.1"
$ObsSha256 = "0f228b960c099c6f6bca9f09a0c3516f14fdf1dd96e1d4702145c5d3ba8cba31"
$DepsVersion = "2026-10-23"
$DepsSha256 = "7508972a867a0ef68c26cd8bdba61fd999edd0a92fa9eb9585774de9bf51bfbe"
$QtSha256 = "78cd7a138464bb6edcdd3b85fcd63a5a7c69e369629963f3bc395bab74fb3f47"

$root = (Resolve-Path (Join-Path $PSScriptRoot "..")).ProviderPath
$deps = if ($DepsDir) { $DepsDir } else { Join-Path $root ".deps-windows" }
$ready = Join-Path $deps ".ready-$ObsVersion-$DepsVersion"
if (Test-Path $ready) {
    Write-Host "OBS development files already prepared in $deps"
    exit 0
}
New-Item -ItemType Directory -Force $deps | Out-Null
# Windows' bsdtar extracts zip archives much faster than Expand-Archive.
$tar = Join-Path $env:SystemRoot "System32\tar.exe"

function Get-Checked([string]$Url, [string]$File, [string]$Sha256) {
    $path = Join-Path $deps $File
    if (-not (Test-Path $path)) {
        Write-Host "Downloading $Url"
        Invoke-WebRequest -Uri $Url -OutFile "$path.part"
        Move-Item "$path.part" $path
    }
    $actual = (Get-FileHash -Algorithm SHA256 $path).Hash.ToLowerInvariant()
    if ($actual -ne $Sha256) { throw "SHA-256 mismatch for ${File}: $actual" }
    $path
}

function Expand-Into([string]$Archive, [string]$Destination) {
    $dir = Join-Path $deps $Destination
    if (-not (Test-Path $dir)) {
        New-Item -ItemType Directory -Force $dir | Out-Null
        & $tar -xf $Archive -C $dir
        if ($LASTEXITCODE) { throw "extracting $Archive failed" }
    }
    $dir
}

$base = "https://github.com/obsproject/obs-deps/releases/download/$DepsVersion"
$obsDeps = Expand-Into (Get-Checked "$base/windows-deps-$DepsVersion-x64.zip" "windows-deps-$DepsVersion-x64.zip" $DepsSha256) "obs-deps-$DepsVersion-x64"
$qt = Expand-Into (Get-Checked "$base/windows-deps-qt6-$DepsVersion-x64.zip" "windows-deps-qt6-$DepsVersion-x64.zip" $QtSha256) "obs-deps-qt6-$DepsVersion-x64"
# The source archive holds a single obs-studio-<version>\ folder.
$obsZip = Get-Checked "https://github.com/obsproject/obs-studio/archive/refs/tags/$ObsVersion.zip" "obs-studio-$ObsVersion.zip" $ObsSha256
$src = Join-Path $deps "obs-studio-$ObsVersion"
if (-not (Test-Path $src)) {
    & $tar -xf $obsZip -C $deps
    if ($LASTEXITCODE) { throw "extracting $obsZip failed" }
}

# libobs and obs-frontend-api from the OBS sources (same options as the original plugin).
$build = Join-Path $src "build_x64"
cmake -S $src -B $build -A x64 `
    -DOBS_CMAKE_VERSION=3.0.0 -DENABLE_PLUGINS=OFF -DENABLE_FRONTEND=OFF -DENABLE_SCRIPTING=OFF `
    "-DOBS_VERSION_OVERRIDE=$ObsVersion" "-DCMAKE_PREFIX_PATH=$obsDeps;$qt"
if ($LASTEXITCODE) { throw "configuring OBS failed" }
cmake --build $build --target obs-frontend-api --config Release --parallel
if ($LASTEXITCODE) { throw "building OBS failed" }
cmake --install $build --component Development --config Release --prefix $deps
if ($LASTEXITCODE) { throw "installing the OBS development files failed" }

foreach ($f in "obs-module.h", "obs-frontend-api.h") {
    if (-not ((Test-Path (Join-Path $deps "include\$f")) -or (Test-Path (Join-Path $deps "include\obs\$f")))) { throw "missing $f in $deps\include" }
}
foreach ($f in "lib\obs.lib", "lib\obs-frontend-api.lib") {
    if (-not (Test-Path (Join-Path $deps $f))) { throw "missing $f in $deps" }
}
# The build tree and downloads are no longer needed (and would bloat the CI cache).
Remove-Item $build -Recurse -Force
Remove-Item (Join-Path $deps "*.zip")
New-Item -ItemType File -Force $ready | Out-Null
Write-Host "OBS development files ready in $deps"
