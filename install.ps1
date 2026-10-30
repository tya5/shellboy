# Compatible with Windows PowerShell 5.1 and PowerShell 7.
& {
    $ErrorActionPreference = 'Stop'
    $version = $env:SHELLBOY_VERSION
    if (-not $version) { $version = '__SHELLBOY_RELEASE_VERSION__' }
    # PROCESSOR_ARCHITEW6432 identifies the native CPU under 32-bit PowerShell.
    $arch = $env:PROCESSOR_ARCHITEW6432
    if (-not $arch) { $arch = $env:PROCESSOR_ARCHITECTURE }
    if ([Environment]::OSVersion.Platform -ne 'Win32NT' -or $arch -ne 'AMD64') {
        throw "Unsupported OS/CPU: $([Environment]::OSVersion.Platform)/$arch. No release archive is available."
    }
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
    if ($version -notmatch '^v') { $version = "v$version" }
    if ($version -cnotmatch '\Av[0-9]+\.[0-9]+\.[0-9]+\z') { throw 'Use a release installer asset or set SHELLBOY_VERSION to a stable version such as v0.3.0.' }
    $archive = "shellboy-$version-x86_64-pc-windows-msvc.zip"
    $base = "https://github.com/tya5/shellboy/releases/download/$version"
    $tmp = Join-Path ([IO.Path]::GetTempPath()) ([IO.Path]::GetRandomFileName())
    $staged = $null
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Invoke-WebRequest -UseBasicParsing "$base/$archive" -OutFile (Join-Path $tmp $archive)
        Invoke-WebRequest -UseBasicParsing "$base/SHA256SUMS" -OutFile (Join-Path $tmp 'SHA256SUMS')
        $entries = @(Get-Content (Join-Path $tmp 'SHA256SUMS') | Where-Object {
            $_ -match ('^[0-9a-fA-F]{64}  ' + [regex]::Escape($archive) + '$')
        })
        if ($entries.Count -ne 1) { throw 'Missing or invalid SHA256SUMS entry.' }
        $expected = $entries[0].Substring(0, 64)
        if ((Get-FileHash (Join-Path $tmp $archive) -Algorithm SHA256).Hash -ne $expected) {
            throw 'SHA-256 mismatch; nothing installed.'
        }
        Expand-Archive -LiteralPath (Join-Path $tmp $archive) -DestinationPath $tmp
        $binary = Join-Path $tmp "shellboy-$version-x86_64-pc-windows-msvc/shellboy.exe"
        $installedVersion = & $binary --version
        if ($LASTEXITCODE -ne 0 -or $installedVersion -ne "shellboy $($version.Substring(1))") {
            throw 'Archive binary version does not match the requested version.'
        }
        $dir = $env:SHELLBOY_INSTALL_DIR
        if (-not $dir) { $dir = Join-Path $env:LOCALAPPDATA 'Programs/shellboy' }
        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $destination = Join-Path $dir 'shellboy.exe'
        $staged = Join-Path $dir ([IO.Path]::GetRandomFileName())
        Copy-Item -LiteralPath $binary -Destination $staged
        $backup = $null
        if (Test-Path -LiteralPath $destination) {
            $backup = "$destination.backup-$([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ'))-$([Guid]::NewGuid().ToString('N'))"
            # Windows permits renaming a running image, but not overwriting it.
            [IO.File]::Move($destination, $backup)
            Write-Output "Previous binary saved to $backup"
        }
        try {
            [IO.File]::Move($staged, $destination)
        } catch {
            $installError = $_
            if ($backup) {
                try { [IO.File]::Move($backup, $destination) } catch {
                    throw "Installation failed and rollback failed. Previous binary remains at '$backup'. Restore it manually after resolving the destination error. $($_.Exception.Message)"
                }
            }
            throw $installError
        }
        Write-Output "Installed $installedVersion to $destination"
        if (($env:PATH -split ';') -notcontains $dir) {
            Write-Output "Add '$dir' to your user PATH in Environment Variables, then reopen your terminal. No profile or PATH settings were changed."
        }
        Write-Output 'If an owner is running, restart it to use the new binary.'
        Write-Output 'Before first startup: stop the owner and back up the data directory.'
        Write-Output 'First startup may upgrade databases; older versions cannot open them.'
        Write-Output 'First setup: shellboy config init -> edit the reported config path and set api_key_env -> shellboy config check -> shellboy'
        Write-Output 'Keep API key values in the environment, not the config file. Use the same --config path for every step.'
        Write-Output 'Guide: shellboy docs get --id shellboy/configuration'
        Write-Output 'For older releases without config init/check, follow the bundled configuration guide manually.'
        Write-Output "Read upgrade details: https://github.com/tya5/shellboy/releases/tag/$version"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force
        if ($staged -and (Test-Path -LiteralPath $staged)) { Remove-Item -LiteralPath $staged -Force }
    }
}
