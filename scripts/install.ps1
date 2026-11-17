# REQUIEM installer for Windows
# Builds the release binary and installs it to %LOCALAPPDATA%\bin\requiem.exe
# Adds that directory to the user PATH if it is not already there.

$ErrorActionPreference = "Stop"
$ScriptDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$RootDir   = Split-Path -Parent $ScriptDir

Write-Host "REQUIEM — Windows installer" -ForegroundColor Cyan
Write-Host "  project root: $RootDir" -ForegroundColor DarkGray
Write-Host ""

# ── 0. Prerequisites ─────────────────────────────────────────────────
if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) {
    throw "cargo not found on PATH — install Rust from https://rustup.rs first"
}

# ── 1. Build release ─────────────────────────────────────────────────
Write-Host "Building release binary…"
Push-Location $RootDir
try {
    $cargoBuild = & cargo build --release 2>&1
    if ($LASTEXITCODE -ne 0) {
        Write-Host $cargoBuild
        throw "cargo build failed"
    }
} finally {
    Pop-Location
}
Write-Host "  ✓ build complete" -ForegroundColor Green

# ── 2. Locate the built binary ───────────────────────────────────────
$Target = Join-Path $RootDir "target\release\requiem.exe"
if (-not (Test-Path $Target)) {
    throw "Cannot find $Target — did the build succeed?"
}

# ── 3. Install to %LOCALAPPDATA%\bin ─────────────────────────────────
$InstallDir = Join-Path $env:LOCALAPPDATA "bin"
if (-not (Test-Path $InstallDir)) {
    New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
}
$InstallPath = Join-Path $InstallDir "requiem.exe"
Copy-Item -Path $Target -Destination $InstallPath -Force
Write-Host "  ✓ installed to $InstallPath" -ForegroundColor Green

# ── 4. Verify the installed binary actually runs ─────────────────────
$versionOut = & $InstallPath --version 2>&1
if ($LASTEXITCODE -ne 0 -or -not $versionOut) {
    throw "Installed binary failed '--version' — installation is broken"
}
Write-Host "  ✓ verified: $versionOut" -ForegroundColor Green

# ── 5. Add to user PATH ───────────────────────────────────────────────
$userPath = [Environment]::GetEnvironmentVariable("Path", "User")
$pathParts = $userPath -split ";" | Where-Object { $_ -ne "" }
$alreadyOnPath = $pathParts -contains $InstallDir

if ($alreadyOnPath) {
    Write-Host "  ✓ $InstallDir already on user PATH" -ForegroundColor Green
} else {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$InstallDir", "User")
    Write-Host "  ✓ added $InstallDir to user PATH (open a new terminal to activate)" -ForegroundColor Green
}

Write-Host ""
Write-Host "Done. Run 'requiem setup' in a new terminal to configure your model endpoint." -ForegroundColor Cyan
