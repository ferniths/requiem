# REQUIEM uninstaller for Windows
# Removes the installed binary from %LOCALAPPDATA%\bin and optionally
# removes the user's PATH entry and the config directory.

$ErrorActionPreference = "Stop"

Write-Host "REQUIEM — Windows uninstaller" -ForegroundColor Cyan

$InstallDir = Join-Path $env:LOCALAPPDATA "bin"
$InstallPath = Join-Path $InstallDir "requiem.exe"

if (Test-Path $InstallPath) {
    Remove-Item -Path $InstallPath -Force
    Write-Host "  ✓ removed $InstallPath" -ForegroundColor Green
} else {
    Write-Host "  · $InstallPath not found — nothing to remove" -ForegroundColor DarkGray
}

# Ask whether to also purge the config directory (~/.requiem).
Write-Host ""
Write-Host "Also remove your configuration and sessions (~\.requiem)?" -ForegroundColor Yellow
Write-Host "  [ Y/n ]" -ForegroundColor DarkGray
$ans = Read-Host
if ($ans -eq "" -or $ans -match "^[yY]") {
    $ConfigDir = Join-Path $env:USERPROFILE ".requiem"
    if (Test-Path $ConfigDir) {
        Remove-Item -Path $ConfigDir -Recurse -Force
        Write-Host "  ✓ removed $ConfigDir" -ForegroundColor Green
    } else {
        Write-Host "  · $ConfigDir not found" -ForegroundColor DarkGray
    }
} else {
    Write-Host "  · config kept" -ForegroundColor DarkGray
}

# Note: the user PATH entry added by install.ps1 is left intact so other
# tools in %LOCALAPPDATA%\bin keep working. Remove it manually if desired.
Write-Host ""
Write-Host "Done." -ForegroundColor Cyan
