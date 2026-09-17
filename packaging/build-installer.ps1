#Requires -Version 5.1
<#
.SYNOPSIS
  Build Sway release and create Windows installer with Inno Setup.

.DESCRIPTION
  1. cargo build --release
  2. Compile packaging\sway.iss with ISCC
  3. Output: dist\Sway-<version>-setup.exe

.PARAMETER SkipBuild
  Skip cargo build; use existing target\release\sway.exe

.PARAMETER IsccPath
  Full path to ISCC.exe (auto-detect if omitted)

.EXAMPLE
  .\packaging\build-installer.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipBuild,
    [string]$IsccPath = ""
)

$ErrorActionPreference = "Stop"

$RepoRoot = Resolve-Path (Join-Path $PSScriptRoot "..")
$IssFile = Join-Path $PSScriptRoot "sway.iss"
$ReleaseDir = Join-Path $RepoRoot "target\release"
$ExePath = Join-Path $ReleaseDir "sway.exe"
$DistDir = Join-Path $RepoRoot "dist"

function Get-AppVersion {
    $toml = Get-Content (Join-Path $RepoRoot "Cargo.toml") -Raw
    if ($toml -match '(?m)^version\s*=\s*"([^"]+)"') {
        return $Matches[1]
    }
    throw "Cannot read version from Cargo.toml"
}

function Find-ISCC {
    if ($IsccPath) {
        if (-not (Test-Path $IsccPath)) {
            throw "ISCC not found: $IsccPath"
        }
        return (Resolve-Path $IsccPath).Path
    }

    $candidates = @(
        "${env:LocalAppData}\Programs\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles}\Inno Setup 6\ISCC.exe",
        "${env:ProgramFiles(x86)}\Inno Setup 5\ISCC.exe"
    )

    foreach ($p in $candidates) {
        if ($p -and (Test-Path $p)) {
            return $p
        }
    }

    $cmd = Get-Command ISCC.exe -ErrorAction SilentlyContinue
    if ($cmd) {
        return $cmd.Source
    }

    throw "ISCC.exe not found. Install Inno Setup 6 from https://jrsoftware.org/isdl.php or pass -IsccPath."
}

Push-Location $RepoRoot
try {
    $version = Get-AppVersion
    Write-Host "==> Sway $version - building installer" -ForegroundColor Cyan

    if (-not $SkipBuild) {
        Write-Host "==> cargo build --release" -ForegroundColor Cyan
        cargo build --release
        if ($LASTEXITCODE -ne 0) {
            throw "cargo build failed (exit $LASTEXITCODE)"
        }
    }
    else {
        Write-Host "==> skip build (-SkipBuild)" -ForegroundColor Yellow
    }

    if (-not (Test-Path $ExePath)) {
        throw "Executable not found: $ExePath. Run cargo build --release first, or omit -SkipBuild."
    }

    New-Item -ItemType Directory -Force -Path $DistDir | Out-Null

    $iscc = Find-ISCC
    Write-Host "==> ISCC: $iscc" -ForegroundColor Cyan
    Write-Host "==> compiling Inno script..." -ForegroundColor Cyan

    $sourceDir = $ReleaseDir
    $outputDir = $DistDir

    & $iscc `
        "/DMyAppVersion=$version" `
        "/DMyAppSourceDir=$sourceDir" `
        "/DMyAppOutputDir=$outputDir" `
        $IssFile

    if ($LASTEXITCODE -ne 0) {
        throw "ISCC failed (exit $LASTEXITCODE)"
    }

    $setup = Join-Path $DistDir "Sway-$version-setup.exe"
    if (-not (Test-Path $setup)) {
        throw "Installer not created: $setup"
    }

    $sizeMb = [math]::Round((Get-Item $setup).Length / 1MB, 2)
    Write-Host ""
    Write-Host "Done: $setup ($sizeMb MB)" -ForegroundColor Green
}
finally {
    Pop-Location
}
