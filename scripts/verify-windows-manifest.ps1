[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [string]$Binary
)

$ErrorActionPreference = 'Stop'
$binaryPath = (Resolve-Path -LiteralPath $Binary).Path
$manifestTool = Get-Command mt.exe -ErrorAction SilentlyContinue
if ($null -ne $manifestTool) {
    $manifestToolPath = $manifestTool.Source
} else {
    $programFiles = [Environment]::GetEnvironmentVariable('ProgramFiles(x86)')
    $kitsBin = Join-Path $programFiles 'Windows Kits\10\bin'
    $manifestToolPath = Get-ChildItem -LiteralPath $kitsBin -Directory |
        Sort-Object -Property Name -Descending |
        ForEach-Object {
            $candidate = Join-Path $_.FullName 'x64\mt.exe'
            if (Test-Path -LiteralPath $candidate -PathType Leaf) {
                $candidate
            }
        } |
        Select-Object -First 1
}
if ([string]::IsNullOrWhiteSpace($manifestToolPath)) {
    throw 'Could not find mt.exe on PATH or in the installed Windows SDK.'
}

$extracted = Join-Path ([IO.Path]::GetTempPath()) ("vulcan-manifest-" + [guid]::NewGuid() + '.xml')
try {
    & $manifestToolPath -nologo "-inputresource:$binaryPath;#1" "-out:$extracted"
    if ($LASTEXITCODE -ne 0) {
        throw "mt.exe failed with exit code $LASTEXITCODE."
    }
    if ((Get-Content -Raw -LiteralPath $extracted) -notmatch '<consoleAllocationPolicy[^>]*>detached</consoleAllocationPolicy>') {
        throw 'The Windows binary does not embed the detached console-allocation policy.'
    }
} finally {
    Remove-Item -LiteralPath $extracted -Force -ErrorAction SilentlyContinue
}
