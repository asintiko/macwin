<#
.SYNOPSIS
Removes macwin for the current user.

.DESCRIPTION
Stops macwin and removes start at login, the firewall rules (Windows asks for
administrator rights once), the PATH entry and the program folder. With
-Purge it deletes all macwin data, including settings, identity, logs and received
files, so the computers have to pair again.
#>
[CmdletBinding()]
param(
    [string]$Destination,
    [switch]$Purge
)
$ErrorActionPreference = 'Stop'

function Get-DefaultDestination([string]$ScriptRoot) {
    # The release folder ships install.ps1; installed copies do not.
    if (Test-Path -LiteralPath (Join-Path $ScriptRoot 'install.ps1')) {
        Join-Path $env:LOCALAPPDATA 'Programs\macwin'
    } elseif ((Test-Path -LiteralPath (Join-Path $ScriptRoot 'macwin.exe') -PathType Leaf) -and
              (Test-Path -LiteralPath (Join-Path $ScriptRoot 'macwin-tray.exe') -PathType Leaf)) {
        $ScriptRoot
    } else {
        throw 'Cannot locate the installation. Run the installed uninstaller or pass -Destination.'
    }
}

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

if (-not $PSBoundParameters.ContainsKey('Destination')) {
    $Destination = Get-DefaultDestination $PSScriptRoot
}
$Destination = Get-NormalizedDirectoryPath $Destination

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

function Remove-ProgramFiles([string]$Folder) {
    foreach ($file in 'macwin.exe', 'macwin-tray.exe', 'uninstall.ps1', 'README.txt', 'THIRD_PARTY_NOTICES') {
        $path = Join-Path $Folder $file
        if (Test-Path -LiteralPath $path -PathType Leaf) {
            Remove-Item -LiteralPath $path -Force
        }
    }
    if (Test-Path -LiteralPath $Folder -PathType Container) {
        if (Get-ChildItem -LiteralPath $Folder -Force | Select-Object -First 1) {
            Write-Warning 'The installation folder contains other files and has been kept.'
        } else {
            Remove-Item -LiteralPath $Folder -Force
        }
    }
}

Get-Process -Name 'macwin', 'macwin-tray' -ErrorAction SilentlyContinue |
    Where-Object { $_.Path -and (Test-PathWithinFolder $_.Path $Destination) } |
    Stop-Process -Force -PassThru |
    Wait-Process -Timeout 10 -ErrorAction SilentlyContinue

Remove-ItemProperty -Path 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Run' -Name 'macwin' `
    -ErrorAction SilentlyContinue

if (Get-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue) {
    $rules = "Remove-NetFirewallRule -Group 'macwin' -ErrorAction SilentlyContinue"
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $admin = ([Security.Principal.WindowsPrincipal]$identity).IsInRole(
        [Security.Principal.WindowsBuiltInRole]::Administrator)
    try {
        if ($admin) {
            Invoke-Expression $rules
        } else {
            $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($rules))
            Start-Process powershell.exe -Verb RunAs -Wait -WindowStyle Hidden `
                -ArgumentList '-NoProfile', '-EncodedCommand', $encoded
        }
    } catch {
        Write-Warning "The firewall rules were not removed ($_)."
    }
}

# Only our entry goes; the rest of the raw value and its type stay (see install.ps1).
$envKey = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
if ($envKey) {
    try {
        $raw = [string]$envKey.GetValue('Path', '',
            [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
        $parts = @($raw -split ';' | Where-Object { $_ })
        $kept = @($parts | Where-Object { -not (Test-SameDirectoryPath $_ $Destination) })
        if ($kept.Count -ne $parts.Count) {
            if ($kept.Count) {
                $envKey.SetValue('Path', ($kept -join ';'), $envKey.GetValueKind('Path'))
            } else {
                $envKey.DeleteValue('Path')
            }
            Send-SettingChange
        }
    } finally {
        $envKey.Close()
    }
}

# A folder cannot be removed while it is the current directory, which it is when this
# script was started from it (for example with "Run with PowerShell").
$neutral = [IO.Path]::GetTempPath()
Set-Location -LiteralPath $neutral
[Environment]::CurrentDirectory = $neutral
Remove-ProgramFiles $Destination
if ($Purge) {
    foreach ($dir in (Join-Path $env:APPDATA 'macwin'), (Join-Path $env:LOCALAPPDATA 'macwin')) {
        if (Test-Path $dir) {
            Remove-Item $dir -Recurse -Force
        }
    }
}
Write-Host 'macwin is removed.'
