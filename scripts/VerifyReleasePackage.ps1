param(
    [Parameter(Mandatory=$true)][string]$PackageDirectory,
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    [Parameter(Mandatory=$true)][ValidatePattern('^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$')][string]$Version
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'Process.ps1')
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem
$optIn = 'satin-windows-x64-opt-in.exe'
$zipName = 'Satin-windows-x64.zip'
$checksumsName = 'SHA256SUMS.txt'

function Assert-X64Executable([string]$Path) {
    $reader = [IO.BinaryReader]::new([IO.File]::OpenRead($Path))
    try {
        if ($reader.BaseStream.Length -lt 64 -or $reader.ReadUInt16() -ne 0x5a4d) {
            throw "Missing executable header in $Path"
        }
        $reader.BaseStream.Position = 0x3c
        $offset = $reader.ReadUInt32()
        if ($offset -lt 64 -or $offset -gt ($reader.BaseStream.Length - 26)) {
            throw "Invalid PE header offset in $Path"
        }
        $reader.BaseStream.Position = $offset
        if ($reader.ReadUInt32() -ne 0x4550 -or $reader.ReadUInt16() -ne 0x8664) {
            throw "Not a Windows x64 PE executable: $Path"
        }
        $reader.BaseStream.Position = $offset + 24
        if ($reader.ReadUInt16() -ne 0x20b) { throw "Not a PE32+ executable: $Path" }
    } finally { $reader.Dispose() }
}

$package = (Resolve-Path -LiteralPath $PackageDirectory).Path
$names = @(Get-ChildItem -LiteralPath $package -Force | Select-Object -ExpandProperty Name)
$expectedFiles = @($optIn, $zipName, $checksumsName)
if (Compare-Object -CaseSensitive ($names | Sort-Object) ($expectedFiles | Sort-Object)) {
    throw 'Unexpected release directory contents. In particular, never ship a legacy update alias.'
}
$hashes = @{}
foreach ($line in [IO.File]::ReadAllLines((Join-Path $package $checksumsName))) {
    if ($line -cnotmatch '^([0-9a-f]{64})  (satin-windows-x64-opt-in\.exe|Satin-windows-x64\.zip)$') {
        throw 'Unrecognized checksum line.'
    }
    $hash, $name = $Matches[1], $Matches[2]
    if ($hashes.ContainsKey($name)) { throw "Duplicate checksum for $name" }
    $hashes[$name] = $hash
}
foreach ($name in @($optIn, $zipName)) {
    if ((Get-SatinFileSha256 (Join-Path $package $name)).ToLowerInvariant() -cne $hashes[$name]) {
        throw "Checksum mismatch for $name"
    }
}

# Only copy two exact archive members into a new scratch hierarchy, not the
# real installation, and never invoke any of the ZIP's installer scripts.
$evidence = (New-Item -ItemType Directory -Path $EvidenceDirectory).FullName
$stage = Join-Path $evidence 'staged'
$execRelative = Join-Path 'versions' (Join-Path $Version 'rust-browser.exe')
$browser = Join-Path $stage $execRelative
$launcher = Join-Path $stage 'RustBrowser.exe'
New-Item -ItemType Directory -Path ([IO.Path]::GetDirectoryName($browser)) | Out-Null
$zip = [IO.Compression.ZipFile]::OpenRead((Join-Path $package $zipName))
try {
    $required = @('Install.cmd', 'README.txt', 'scripts/Install.ps1', 'scripts/Process.ps1',
        'scripts/Rollback.ps1', 'target/release/rust-browser.exe', 'target/release/rust-browser-launcher.exe')
    foreach ($member in $required) {
        if (@($zip.Entries | Where-Object FullName -CEQ $member).Count -ne 1) {
            throw "Missing or ambiguous ZIP member: $member"
        }
    }
    [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry($required[-2]), $browser)
    [IO.Compression.ZipFileExtensions]::ExtractToFile($zip.GetEntry($required[-1]), $launcher)
} finally { $zip.Dispose() }
Assert-X64Executable $browser
Assert-X64Executable $launcher
if ((Get-SatinFileSha256 $browser).ToLowerInvariant() -cne $hashes[$optIn]) {
    throw 'The ZIP browser is not byte-identical to the opt-in update asset.'
}
$configuration = @{executable=$execRelative} | ConvertTo-Json -Compress
[IO.File]::WriteAllText((Join-Path $stage 'current.json'), $configuration, [Text.UTF8Encoding]::new($false))
$logs = Join-Path $evidence 'version-logs'
foreach ($binary in @($browser, $launcher)) {
    $result = Invoke-BrowserProcess -Executable $binary -Arguments @('--version') -LogDirectory $logs
    if ($result.ExitCode -ne 0 -or $result.Stdout.Trim() -cne "Satin $Version") {
        throw "The staged package did not report version $Version. See $($result.StderrPath)."
    }
}
[ordered]@{
    version = $Version
    browser = $hashes[$optIn]
    installer = $hashes[$zipName]
    launcher = (Get-SatinFileSha256 $launcher).ToLowerInvariant()
    legacyAssetAbsent = $true
} | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $evidence 'verified.json') -Encoding ASCII
"Verified Windows x64 release package ${Version}: $package"
