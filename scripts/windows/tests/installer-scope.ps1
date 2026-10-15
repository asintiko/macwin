$ErrorActionPreference = 'Stop'

function Get-FunctionScriptBlock([string]$Path, [string]$Name) {
    $tokens = $null
    $parseErrors = $null
    $ast = [System.Management.Automation.Language.Parser]::ParseFile(
        $Path, [ref]$tokens, [ref]$parseErrors)
    if ($parseErrors.Count) {
        throw "PowerShell syntax errors in ${Path}: $($parseErrors -join '; ')"
    }
    $definition = $ast.Find({
        param($node)
        $node -is [System.Management.Automation.Language.FunctionDefinitionAst] -and
            $node.Name -eq $Name
    }, $true)
    if (-not $definition) {
        throw "Function '$Name' was not found in $Path"
    }
    [scriptblock]::Create($definition.Extent.Text)
}

$windowsScripts = Split-Path -Parent $PSScriptRoot
$installScript = Join-Path $windowsScripts 'install.ps1'
$uninstallScript = Join-Path $windowsScripts 'uninstall.ps1'
$tempRoot = Join-Path ([IO.Path]::GetTempPath()) ("macwin-installer-tests-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tempRoot | Out-Null

try {
    $base = Join-Path $tempRoot 'macwin'
    $pathCases = @(
        @{ Path = (Join-Path $base 'macwin.exe'); Expected = $true },
        @{ Path = (Join-Path (Join-Path $base 'nested') 'macwin-tray.exe'); Expected = $true },
        @{ Path = (Join-Path "${base}-old" 'macwin.exe'); Expected = $false },
        @{ Path = (Join-Path $base 'MACWIN.EXE'); Expected = $true }
    )

    foreach ($script in @($installScript, $uninstallScript)) {
        . (Get-FunctionScriptBlock $script 'Test-PathWithinFolder')
        foreach ($case in $pathCases) {
            $actual = Test-PathWithinFolder -Path $case.Path -Folder $base
            if ($actual -ne $case.Expected) {
                throw "Unexpected folder match for '$($case.Path)' against '$base'"
            }
        }

        . (Get-FunctionScriptBlock $script 'Get-NormalizedDirectoryPath')
        $expectedPath = [IO.Path]::GetFullPath($base)
        $trailingSeparatorPath = $base + [IO.Path]::DirectorySeparatorChar
        if ((Get-NormalizedDirectoryPath $trailingSeparatorPath) -ne $expectedPath) {
            throw "A trailing separator was not normalized in $script"
        }
        $root = [IO.Path]::GetPathRoot($expectedPath)
        if ((Get-NormalizedDirectoryPath $root) -ne $root) {
            throw "A filesystem root was changed in $script"
        }
        . (Get-FunctionScriptBlock $script 'Test-SameDirectoryPath')
        if (-not (Test-SameDirectoryPath $trailingSeparatorPath $expectedPath)) {
            throw "Equivalent absolute paths were not matched in $script"
        }
        if (Test-SameDirectoryPath ($base + '-old') $expectedPath) {
            throw "A sibling directory was treated as the same path in $script"
        }
        if (Test-SameDirectoryPath '%USERPROFILE%\Programs\macwin' $expectedPath) {
            throw "An unrelated PATH variable was treated as the same path in $script"
        }

        $nonFileSystemPathRejected = $false
        try { Get-NormalizedDirectoryPath 'Env:PATH' | Out-Null } catch { $nonFileSystemPathRejected = $true }
        if (-not $nonFileSystemPathRejected) {
            throw "A non-filesystem provider path was accepted in $script"
        }

        $oldCurrentDirectory = [Environment]::CurrentDirectory
        $dotNetCurrentDirectory = Join-Path $tempRoot 'dotnet-current'
        New-Item -ItemType Directory -Path $dotNetCurrentDirectory -Force | Out-Null
        $locationPushed = $false
        try {
            Push-Location -LiteralPath $tempRoot
            $locationPushed = $true
            [Environment]::CurrentDirectory = $dotNetCurrentDirectory
            if ([string]::Equals((Get-Location).ProviderPath, [Environment]::CurrentDirectory,
                    [StringComparison]::OrdinalIgnoreCase)) {
                throw 'The test must keep PowerShell and .NET current directories different.'
            }
            $separator = [IO.Path]::DirectorySeparatorChar
            $relativePath = ".${separator}nested${separator}..${separator}macwin${separator}"
            if ((Get-NormalizedDirectoryPath $relativePath) -ne $expectedPath) {
                throw "A relative path was not normalized in $script"
            }
        } finally {
            try {
                if ($locationPushed) { Pop-Location }
            } finally {
                [Environment]::CurrentDirectory = $oldCurrentDirectory
            }
        }
    }

    . (Get-FunctionScriptBlock $uninstallScript 'Get-DefaultDestination')
    $hadLocalAppData = Test-Path Env:LOCALAPPDATA
    $oldLocalAppData = $env:LOCALAPPDATA
    try {
        $env:LOCALAPPDATA = Join-Path $tempRoot 'local'
        $releaseRoot = Join-Path $tempRoot 'release'
        New-Item -ItemType Directory -Path $releaseRoot | Out-Null
        New-Item -ItemType File -Path (Join-Path $releaseRoot 'install.ps1') | Out-Null
        $expectedDefault = Join-Path $env:LOCALAPPDATA 'Programs\macwin'
        if ((Get-DefaultDestination $releaseRoot) -ne $expectedDefault) {
            throw 'The bundled uninstaller must keep the standard install destination.'
        }

        $installedRoot = Join-Path $tempRoot 'custom-install'
        New-Item -ItemType Directory -Path $installedRoot | Out-Null
        foreach ($file in 'macwin.exe', 'macwin-tray.exe') {
            New-Item -ItemType File -Path (Join-Path $installedRoot $file) | Out-Null
        }
        if ((Get-DefaultDestination $installedRoot) -ne $installedRoot) {
            throw 'The installed uninstaller must default to its own folder.'
        }

        $standalone = Join-Path $tempRoot 'standalone'
        New-Item -ItemType Directory -Path $standalone | Out-Null
        $rejected = $false
        try { Get-DefaultDestination $standalone | Out-Null } catch { $rejected = $true }
        if (-not $rejected) {
            throw 'A moved uninstaller must not guess that an unrelated folder is the installation.'
        }
    } finally {
        if ($hadLocalAppData) {
            $env:LOCALAPPDATA = $oldLocalAppData
        } else {
            Remove-Item Env:LOCALAPPDATA -ErrorAction SilentlyContinue
        }
    }

    . (Get-FunctionScriptBlock $uninstallScript 'Remove-ProgramFiles')
    foreach ($preserveOtherFiles in @($false, $true)) {
        $folder = Join-Path $tempRoot "cleanup-$preserveOtherFiles"
        New-Item -ItemType Directory -Path $folder | Out-Null
        $programFiles = 'macwin.exe', 'macwin-tray.exe', 'uninstall.ps1', 'README.txt', 'THIRD_PARTY_NOTICES'
        foreach ($file in $programFiles) {
            New-Item -ItemType File -Path (Join-Path $folder $file) | Out-Null
        }
        $userFile = Join-Path $folder 'notes.txt'
        if ($preserveOtherFiles) {
            Set-Content -LiteralPath $userFile -Value 'keep me'
        }
        Remove-ProgramFiles $folder
        foreach ($file in $programFiles) {
            if (Test-Path -LiteralPath (Join-Path $folder $file)) {
                throw "Program file was not removed: $file"
            }
        }
        if ($preserveOtherFiles) {
            if ((Get-Content -LiteralPath $userFile) -ne 'keep me') {
                throw 'An unrelated user file was modified or removed.'
            }
        } elseif (Test-Path -LiteralPath $folder) {
            throw 'An empty installation folder was left behind.'
        }
    }
} finally {
    Remove-Item -LiteralPath $tempRoot -Recurse -Force
}

Write-Host 'Windows installer scope tests passed.'
