<#
.SYNOPSIS
    Verifies a SHA256SUMS file against the files next to it, and the reverse.

.DESCRIPTION
    Every line must be `<64 lowercase hex>  <file name>` and match the file's hash. Every file
    in the directory other than SHA256SUMS itself must be listed. Exits non-zero otherwise.
#>
[CmdletBinding()]
param([Parameter(Mandatory)][string]$Dir)

$ErrorActionPreference = 'Stop'
$Dir = (Resolve-Path -LiteralPath $Dir).Path
$sumsPath = Join-Path $Dir 'SHA256SUMS'
if (-not (Test-Path -LiteralPath $sumsPath -PathType Leaf)) { throw "No SHA256SUMS in $Dir." }

$listed = @{}
foreach ($line in [System.IO.File]::ReadAllLines($sumsPath)) {
    if ($line -notmatch '^(?<hash>[0-9a-f]{64})  (?<name>[^/\\]+)$') {
        throw "Malformed SHA256SUMS line: '$line'"
    }
    $name = $Matches['name']
    $expected = $Matches['hash']
    $file = Join-Path $Dir $name
    if (-not (Test-Path -LiteralPath $file -PathType Leaf)) { throw "Listed file is missing: $name" }
    $actual = (Get-FileHash -Algorithm SHA256 -LiteralPath $file).Hash.ToLowerInvariant()
    if ($actual -ne $expected) { throw "Hash mismatch for ${name}: expected $expected, got $actual" }
    $listed[$name] = $true
}
foreach ($file in Get-ChildItem -LiteralPath $Dir -File) {
    if ($file.Name -ne 'SHA256SUMS' -and -not $listed.ContainsKey($file.Name)) {
        throw "File is not covered by SHA256SUMS: $($file.Name)"
    }
}
Write-Host "SHA256SUMS verified: $($listed.Count) files."
