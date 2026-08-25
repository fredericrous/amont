<#
.SYNOPSIS
    Install the amont binary on Windows. Nothing else.

.DESCRIPTION
    irm https://raw.githubusercontent.com/fredericrous/amont/main/install/install.ps1 | iex

    The counterpart to install/install.sh, which is POSIX sh and therefore
    reaches Windows only through Git Bash. This one runs in the PowerShell
    that ships with Windows, so `curl | sh` stops being the only documented
    path on a platform the hooks genuinely support and CI genuinely tests.

    Like its sibling it deliberately turns NOTHING on. It downloads a verified
    binary, puts it where the shims already look, and says what to run next.
    An installer that enabled hooks — or set init.templateDir so every future
    clone got them — would contradict the project's central promise on first
    contact with the machine.

.PARAMETER Version
    A specific release, e.g. "v1.0.0". Defaults to the latest.

.PARAMETER BinDir
    Where to put the executables. Defaults to $HOME\.local\bin, and not
    arbitrarily: that is candidate 3 in the shim's own resolution order, and
    the shim tries both `amont` and `amont.exe` there — so a binary in
    that directory is found even by a shim whose path was never baked.
#>
[CmdletBinding()]
param(
    [string]$Version = $env:AMONT_VERSION,
    [string]$BinDir  = $env:AMONT_BIN_DIR
)

$ErrorActionPreference = 'Stop'
Set-StrictMode -Version Latest

$Repo = 'fredericrous/amont'
if (-not $BinDir)  { $BinDir  = Join-Path $HOME '.local\bin' }
if (-not $Version) { $Version = 'latest' }

function Write-Ok   ($m) { Write-Host "  [ok] $m"   -ForegroundColor Green }
function Write-Warn ($m) { Write-Host "  [!]  $m"   -ForegroundColor Yellow }
function Fail       ($m) { Write-Host "  [x]  $m"   -ForegroundColor Red; exit 1 }

# Windows PowerShell 5.1 still negotiates TLS 1.0 by default, which GitHub
# refuses outright — the download fails with a connection error that says
# nothing about the cause. PowerShell 7 already defaults to TLS 1.2+.
try {
    [Net.ServicePointManager]::SecurityProtocol =
        [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12
} catch {
    # Not fatal: PS7 has no mutable ServicePointManager and does not need one.
}

Write-Host ''
Write-Host '  amont installer'
Write-Host ''

# Only x86_64 is published today. Windows on ARM runs x64 binaries through
# emulation, so this is a working install rather than a refusal — said out
# loud, because a silently emulated binary is a surprise worth naming.
$arch = $env:PROCESSOR_ARCHITECTURE
$target = 'x86_64-pc-windows-msvc'
if ($arch -eq 'ARM64') {
    Write-Warn 'ARM64 detected — installing the x64 build, which Windows runs under emulation.'
}

if ($Version -eq 'latest') {
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$Repo/releases/latest" `
            -Headers @{ 'User-Agent' = 'amont-installer' }
        $Version = $release.tag_name
    } catch {
        Fail "could not determine the latest release (rate limited? pass -Version v1.2.3): $_"
    }
}
$v = $Version -replace '^v', ''

$name = "amont-$v-$target"
$base = "https://github.com/$Repo/releases/download/v$v"

Write-Host "  version:  $v"
Write-Host "  platform: $target"
Write-Host "  into:     $BinDir"
Write-Host ''

$tmp = Join-Path ([System.IO.Path]::GetTempPath()) ("amont-" + [guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $tmp -Force | Out-Null

try {
    $zip = Join-Path $tmp "$name.zip"
    Write-Host '  downloading...'
    try {
        Invoke-WebRequest -Uri "$base/$name.zip" -OutFile $zip -UseBasicParsing
    } catch {
        Fail "download failed: $base/$name.zip"
    }

    # Checksums are not optional here. This binary runs on every commit with
    # your credentials and reads every staged file; verifying what it is
    # before putting it in that position is the argument the project makes
    # about its own dependencies, applied to itself.
    $sums = Join-Path $tmp 'SHA256SUMS'
    try {
        Invoke-WebRequest -Uri "$base/SHA256SUMS" -OutFile $sums -UseBasicParsing
    } catch {
        $sums = $null
        Write-Warn 'no SHA256SUMS published for this release - the download was NOT verified'
    }

    if ($sums) {
        $got  = (Get-FileHash -Path $zip -Algorithm SHA256).Hash.ToLower()
        $line = Select-String -Path $sums -Pattern ([regex]::Escape("$name.zip")) |
                Select-Object -First 1
        if (-not $line) { Fail "SHA256SUMS has no entry for $name.zip" }
        $want = ($line.Line -split '\s+')[0].ToLower()
        if ($got -ne $want) {
            Fail "checksum mismatch - refusing to install`n    expected $want`n    got      $got"
        }
        Write-Ok 'checksum verified'
    }

    Expand-Archive -Path $zip -DestinationPath $tmp -Force
    $src = Join-Path $tmp $name
    # Whether this is an upgrade decides what to say at the end: the
    # first-install steps are wrong advice the second time.
    $upgrading = Test-Path (Join-Path $BinDir 'amont.exe')
    New-Item -ItemType Directory -Path $BinDir -Force | Out-Null

    foreach ($exe in @('amont.exe', 'amont-fleet.exe', 'amont-agent.exe')) {
        $from = Join-Path $src $exe
        if (Test-Path $from) {
            $to = Join-Path $BinDir $exe
            # Copy beside the destination and move it into place: replacing a
            # RUNNING executable in place fails on Windows with a sharing
            # violation, and a move is atomic, so a half-copied amont.exe
            # never exists.
            $staged = "$to.new"
            Copy-Item -Path $from -Destination $staged -Force
            Move-Item -Path $staged -Destination $to -Force
            Write-Ok "installed $to"
        }
    }
} finally {
    Remove-Item -Path $tmp -Recurse -Force -ErrorAction SilentlyContinue
}

Write-Host ''

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
if ($userPath -notlike "*$BinDir*") {
    Write-Warn "$BinDir is not on your PATH."
    Write-Host '       The shims find the binary there regardless; this is so YOU can run it:'
    Write-Host ''
    Write-Host "         [Environment]::SetEnvironmentVariable('Path', `"`$env:Path;$BinDir`", 'User')"
    Write-Host ''
}

if ($upgrading) {
    Write-Host '  Upgraded. Hooks already run this binary — nothing per repository'
    Write-Host '  needs redoing for the checks themselves. What does not update on its'
    Write-Host "  own: a repository's hook shims when a release changes them, and the"
    Write-Host '  generated block in AGENTS.md/CLAUDE.md. One command sees both:'
    Write-Host ''
    Write-Host '    amont-fleet fix --root $HOME\source                       # dry run'
    Write-Host '    amont-fleet fix --root $HOME\source --apply --agents-md'
    Write-Host ''
    Write-Host '  or, in one repository:  amont install; amont agents-md'
    Write-Host ''
} else {
    Write-Host '  Nothing is enabled yet, on purpose. To turn the hooks on:'
    Write-Host ''
    Write-Host '    cd <your-repo>; amont install     # this repository only'
    Write-Host '    amont list                        # what would run here'
    Write-Host '    amont uninstall                   # and back out again'
    Write-Host ''
    Write-Host '  Across many repositories at once:  amont-fleet install --root $HOME\source'
    Write-Host ''
}
