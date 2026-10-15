<#
.SYNOPSIS
Installs macwin for the current user.

.DESCRIPTION
Copies macwin.exe and macwin-tray.exe to %LOCALAPPDATA%\Programs\macwin, adds
that folder to the user's PATH, starts the tray at login (`macwin autostart
on`), allows macwin through Windows Firewall (UDP 47800 for sessions and
pairing, 47801 for discovery; Windows asks for administrator rights once) and
starts the tray. Running it again updates an existing installation.
#>
[CmdletBinding()]
param(
    # Install from this folder instead of the script's own.
    [string]$Source = $PSScriptRoot,
    [string]$Destination = (Join-Path $env:LOCALAPPDATA 'Programs\macwin'),
    # Also allow macwin on networks marked Public.
    [switch]$AllNetworks,
    [switch]$NoFirewall,
    [switch]$NoAutostart,
    [switch]$NoPath,
    [switch]$NoStart
)
$ErrorActionPreference = 'Stop'

function Get-NormalizedDirectoryPath([string]$Path) {
    $provider = $null
    $drive = $null
    $providerPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath(
        $Path, [ref]$provider, [ref]$drive)
    if ($null -eq $provider -or $provider.Name -ne 'FileSystem') {
        throw "Destination must resolve to a filesystem path, not '$($provider.Name)'."
    }
    $fullPath = [IO.Path]::GetFullPath($providerPath)
    $root = [IO.Path]::GetPathRoot($fullPath)
    if ($fullPath.Length -gt $root.Length) {
        $separators = [char[]]@([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
        $fullPath = $fullPath.TrimEnd($separators)
    }
    $fullPath
}

function Test-SameDirectoryPath([string]$Path, [string]$OtherPath) {
    $comparison = [StringComparison]::OrdinalIgnoreCase
    if (-not [IO.Path]::IsPathRooted($Path) -or -not [IO.Path]::IsPathRooted($OtherPath)) {
        return [string]::Equals($Path, $OtherPath, $comparison)
    }
    try {
        [string]::Equals((Get-NormalizedDirectoryPath $Path),
            (Get-NormalizedDirectoryPath $OtherPath), $comparison)
    } catch {
        [string]::Equals($Path, $OtherPath, $comparison)
    }
}

$Destination = Get-NormalizedDirectoryPath $Destination

$programs = 'macwin.exe', 'macwin-tray.exe'
foreach ($exe in $programs) {
    if (-not (Test-Path (Join-Path $Source $exe))) {
        throw "$exe is missing in $Source"
    }
}

function Send-SettingChange {
    # Removing a variable that does not exist changes nothing, but .NET still broadcasts
    # WM_SETTINGCHANGE, so Explorer starts new programs with the new PATH.
    [Environment]::SetEnvironmentVariable('MACWIN_SETTING_CHANGE', $null, 'User')
}

function Test-PathWithinFolder([string]$Path, [string]$Folder) {
    # A separator boundary avoids matching siblings such as `macwin-old`.
    $folderPath = [IO.Path]::GetFullPath($Folder)
    $separator = [IO.Path]::DirectorySeparatorChar
    if ($folderPath[$folderPath.Length - 1] -ne $separator) {
        $folderPath += $separator
    }
    [IO.Path]::GetFullPath($Path).StartsWith($folderPath, [StringComparison]::OrdinalIgnoreCase)
}

function Stop-Macwin([string]$Folder) {
    # A running copy keeps its files locked; its peer just sees a disconnect.
    Get-Process -Name 'macwin', 'macwin-tray' -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -and (Test-PathWithinFolder $_.Path $Folder) } |
        Stop-Process -Force -PassThru |
        Wait-Process -Timeout 10 -ErrorAction SilentlyContinue
}

Stop-Macwin $Destination
New-Item -ItemType Directory -Force -Path $Destination | Out-Null
foreach ($file in $programs + 'uninstall.ps1', 'README.txt', 'THIRD_PARTY_NOTICES') {
    $path = Join-Path $Source $file
    if (Test-Path $path) {
        Copy-Item $path $Destination -Force
    }
}
Write-Host "Installed in $Destination"

if (-not $NoPath) {
    # The raw value and its type (normally REG_EXPAND_SZ), so entries like
    # %USERPROFILE%\bin stay as they are; [Environment]::SetEnvironmentVariable
    # would store them expanded.
    $envKey = [Microsoft.Win32.Registry]::CurrentUser.CreateSubKey('Environment')
    try {
        $raw = [string]$envKey.GetValue('Path', '',
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $kind = [Microsoft.Win32.RegistryValueKind]::ExpandString
        if ($envKey.GetValueNames() -contains 'Path' -and
            $envKey.GetValueKind('Path') -eq [Microsoft.Win32.RegistryValueKind]::String) {
            $kind = [Microsoft.Win32.RegistryValueKind]::String
        }
        $parts = @($raw -split ';' | Where-Object { $_ })
        $pathContainsDestination = $false
        foreach ($part in $parts) {
            if (Test-SameDirectoryPath $part $Destination) {
                $pathContainsDestination = $true
                break
            }
        }
        if (-not $pathContainsDestination) {
            $envKey.SetValue('Path', (($parts + $Destination) -join ';'), $kind)
            Send-SettingChange
            Write-Host 'Added to PATH: new terminals know `macwin`.'
        }
    } finally {
        $envKey.Close()
    }
}

if (-not $NoAutostart) {
    & (Join-Path $Destination 'macwin.exe') autostart on
    if ($LASTEXITCODE -ne 0) {
        throw '`macwin autostart on` failed'
    }
}

if (-not $NoFirewall) {
    $profiles = if ($AllNetworks) { 'Private,Domain,Public' } else { 'Private,Domain' }
    $folder = $Destination -replace "'", "''"
    $rules = @"
Remove-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue
foreach (`$exe in 'macwin.exe', 'macwin-tray.exe') {
    New-NetFirewallRule -DisplayName "macwin (`$exe)" -Group 'macwin' -Direction Inbound ``
        -Action Allow -Protocol UDP -LocalPort 47800,47801 -Profile $profiles ``
        -Program (Join-Path '$folder' `$exe) | Out-Null
}
"@
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    try {
        if ($admin) {
            Invoke-Expression $rules
        } else {
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($rules))
            $p = Start-Process powershell.exe -Verb RunAs -Wait -PassThru -WindowStyle Hidden `
                -ArgumentList '-NoProfile', '-EncodedCommand', $encoded
            if ($p.ExitCode -ne 0) {
                throw "exit code $($p.ExitCode)"
            }
        }
        Write-Host "Allowed through Windows Firewall ($profiles networks)."
    } catch {
        Write-Warning "The firewall rules were not added ($_). Windows asks when macwin first listens."
    }
}

if (-not $NoStart) {
    Start-Process (Join-Path $Destination 'macwin-tray.exe')
    Write-Host 'macwin is running: its icon is in the notification area.'
}
