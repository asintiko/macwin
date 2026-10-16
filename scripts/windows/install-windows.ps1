<#
.SYNOPSIS
Downloads and installs MacWin for Windows without SmartScreen prompts.

.DESCRIPTION
  powershell -NoProfile -ExecutionPolicy Bypass -Command "irm https://github.com/djjdjdkdjdvdvd/macwin/releases/latest/download/install-windows.ps1 | iex"

PowerShell downloads carry no "downloaded from the internet" mark (Mark of
the Web), so SmartScreen does not stop the unsigned test build. The package
is checked against SHA256SUMS.txt of the same release, extracted to a
temporary folder, and installed by its install.ps1: %LOCALAPPDATA%\Programs\macwin,
Start menu, start at login, firewall rules for private networks (Windows asks
for administrator rights once), then the MacWin window opens.

MACWIN_REPO=owner/repo or MACWIN_RELEASE_URL=<folder URL> choose another
download.
#>
$ErrorActionPreference = 'Stop'
# Windows PowerShell 5.1 downloads slowly while it draws a progress bar.
$ProgressPreference = 'SilentlyContinue'
[Net.ServicePointManager]::SecurityProtocol =
    [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

$repo = if ($env:MACWIN_REPO) { $env:MACWIN_REPO } else { 'djjdjdkdjdvdvd/macwin' }
$base = if ($env:MACWIN_RELEASE_URL) {
    $env:MACWIN_RELEASE_URL.TrimEnd('/')
} else {
    "https://github.com/$repo/releases/latest/download"
}
$zipName = 'MacWin-windows-x64.zip'

if (-not [Environment]::Is64BitOperatingSystem) {
    throw 'MacWin needs 64-bit Windows.'
}

$tmp = Join-Path ([IO.Path]::GetTempPath()) ('macwin-' + [Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp | Out-Null
try {
    $zip = Join-Path $tmp $zipName
    $sumsFile = Join-Path $tmp 'SHA256SUMS.txt'
    Write-Host "Downloading $base/$zipName"
    Invoke-WebRequest -UseBasicParsing -Uri "$base/$zipName" -OutFile $zip
    Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS.txt" -OutFile $sumsFile

    $pattern = '^([0-9a-fA-F]{64})\s+\*?' + [regex]::Escape($zipName) + '\s*$'
    $line = Get-Content -LiteralPath $sumsFile | Where-Object { $_ -match $pattern } |
        Select-Object -First 1
    if (-not $line) { throw "SHA256SUMS.txt lists no $zipName" }
    $want = ([regex]::Match($line, $pattern)).Groups[1].Value.ToLowerInvariant()
    $have = (Get-FileHash -Algorithm SHA256 -LiteralPath $zip).Hash.ToLowerInvariant()
    if ($have -ne $want) { throw "$zipName does not match its checksum; try again later" }

    $stage = Join-Path $tmp 'package'
    Expand-Archive -LiteralPath $zip -DestinationPath $stage
    $installer = Get-ChildItem -LiteralPath $stage -Recurse -Filter install.ps1 |
        Select-Object -First 1
    if (-not $installer) { throw 'the package has no install.ps1' }

    # A child process: the policy of this session may forbid running script files.
    & powershell.exe -NoProfile -ExecutionPolicy Bypass -File $installer.FullName
    if ($LASTEXITCODE -ne 0) { throw "install.ps1 failed with exit code $LASTEXITCODE" }
} finally {
    Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
}
