# Install the conde binary for this Windows user.
#
#   irm https://conde.nativeai.space/install.ps1 | iex
#
# Puts conde.exe in %LOCALAPPDATA%\conde\bin and adds that directory to the user PATH.
# CONDE_DIST_URL overrides the download base; CONDE_INSTALL_DIR the target dir.
# Runs in Windows PowerShell 5.1 and PowerShell 7; never exits the calling shell.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $base = if ($env:CONDE_DIST_URL) { $env:CONDE_DIST_URL.TrimEnd('/') } else { 'https://conde.nativeai.space/dist' }
    $dir = if ($env:CONDE_INSTALL_DIR) { $env:CONDE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'conde\bin' }

    # x64 Windows, or Arm64 Windows 11 running it under x64 emulation; a 32-bit shell on either
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        throw "install.ps1: unsupported CPU $arch; conde ships for x64 Windows"
    }
    $name = 'conde-windows-x86_64.exe'

    $tmp = Join-Path ([IO.Path]::GetTempPath()) ('conde-' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        $version = ''
        try {
            # a blob without a text content type arrives as bytes
            $body = (Invoke-WebRequest -UseBasicParsing -Uri "$base/latest").Content
            if ($body -is [byte[]]) { $body = [Text.Encoding]::UTF8.GetString($body) }
            $version = ([string]$body -split "`n")[0].Trim()
        } catch { }
        Write-Host "downloading $name $version from $base"
        $exe = Join-Path $tmp $name
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/$name" -OutFile $exe
        } catch {
            throw "install.ps1: download failed: $base/$name"
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$base/SHA256SUMS" -OutFile $sums
        } catch {
            throw "install.ps1: download failed: $base/SHA256SUMS; nothing installed"
        }

        # SHA256SUMS is the contract: this file's line ("<hash>  <name>") only
        $want = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            if ($line.TrimEnd("`r") -match ('^([0-9a-fA-F]{64}) [ *]' + [regex]::Escape($name) + '$')) { $want = $Matches[1]; break }
        }
        if (-not $want) { throw "install.ps1: SHA256SUMS has no line for $name; nothing installed" }
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash
        if ($got -ne $want.ToUpperInvariant()) { throw "install.ps1: checksum mismatch for $name; nothing installed" }

        $runs = $false
        try { & $exe --version | Out-Null; $runs = ($LASTEXITCODE -eq 0) } catch { }
        if (-not $runs) { throw "install.ps1: the downloaded binary does not run on this machine ($name)" }

        New-Item -ItemType Directory -Force -Path $dir | Out-Null
        $dest = Join-Path $dir 'conde.exe'
        # binaries moved aside earlier; one a bridge still runs stays locked until it exits
        Get-ChildItem -LiteralPath $dir -Filter 'conde.exe*.old' -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
        if (Test-Path -LiteralPath $dest) {
            # a running bridge locks conde.exe; a locked file can still be renamed, to a fresh name
            Move-Item -LiteralPath $dest -Destination ("$dest." + [guid]::NewGuid().ToString('N') + '.old') -Force
        }
        Move-Item -LiteralPath $exe -Destination $dest -Force
        Write-Host "installed $dest ($(& $dest --version))"
    } finally {
        Remove-Item -LiteralPath $tmp -Recurse -Force -ErrorAction SilentlyContinue
    }

    # the user PATH, edited in the registry so its value kind (and %VARIABLES%) stay as they are
    $key = [Microsoft.Win32.Registry]::CurrentUser.OpenSubKey('Environment', $true)
    $raw = [string]$key.GetValue('Path', '', [Microsoft.Win32.RegistryValueOptions]::DoNotExpandEnvironmentNames)
    $entries = @($raw -split ';' | Where-Object { $_ -ne '' })
    $present = $entries | Where-Object { [Environment]::ExpandEnvironmentVariables($_).TrimEnd('\') -ieq $dir.TrimEnd('\') }
    if (-not $present) {
        $kind = if ($raw) { $key.GetValueKind('Path') } else { [Microsoft.Win32.RegistryValueKind]::ExpandString }
        $key.SetValue('Path', (($entries + $dir) -join ';'), $kind)
        # setting a user variable through .NET broadcasts the change to Explorer and new terminals
        [Environment]::SetEnvironmentVariable('CONDE_PATH_REFRESH', '1', 'User')
        [Environment]::SetEnvironmentVariable('CONDE_PATH_REFRESH', $null, 'User')
        Write-Host ""
        Write-Host "added $dir to your user PATH. This window has it now; open new terminals, and restart"
        Write-Host "running Claude Code and Codex sessions, so their shells find conde."
    }
    $key.Close()
    if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $dir.TrimEnd('\') })) {
        $env:Path = "$env:Path;$dir"
    }

    Write-Host ""
    Write-Host "Next:"
    Write-Host "  conde setup        # this machine's local board, the bridge (a scheduled task at logon), skills and MCP for Claude Code and Codex"
    Write-Host "  conde login --server https://conde.nativeai.space   # then: join the team's board; your human clicks the browser sign-in (publishes nothing)"
}
