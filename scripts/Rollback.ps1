$ErrorActionPreference = 'Stop'
$installRoot = Join-Path $env:LOCALAPPDATA 'RustBrowser'
$current = Join-Path $installRoot 'current.json'
$previous = Join-Path $installRoot 'previous.json'
if (-not (Test-Path -LiteralPath $previous)) { throw 'Nie ma jeszcze poprzedniego wydania do przywrócenia.' }
$metadata = Get-Content -LiteralPath $previous -Raw | ConvertFrom-Json
if ($metadata.product -ne 'RustBrowser' -or $metadata.profileSchema -ne 1) { throw 'Nieobsługiwana wersja lub format profilu.' }
$target = [IO.Path]::GetFullPath((Join-Path $installRoot $metadata.executable))
$versionsRoot = [IO.Path]::GetFullPath((Join-Path $installRoot 'versions')) + [IO.Path]::DirectorySeparatorChar
if (-not $target.StartsWith($versionsRoot,[StringComparison]::OrdinalIgnoreCase) -or -not (Test-Path -LiteralPath $target)) { throw 'Nieprawidłowa ścieżka pliku poprzedniej wersji.' }
if ((Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash -ne $metadata.sha256) { throw 'Plik poprzedniej wersji został zmieniony.' }
$pending = Join-Path $installRoot 'rollback.pending'
[IO.File]::WriteAllText($pending,(Get-Content -LiteralPath $previous -Raw),[Text.UTF8Encoding]::new($false))
[IO.File]::Replace($pending,$current,(Join-Path $installRoot 'rollback.backup'))
Copy-Item -LiteralPath (Join-Path $installRoot 'rollback.backup') -Destination $previous -Force
Write-Output ('Przywrócono ' + $metadata.buildId + '. Zamknij otwartą przeglądarkę i uruchom ją ponownie przez ikonę.')
