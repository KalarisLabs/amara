# AMARA installer for Windows.
#   irm https://raw.githubusercontent.com/KalarisLabs/amara/main/install.ps1 | iex
# Env: AMARA_VERSION (default: latest), AMARA_INSTALL_DIR (default: %LOCALAPPDATA%\amara\bin)
$ErrorActionPreference = 'Stop'

$repo = 'KalarisLabs/amara'
$version = if ($env:AMARA_VERSION) { $env:AMARA_VERSION } else { 'latest' }
$dir = if ($env:AMARA_INSTALL_DIR) { $env:AMARA_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'amara\bin' }

if (-not [Environment]::Is64BitOperatingSystem) { throw 'AMARA needs 64-bit Windows.' }
$asset = 'amara-windows-x64.exe'
$base = if ($version -eq 'latest') { "https://github.com/$repo/releases/latest/download" } else { "https://github.com/$repo/releases/download/$version" }

New-Item -ItemType Directory -Force -Path $dir | Out-Null
$target = Join-Path $dir 'amara.exe'
$tmp = Join-Path ([IO.Path]::GetTempPath()) "amara-$([guid]::NewGuid()).exe"

Write-Host "> downloading $asset ($version)" -ForegroundColor Magenta
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset" -OutFile $tmp

try {
    $expected = ((Invoke-WebRequest -UseBasicParsing -Uri "$base/$asset.sha256").Content -split '\s+')[0]
    $actual = (Get-FileHash -Algorithm SHA256 $tmp).Hash
    if ($expected -and ($expected.ToLower() -ne $actual.ToLower())) { Remove-Item $tmp; throw "checksum mismatch for $asset" }
    Write-Host '> checksum verified' -ForegroundColor Magenta
} catch [System.Net.WebException] { }

Move-Item -Force $tmp $target
Write-Host "> installed $target" -ForegroundColor Magenta

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if (($userPath -split ';') -notcontains $dir) {
    [Environment]::SetEnvironmentVariable('Path', "$userPath;$dir", 'User')
    $env:Path = "$env:Path;$dir"
    Write-Host "> added $dir to your user PATH (open a new terminal)"
}

Write-Host ''
Write-Host '  next:  amara doctor   ·   amara simulate   ·   amara setup claude-code --write'
Write-Host '  tip:   allow UDP 7433 in Windows Firewall when prompted so nearby agents can find you'
Write-Host ''
