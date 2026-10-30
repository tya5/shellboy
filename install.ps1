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
    if ($version.StartsWith('_')) {
        $version = (Invoke-RestMethod 'https://api.github.com/repos/tya5/shellboy/releases/latest').tag_name
    }
    if ($version -notmatch '^v') { $version = "v$version" }
    if ($version -cnotmatch '\Av[0-9]+\.[0-9]+\.[0-9]+\z') { throw 'Use a stable version such as v0.3.0.' }
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
        if (Test-Path -LiteralPath $destination) {
            $backup = "$destination.backup-$([DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ'))-$([Guid]::NewGuid().ToString('N'))"
            Copy-Item -LiteralPath $destination -Destination $backup
            Write-Output "Previous binary saved to $backup"
        }
        Move-Item -LiteralPath $staged -Destination $destination -Force
        Write-Output "Installed $installedVersion to $destination"
        if (($env:PATH -split ';') -notcontains $dir) {
            Write-Output "Add '$dir' to your user PATH in Environment Variables, then reopen your terminal. No profile or PATH settings were changed."
        }
        Write-Output 'If an owner is running, restart it to use the new binary.'
        Write-Output 'Before first startup: stop the owner and back up the data directory.'
        Write-Output 'First startup may upgrade databases; older versions cannot open them.'
        Write-Output "Read upgrade details: https://github.com/tya5/shellboy/releases/tag/$version"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force
        if ($staged -and (Test-Path -LiteralPath $staged)) { Remove-Item -LiteralPath $staged -Force }
    }
}
