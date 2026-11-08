<#
.SYNOPSIS
    Extracts one version's section from CHANGELOG.md for use as release notes.

.DESCRIPTION
    Finds the heading `## [<Version>]`, optionally followed by ` - <date>`, and takes every
    line up to the next `## [` heading. Trailing link-reference definitions
    (`[x]: https://...`) are dropped. Fails if the heading is missing or repeated, or if the
    section has no content other than blank lines and sub-headings. Writes the text to
    -OutFile as UTF-8 without a byte order mark.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Version,
    [string]$Changelog = 'CHANGELOG.md',
    [Parameter(Mandatory)][string]$OutFile
)

$ErrorActionPreference = 'Stop'

$lines = [System.IO.File]::ReadAllLines((Resolve-Path -LiteralPath $Changelog).Path)
$heading = '^##\s+\[' + [regex]::Escape($Version) + '\](\s+-\s+.+)?\s*$'

$starts = @()
for ($i = 0; $i -lt $lines.Length; $i++) {
    if ($lines[$i] -match $heading) { $starts += $i }
}
if ($starts.Count -eq 0) {
    throw "$Changelog has no '## [$Version]' section. Add it before tagging."
}
if ($starts.Count -gt 1) {
    throw "$Changelog has $($starts.Count) '## [$Version]' headings; expected exactly one."
}

$body = New-Object System.Collections.Generic.List[string]
for ($i = $starts[0] + 1; $i -lt $lines.Length; $i++) {
    if ($lines[$i] -match '^##\s+\[') { break }
    $body.Add($lines[$i])
}
while ($body.Count -gt 0 -and $body[$body.Count - 1] -match '^\s*$|^\[[^\]]+\]:\s') {
    $body.RemoveAt($body.Count - 1)
}
while ($body.Count -gt 0 -and $body[0] -match '^\s*$') {
    $body.RemoveAt(0)
}

$hasContent = $false
foreach ($line in $body) {
    if ($line -match '\S' -and $line -notmatch '^\s*#') { $hasContent = $true; break }
}
if (-not $hasContent) {
    throw "The '## [$Version]' section in $Changelog is empty."
}

$text = ($body -join "`n") + "`n"
[System.IO.File]::WriteAllText($OutFile, $text, (New-Object System.Text.UTF8Encoding($false)))
Write-Host "Wrote $($body.Count) lines of release notes for $Version to $OutFile."
