param([switch]$Release,[switch]$Test)
$ErrorActionPreference = 'Stop'
$projectDirectory = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$privateTools = Join-Path $env:LOCALAPPDATA 'RustBrowserDev'
$privateCargo = Join-Path $privateTools 'cargo\bin\cargo.exe'
if (Test-Path -LiteralPath $privateCargo) {
    $env:CARGO_HOME = Join-Path $privateTools 'cargo'
    $env:RUSTUP_HOME = Join-Path $privateTools 'rustup'
    $cargoCommand = $privateCargo
} else {
    $cargoCommand = (Get-Command cargo -ErrorAction Stop).Source
}
Push-Location -LiteralPath $projectDirectory
try {
    if ($Test) {
        & $cargoCommand test --workspace --locked
        if ($LASTEXITCODE -ne 0) { throw 'Testy nie przeszły.' }
    }
    $buildArguments = @('build', '--workspace', '--locked')
    if ($Release) { $buildArguments += '--release' }
    & $cargoCommand @buildArguments
    if ($LASTEXITCODE -ne 0) { throw 'Kompilacja nie powiodła się.' }
} finally { Pop-Location }
