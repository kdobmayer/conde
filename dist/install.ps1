# Install the conde binary for this Windows user, in %LOCALAPPDATA%\conde\bin (no admin).
#
#   irm https://conde.cloud/install.ps1 | iex
#
# CONDE_VERSION picks a release (default: the one in `latest`); CONDE_DIST_BASE_URL overrides the
# download base, CONDE_INSTALL_DIR the target directory. Adds the directory to the user PATH.
# Runs in Windows PowerShell 5.1 and PowerShell 7; never exits the calling shell.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12

    $base = if ($env:CONDE_DIST_BASE_URL) { $env:CONDE_DIST_BASE_URL.TrimEnd('/') } else { 'https://storage.googleapis.com/conde-cloud-dist' }
    $dir = if ($env:CONDE_INSTALL_DIR) { $env:CONDE_INSTALL_DIR } else { Join-Path $env:LOCALAPPDATA 'conde\bin' }

    # x64 Windows, or Arm64 Windows 11 running it under x64 emulation; a 32-bit shell on either
    $arch = if ($env:PROCESSOR_ARCHITEW6432) { $env:PROCESSOR_ARCHITEW6432 } else { $env:PROCESSOR_ARCHITECTURE }
    if ($arch -ne 'AMD64' -and $arch -ne 'ARM64') {
        throw "install.ps1: unsupported CPU $arch; conde ships for x64 Windows"
    }
    $name = 'conde-windows-x86_64.exe'

    # a blob without a text content type arrives as bytes
    function Read-Text($uri) {
        $body = (Invoke-WebRequest -UseBasicParsing -Uri $uri).Content
        if ($body -is [byte[]]) { $body = [Text.Encoding]::UTF8.GetString($body) }
        [string]$body
    }

    $version = $env:CONDE_VERSION
    if (-not $version) {
        try { $version = ((Read-Text "$base/latest") -split "`n")[0].Trim() } catch { throw "install.ps1: cannot read $base/latest" }
    }
    $version = $version.Trim() -replace '^v', ''
    if ($version -notmatch '^[0-9]+\.[0-9]+\.[0-9]+([.+-][0-9A-Za-z.+-]+)?$') {
        throw "install.ps1: not a release version: '$version'"
    }
    $url = "$base/v$version"

    # staged next to the target, so the final rename stays on one volume
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $tmp = Join-Path $dir ('.conde-install.' + [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tmp | Out-Null
    try {
        Write-Host "downloading $name v$version from $base"
        $exe = Join-Path $tmp $name
        $sums = Join-Path $tmp 'SHA256SUMS'
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$url/$name" -OutFile $exe
        } catch {
            throw "install.ps1: download failed: $url/$name"
        }
        try {
            Invoke-WebRequest -UseBasicParsing -Uri "$url/SHA256SUMS" -OutFile $sums
        } catch {
            throw "install.ps1: download failed: $url/SHA256SUMS; nothing installed"
        }

        # SHA256SUMS is the contract: this file's line ("<hash>  <name>") only
        $want = $null
        foreach ($line in Get-Content -LiteralPath $sums) {
            if ($line.TrimEnd("`r") -match ('^([0-9a-fA-F]{64}) [ *]' + [regex]::Escape($name) + '$')) { $want = $Matches[1]; break }
        }
        if (-not $want) { throw "install.ps1: SHA256SUMS has no line for $name; nothing installed" }
        $got = (Get-FileHash -Algorithm SHA256 -LiteralPath $exe).Hash
        if ($got -ne $want.ToUpperInvariant()) { throw "install.ps1: checksum mismatch for $name; nothing installed" }

        # the download carries the mark of the web; without it SmartScreen does not stop the exe or the task
        Unblock-File -LiteralPath $exe -ErrorAction SilentlyContinue
        $runs = $false
        try { & $exe --version | Out-Null; $runs = ($LASTEXITCODE -eq 0) } catch { }
        if (-not $runs) {
            throw ("install.ps1: the downloaded binary does not run on this machine ($name). The release is not " +
                "code-signed yet: if Windows Security blocked it (Smart App Control or SmartScreen), allow it there, " +
                "then run this installer again")
        }

        $dest = Join-Path $dir 'conde.exe'
        # binaries moved aside earlier; one a bridge still runs stays locked until it exits
        Get-ChildItem -LiteralPath $dir -Filter 'conde.exe*.old' -ErrorAction SilentlyContinue |
            Remove-Item -Force -ErrorAction SilentlyContinue
        # an upgrade: what the old conde says it is, given at most 5 s (an old binary that hangs must not stop it)
        $upgrade = Test-Path -LiteralPath $dest
        $previous = 'unknown version'
        if ($upgrade) {
            $probe = New-Object Diagnostics.Process
            $probe.StartInfo.FileName = $dest
            $probe.StartInfo.Arguments = '--version'
            $probe.StartInfo.UseShellExecute = $false
            $probe.StartInfo.RedirectStandardOutput = $true
            $probe.StartInfo.CreateNoWindow = $true
            try {
                [void]$probe.Start()
                $read = $probe.StandardOutput.ReadLineAsync()
                if ($probe.WaitForExit(5000) -and $probe.ExitCode -eq 0 -and $read.Wait(1000) -and $read.Result) {
                    $previous = $read.Result.Trim()
                } elseif (-not $probe.HasExited) {
                    $probe.Kill()
                }
            } catch { }
            # never a downgrade by accident (a stale "latest", a second run): an older release only with CONDE_VERSION set
            if (-not $env:CONDE_VERSION -and $previous -match '^conde ([0-9]+\.[0-9]+\.[0-9]+)') {
                try {
                    if ([version]$Matches[1] -gt [version]($version -replace '[.+-][0-9A-Za-z.+-]+$', '')) {
                        Remove-Item -LiteralPath $exe -Force -ErrorAction SilentlyContinue
                        Write-Host "kept $dest ($previous): newer than v$version; set CONDE_VERSION=$version to install it anyway"
                        return
                    }
                } catch { }
            }
            # a running bridge locks conde.exe; a locked file can still be renamed, to a fresh name
            Move-Item -LiteralPath $dest -Destination ("$dest." + [guid]::NewGuid().ToString('N') + '.old') -Force
        }
        Move-Item -LiteralPath $exe -Destination $dest -Force
        $installed = "$(& $dest --version)"
        if ($upgrade) { Write-Host "replaced $dest ($previous) with $installed" } else { Write-Host "installed $dest ($installed)" }
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
    if ($upgrade) {
        Write-Host "Next (an upgrade; your enrollment and names stay as they are, no sign-in):"
        Write-Host "  conde setup        # rewrites what this release changed (skills, MCP entries, the service) and restarts the bridge onto it"
        Write-Host "  conde doctor       # shows: ok bridge: running this build"
    } else {
        Write-Host "Next:"
        Write-Host "  conde login --server https://<workspace>.conde.cloud   # the address in your invite; your human clicks the browser sign-in, then it sets up this machine"
        Write-Host "(No workspace: conde setup sets up this machine with its local board only.)"
    }
}
