# Builds dist\ClipboardX-<version>-windows-x86_64.zip (the app alone, to
# run from anywhere) and dist\ClipboardX-<version>-windows-x86_64-setup.exe
# (the installer, made with Inno Setup 6), each with its .sha256.
#
#   scripts\package-windows.ps1 [-NoInstaller]
#
# -NoInstaller leaves the installer out, for a machine without Inno Setup.
# Nothing is signed: Windows SmartScreen warns the first time either runs.
param([switch]$NoInstaller)

$ErrorActionPreference = 'Stop'
Set-Location (Split-Path -Parent $PSScriptRoot)

$match = Select-String -Path Cargo.toml -Pattern '^version = "(.*)"' | Select-Object -First 1
if (-not $match) { throw 'no version in Cargo.toml' }
$version = $match.Matches[0].Groups[1].Value

cargo build --release
if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }

# Where cargo puts builds (CARGO_TARGET_DIR and config included).
$metadata = cargo metadata --format-version 1 --no-deps | ConvertFrom-Json
$built = Join-Path $metadata.target_directory 'release\clipboardx.exe'
if (-not (Test-Path $built)) { throw "no executable at $built" }

$dist = Join-Path (Get-Location) 'dist'
$stage = Join-Path $dist 'windows'
if (Test-Path $stage) { Remove-Item -Recurse -Force $stage }
New-Item -ItemType Directory -Force $stage | Out-Null
$exe = Join-Path $stage 'ClipboardX.exe'
Copy-Item $built $exe

# "<hash>  <name>", as shasum writes it for the other platforms.
function Write-Checksum([string]$Path) {
    $hash = (Get-FileHash -Algorithm SHA256 $Path).Hash.ToLower()
    $name = Split-Path -Leaf $Path
    [IO.File]::WriteAllText("$Path.sha256", "$hash  $name`n")
}

$name = "ClipboardX-$version-windows-x86_64"
$zip = Join-Path $dist "$name.zip"
if (Test-Path $zip) { Remove-Item -Force $zip }
Compress-Archive -Path $exe -DestinationPath $zip
Write-Checksum $zip
Write-Host "built $zip"

if ($NoInstaller) { return }

$iscc = Get-Command iscc -ErrorAction SilentlyContinue | Select-Object -First 1 -ExpandProperty Source
if (-not $iscc) {
    $installed = Join-Path ${env:ProgramFiles(x86)} 'Inno Setup 6\ISCC.exe'
    if (Test-Path $installed) { $iscc = $installed }
}
if (-not $iscc) { throw 'Inno Setup 6 (ISCC.exe) is not installed; pass -NoInstaller to skip the installer' }

$setup = "$name-setup"
& $iscc /Qp "/DAppVersion=$version" "/DSourceExe=$exe" "/DOutputDir=$dist" "/DOutputName=$setup" 'scripts\windows-installer.iss'
if ($LASTEXITCODE -ne 0) { throw 'Inno Setup failed' }
$installer = Join-Path $dist "$setup.exe"
Write-Checksum $installer
Write-Host "built $installer"
