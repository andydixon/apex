# Build Windows artefacts into .\dist:
#   apex-<version>-windows-x86_64.zip   portable: apex.exe, licence, README
#   Apex-<version>-setup.exe            when Inno Setup 6 (ISCC.exe) is installed
# Run from any shell with cargo on PATH:  powershell -File packaging\windows\build.ps1
$ErrorActionPreference = "Stop"
$Root = Resolve-Path (Join-Path $PSScriptRoot "..\..")
Set-Location $Root
$Version = (Select-String -Path Cargo.toml -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
$Dist = Join-Path $Root "dist"
New-Item -ItemType Directory -Force -Path $Dist | Out-Null

if (-not (Test-Path "assets\icons\apex.ico")) {
  Write-Host "assets\icons\apex.ico is missing; generate it with packaging/icons/generate.sh (needs librsvg or ImageMagick)."
}
# crates/apex-app/build.rs embeds assets\icons\apex.ico and the version resource.
cargo build --release -p apex-app

$Stage = Join-Path $Dist "apex-$Version-windows-x86_64"
if (Test-Path $Stage) { Remove-Item -Recurse -Force $Stage }
New-Item -ItemType Directory -Path $Stage | Out-Null
Copy-Item target\release\apex.exe $Stage
Copy-Item LICENSE (Join-Path $Stage "LICENSE.txt")
Copy-Item README.md (Join-Path $Stage "README.md")
Compress-Archive -Path "$Stage\*" -DestinationPath "$Stage.zip" -Force
Remove-Item -Recurse -Force $Stage

$Iscc = Get-Command ISCC.exe -ErrorAction SilentlyContinue
if (-not $Iscc) {
  $Candidate = "${env:ProgramFiles(x86)}\Inno Setup 6\ISCC.exe"
  if (Test-Path $Candidate) { $Iscc = Get-Command $Candidate }
}
if ($Iscc) {
  & $Iscc.Source "/DAppVersion=$Version" "/DSourceDir=$Root" "/O$Dist" (Join-Path $PSScriptRoot "apex.iss")
} else {
  Write-Host "Inno Setup not found; skipped the installer (portable zip built)."
}
Get-ChildItem $Dist
