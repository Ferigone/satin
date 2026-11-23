param(
    [switch]$CreateShortcut,
    [switch]$NoLaunch,
    [string]$ShortcutDirectory = [Environment]::GetFolderPath('DesktopDirectory'),
    [switch]$FromShortcut,
    [ValidatePattern('\A[0-9a-fA-F]{64}\z')]
    [string]$VerifiedReleaseSha256
)

$ErrorActionPreference = 'Stop'
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))

function New-PreviewShortcut {
    if (-not $ShortcutDirectory) { throw 'No desktop directory is configured. Pass -ShortcutDirectory with a destination.' }
    $directory = [IO.Directory]::CreateDirectory([IO.Path]::GetFullPath($ShortcutDirectory))
    $shortcutPath = Join-Path $directory.FullName 'Satin Preview.lnk'
    $icon = Get-Item -LiteralPath (Join-Path $projectDirectory 'assets\satin.ico')
    $windowsPowerShell = Join-Path ([Environment]::SystemDirectory) 'WindowsPowerShell\v1.0\powershell.exe'
    $description = 'Build and open Satin from the current checkout'
    $shell = New-Object -ComObject WScript.Shell
    $shortcut = $shell.CreateShortcut($shortcutPath)
    if ((Test-Path -LiteralPath $shortcutPath) -and
        ($shortcut.TargetPath -ne $windowsPowerShell -or $shortcut.Description -ne $description)) {
        throw "A different shortcut already uses $shortcutPath."
    }
    $shortcut.TargetPath = $windowsPowerShell
    $shortcut.Arguments = '-NoLogo -NoProfile -ExecutionPolicy Bypass -File "{0}" -FromShortcut' -f $PSCommandPath
    if ($VerifiedReleaseSha256) {
        $shortcut.Arguments += ' -VerifiedReleaseSha256 ' + $VerifiedReleaseSha256.ToLowerInvariant()
    }
    $shortcut.WorkingDirectory = $projectDirectory
    $shortcut.IconLocation = $icon.FullName + ',0'
    $shortcut.Description = $description
    $shortcut.WindowStyle = 7
    $shortcut.Save()
    $shortcutPath
}

function Build-Preview {
    $privateTools = Join-Path $env:LOCALAPPDATA 'RustBrowserDev'
    $cargo = Join-Path $privateTools 'cargo\bin\cargo.exe'
    $cargoHome = $env:CARGO_HOME
    $rustupHome = $env:RUSTUP_HOME
    $debug = $env:CARGO_PROFILE_DEV_DEBUG
    try {
        if (Test-Path -LiteralPath $cargo -PathType Leaf) {
            $env:CARGO_HOME = Join-Path $privateTools 'cargo'
            $env:RUSTUP_HOME = Join-Path $privateTools 'rustup'
        } else {
            $command = Get-Command cargo.exe -CommandType Application -ErrorAction SilentlyContinue
            if (-not $command) { throw 'Cargo was unavailable. Install the Rust toolchain or add cargo.exe to PATH.' }
            $cargo = $command.Source
        }
        # Keep the default incremental build without copying debug symbols into each snapshot.
        if (-not $debug) { $env:CARGO_PROFILE_DEV_DEBUG = '0' }
        Write-Host "Building Satin Preview. Build log at $buildLog"
        $process = Start-Process -FilePath $cargo -WorkingDirectory $projectDirectory `
            -ArgumentList @('build', '--locked', '--bin', 'rust-browser', '--color', 'never', '--message-format=json-render-diagnostics') `
            -WindowStyle Hidden -RedirectStandardOutput $artifactLog -RedirectStandardError $buildLog -PassThru -Wait
        if ($process.ExitCode -ne 0) {
            Get-Content -LiteralPath $buildLog -Tail 30 -Encoding UTF8 | ForEach-Object { Write-Host $_ }
            throw "Cargo exited with code $($process.ExitCode). See $buildLog."
        }
    } finally {
        $env:CARGO_HOME = $cargoHome
        $env:RUSTUP_HOME = $rustupHome
        $env:CARGO_PROFILE_DEV_DEBUG = $debug
    }

    $executable = $null
    foreach ($line in (Get-Content -LiteralPath $artifactLog -Encoding UTF8)) {
        if (-not $line.StartsWith('{')) { continue }
        $artifact = ConvertFrom-Json -InputObject $line
        if ($artifact.reason -eq 'compiler-artifact' -and $artifact.target.name -eq 'rust-browser' -and
            $artifact.target.kind -contains 'bin' -and $artifact.executable) {
            $executable = [string]$artifact.executable
        }
    }
    if (-not $executable -or -not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Cargo did not report a Satin executable. See $buildLog."
    }
    $executable
}

function Confirm-PreviewExecutable([string]$Executable) {
    $stdout = Join-Path $previewRoot 'version.log'
    $stderr = Join-Path $previewRoot 'version-errors.log'
    $process = Start-Process -FilePath $Executable -ArgumentList '--version' -WindowStyle Hidden `
        -RedirectStandardOutput $stdout -RedirectStandardError $stderr -PassThru -Wait
    $version = [IO.File]::ReadAllText($stdout).Trim()
    if ($process.ExitCode -ne 0 -or $version -notmatch '^Satin [0-9][^\r\n]*$') {
        throw "The preview executable did not report a Satin version. See $stdout and $stderr."
    }
    $version
}

if ($CreateShortcut) {
    New-PreviewShortcut
    return
}

if (-not $env:LOCALAPPDATA) { throw 'LOCALAPPDATA is required for Satin Preview.' }
$previewRoot = Join-Path $env:LOCALAPPDATA 'SatinPreview'
$snapshots = Join-Path $previewRoot 'builds'
$profile = Join-Path $previewRoot 'User Data'
$buildLog = Join-Path $previewRoot 'build.log'
$artifactLog = Join-Path $previewRoot 'build-artifacts.jsonl'
$pending = Join-Path $snapshots 'rust-browser.pending.exe'
$guard = $null
try {
    [void][IO.Directory]::CreateDirectory($snapshots)
    try {
        $guard = [IO.File]::Open((Join-Path $previewRoot 'build.lock'), 'OpenOrCreate', 'ReadWrite', 'None')
    } catch [IO.IOException] {
        throw "Satin Preview could not acquire its build lock. Another launch may already be building. $($_.Exception.Message)"
    }

    if ($VerifiedReleaseSha256) {
        $pinnedHash = $VerifiedReleaseSha256.ToLowerInvariant()
        $pinnedSnapshot = Join-Path $snapshots "rust-browser-$pinnedHash.exe"
        $executable = if (Test-Path -LiteralPath $pinnedSnapshot -PathType Leaf) {
            $pinnedSnapshot
        } else {
            Join-Path $projectDirectory 'target\release\rust-browser.exe'
        }
    } else {
        $executable = Build-Preview
    }
    $hash = (Get-FileHash -LiteralPath $executable -Algorithm SHA256).Hash.ToLowerInvariant()
    if ($VerifiedReleaseSha256 -and $hash -cne $pinnedHash) {
        throw 'The Satin Preview executable no longer matches the verified release build.'
    }
    $snapshot = Join-Path $snapshots "rust-browser-$hash.exe"
    $reused = (Test-Path -LiteralPath $snapshot -PathType Leaf) -and
        (Get-FileHash -LiteralPath $snapshot -Algorithm SHA256).Hash -eq $hash
    if ($reused) {
        $version = Confirm-PreviewExecutable $snapshot
    } else {
        Copy-Item -LiteralPath $executable -Destination $pending -Force
        if ((Get-FileHash -LiteralPath $pending -Algorithm SHA256).Hash -ne $hash) {
            throw 'The built executable changed while copying. Try Satin Preview again.'
        }
        $version = Confirm-PreviewExecutable $pending
        if (Test-Path -LiteralPath $snapshot) { [IO.File]::Delete($snapshot) }
        [IO.File]::Move($pending, $snapshot)
    }

    Get-ChildItem -LiteralPath $snapshots -File -Filter 'rust-browser-*.exe' |
        Where-Object { $_.Name -match '^rust-browser-[0-9a-f]{64}\.exe$' -and $_.FullName -ne $snapshot } |
        Sort-Object LastWriteTimeUtc -Descending | Select-Object -Skip 2 | ForEach-Object {
            # An older preview window can still hold its executable open.
            try { [IO.File]::Delete($_.FullName) } catch [IO.IOException] {} catch [UnauthorizedAccessException] {}
        }

    if (-not $NoLaunch) {
        $launch = New-Object System.Diagnostics.ProcessStartInfo
        $launch.FileName = $snapshot
        $launch.WorkingDirectory = $projectDirectory
        $launch.Arguments = '--profile "{0}"' -f $profile
        $launch.UseShellExecute = $false
        $launch.CreateNoWindow = $true
        ([Diagnostics.Process]::Start($launch)).Dispose()
    }
    [pscustomobject]@{
        version = $version
        executable = $snapshot
        sha256 = $hash
        profile = $profile
        reused = $reused
        launched = -not $NoLaunch
        buildLog = $buildLog
    }
} catch {
    if ($FromShortcut) {
        $shell = New-Object -ComObject WScript.Shell
        [void]$shell.Popup($_.Exception.Message, 0, 'Satin Preview', 16)
    }
    throw
} finally {
    if ($guard) {
        try { [IO.File]::Delete($pending) } finally { $guard.Dispose() }
    }
}
