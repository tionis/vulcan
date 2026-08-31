[CmdletBinding()]
param(
    [Parameter(Mandatory = $false)]
    [string]$Version = $env:VULCAN_VERSION,
    [Parameter(Mandatory = $false)]
    [string]$Prefix = $env:VULCAN_INSTALL_PREFIX,
    [Parameter(Mandatory = $false)]
    [string]$BaseUrl = $env:VULCAN_RELEASE_BASE_URL,
    [switch]$AddToPath,
    [switch]$DryRun
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($Version) -or $Version -notmatch '^[0-9A-Za-z.+-]+$') {
    throw 'A valid -Version is required.'
}
if ([string]::IsNullOrWhiteSpace($Prefix)) {
    $Prefix = Join-Path $env:LOCALAPPDATA 'Programs\Vulcan'
}
if ([string]::IsNullOrWhiteSpace($BaseUrl)) {
    $BaseUrl = "https://github.com/tionis/vulcan/releases/download/v$Version"
}

$architecture = [System.Runtime.InteropServices.RuntimeInformation]::OSArchitecture
switch ($architecture) {
    'X64' { $target = 'x86_64-pc-windows-msvc' }
    default { throw "Unsupported Windows architecture: $architecture" }
}
$archive = "vulcan-$Version-$target.zip"

Write-Output "Version: $Version"
Write-Output "Target: $target"
Write-Output "Prefix: $Prefix"
Write-Output "Archive: $BaseUrl/$archive"
if ($DryRun) {
    Write-Output 'Dry run: no files were downloaded or installed.'
    return
}

$temporary = Join-Path ([System.IO.Path]::GetTempPath()) ("vulcan-install-" + [guid]::NewGuid())
New-Item -ItemType Directory -Path $temporary | Out-Null
try {
    $archivePath = Join-Path $temporary $archive
    $checksumsPath = Join-Path $temporary 'SHA256SUMS'
    Invoke-WebRequest -Uri "$BaseUrl/$archive" -OutFile $archivePath
    Invoke-WebRequest -Uri "$BaseUrl/SHA256SUMS" -OutFile $checksumsPath
    $checksumLine = Get-Content $checksumsPath | Where-Object { $_ -match "^[0-9a-fA-F]{64}\s+$([regex]::Escape($archive))$" }
    if (@($checksumLine).Count -ne 1) {
        throw "Expected exactly one published checksum for $archive."
    }
    $expected = ($checksumLine -split '\s+')[0].ToLowerInvariant()
    $actual = (Get-FileHash -Algorithm SHA256 $archivePath).Hash.ToLowerInvariant()
    if ($actual -ne $expected) {
        throw "Checksum mismatch for $archive."
    }
    Expand-Archive -Path $archivePath -DestinationPath $temporary
    $root = Join-Path $temporary "vulcan-$Version-$target"
    $bin = Join-Path $Prefix 'bin'
    New-Item -ItemType Directory -Force -Path $bin | Out-Null
    $staged = Join-Path $bin 'vulcan.exe.new'
    Copy-Item (Join-Path $root 'vulcan.exe') $staged -Force
    Move-Item $staged (Join-Path $bin 'vulcan.exe') -Force

    if ($AddToPath) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $entries = @($userPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
        if ($entries -notcontains $bin) {
            [Environment]::SetEnvironmentVariable('Path', (($entries + $bin) -join ';'), 'User')
        }
    }
} finally {
    Remove-Item -Recurse -Force $temporary -ErrorAction SilentlyContinue
}

Write-Output "Installed Vulcan $Version at $Prefix\bin\vulcan.exe."
Write-Output 'The daemon was not enabled. Run `vulcan daemon install --dry-run` to review it.'
