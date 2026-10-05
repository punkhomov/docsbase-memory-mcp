<#
.SYNOPSIS
  docsbase installer — Windows x64.
.DESCRIPTION
  Downloads the release asset from GitHub Releases, verifies sha256,
  installs docsbase.exe into %USERPROFILE%\.local\bin (user PATH),
  then runs `docsbase install`.
.EXAMPLE
  irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1 | iex
.EXAMPLE
  $env:DOCSBASE_CHANNEL = "prerelease"; iex (irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1)
.EXAMPLE
  $env:DOCSBASE_VERSION = "v0.1.0-alpha.3"; iex (irm https://raw.githubusercontent.com/punkhomov/docsbase-memory-mcp/main/install.ps1)
#>
[CmdletBinding()]
param()

$ErrorActionPreference = "Stop"

$Repo = if ($env:DOCSBASE_REPO) { $env:DOCSBASE_REPO } else { "punkhomov/docsbase-memory-mcp" }
$Version = if ($env:DOCSBASE_VERSION) { $env:DOCSBASE_VERSION } else { "latest" }
$BinDir = if ($env:DOCSBASE_BIN_DIR) { $env:DOCSBASE_BIN_DIR } else { Join-Path $env:USERPROFILE ".local\bin" }
$Channel = if ($env:DOCSBASE_CHANNEL) { $env:DOCSBASE_CHANNEL } else { "stable" }

function Fail([string]$msg) { Write-Error "install.ps1: error: $msg"; exit 1 }
function Info([string]$msg) { Write-Host "install.ps1: $msg" }

$arch = $env:PROCESSOR_ARCHITECTURE
if ($arch -notmatch "^(AMD64|x64)$") { Fail "only x64 is supported in v1. Got: $arch" }

if ($Version -eq "latest") {
  if ($Channel -eq "stable") {
    Info "resolving latest stable release tag for $Repo..."
    try {
      $rel = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" -UseBasicParsing
      $Version = $rel.tag_name
    } catch {
      Fail "no stable release published yet; set `$env:DOCSBASE_CHANNEL = 'prerelease' (or pin `$env:DOCSBASE_VERSION = 'vX.Y.Z'). $_"
    }
  } elseif ($Channel -eq "prerelease") {
    Info "resolving newest release tag (prereleases included) for $Repo..."
    try {
      $rels = @(Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases?per_page=1" -UseBasicParsing)
      $Version = $rels[0].tag_name
    } catch {
      Fail "could not resolve any release (set `$env:DOCSBASE_VERSION = 'vX.Y.Z'). $_"
    }
  } else {
    Fail "unknown `$env:DOCSBASE_CHANNEL '$Channel' (expected: stable or prerelease)"
  }
}
if (-not $Version.StartsWith("v")) { $Version = "v$Version" }
$Tag = $Version
$Ver = $Tag.TrimStart("v")

$Asset = "docsbase-$Ver-windows-x86_64.zip"
$BaseUrl = "https://github.com/$Repo/releases/download/$Tag"

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("docsbase-" + [System.Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
  Info "downloading $Asset ($Tag)..."
  try {
    Invoke-WebRequest -Uri "$BaseUrl/$Asset" -OutFile (Join-Path $tmp $Asset) -UseBasicParsing
    Invoke-WebRequest -Uri "$BaseUrl/$Asset.sha256" -OutFile (Join-Path $tmp "$Asset.sha256") -UseBasicParsing
  } catch {
    Fail "download failed: $BaseUrl/$Asset (check the tag exists). $_"
  }

  # Integrity only: the .sha256 ships in the same release as the asset, so it
  # catches truncated/corrupt downloads, not a compromised release.
  Info "verifying sha256..."
  $expected = ((Get-Content (Join-Path $tmp "$Asset.sha256") -Raw) -split '\s+')[0].ToLowerInvariant()
  $actual = (Get-FileHash -Path (Join-Path $tmp $Asset) -Algorithm SHA256).Hash.ToLowerInvariant()
  if ($expected -ne $actual) { Fail "checksum mismatch for $Asset (expected $expected, got $actual)" }

  Info "extracting..."
  Expand-Archive -Path (Join-Path $tmp $Asset) -DestinationPath (Join-Path $tmp "out") -Force
  $exe = Get-ChildItem -Path (Join-Path $tmp "out") -Filter "docsbase.exe" -Recurse | Select-Object -First 1
  if (-not $exe) { Fail "archive did not contain docsbase.exe" }

  New-Item -ItemType Directory -Path $BinDir -Force | Out-Null
  Copy-Item -Path $exe.FullName -Destination (Join-Path $BinDir "docsbase.exe") -Force
  Info "installed $(Join-Path $BinDir 'docsbase.exe')"

  $userPath = [Environment]::GetEnvironmentVariable("Path", "User")
  if ($userPath -notlike "*$BinDir*") {
    [Environment]::SetEnvironmentVariable("Path", "$userPath;$BinDir", "User")
    $env:Path = "$env:Path;$BinDir"
    Info "added $BinDir to the user PATH (reopen the terminal to pick it up everywhere)"
  }

  Info "running: docsbase install"
  & (Join-Path $BinDir "docsbase.exe") install
  Info "done. Verify: docsbase status"
} finally {
  Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
