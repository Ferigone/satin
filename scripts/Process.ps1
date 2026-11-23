function Invoke-BrowserProcess {
    param(
        [Parameter(Mandatory=$true)][string]$Executable,
        [string[]]$Arguments = @(),
        [Parameter(Mandatory=$true)][string]$LogDirectory,
        [int]$TimeoutMilliseconds = 45000
    )
    New-Item -ItemType Directory -Path $LogDirectory -Force | Out-Null
    $token = [Guid]::NewGuid().ToString('N')
    $stdout = Join-Path $LogDirectory ($token + '.out')
    $stderr = Join-Path $LogDirectory ($token + '.err')
    # Start-Process joins its arguments on Windows. Quote each argument using
    # CommandLineToArgvW rules, including trailing slashes and embedded quotes.
    $quoted = @($Arguments | ForEach-Object {
        $escaped = [regex]::Replace($_, '(\\*)"', '$1$1\"')
        $escaped = [regex]::Replace($escaped, '(\\+)$', '$1$1')
        '"' + $escaped + '"'
    })
    $child = Start-Process -FilePath $Executable -ArgumentList ($quoted -join ' ') -RedirectStandardOutput $stdout -RedirectStandardError $stderr -WindowStyle Hidden -PassThru
    if (-not $child.WaitForExit($TimeoutMilliseconds)) {
        $child.Kill()
        $child.WaitForExit()
        throw ('Przekroczono czas próby procesu ' + $child.Id + '. Zatrzymano wyłącznie ten proces testowy.')
    }
    $child.WaitForExit()
    [pscustomobject]@{
        Id = $child.Id
        ExitCode = $child.ExitCode
        Stdout = [string](Get-Content -LiteralPath $stdout -Raw)
        Stderr = [string](Get-Content -LiteralPath $stderr -Raw)
    }
}
