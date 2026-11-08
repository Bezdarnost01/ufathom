<#
.SYNOPSIS
    Checks that a release tag matches the workspace version in Cargo.toml.

.DESCRIPTION
    The tag must be `v<major>.<minor>.<patch>` with an optional `-<pre-release>` suffix,
    and `<version>` must equal `[workspace.package] version` in the root Cargo.toml.
    Prints the version and whether the release is a pre-release (major 0 or a suffix),
    and appends `version=` and `prerelease=` to $env:GITHUB_OUTPUT when it is set.
    Exits non-zero with a clear message on any mismatch.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [string]$CargoToml = 'Cargo.toml'
)

$ErrorActionPreference = 'Stop'

$tagPattern = '^v(?<major>0|[1-9]\d*)\.(?<minor>0|[1-9]\d*)\.(?<patch>0|[1-9]\d*)(?<pre>-[0-9A-Za-z][0-9A-Za-z.-]*)?$'
$match = [regex]::Match($Tag, $tagPattern)
if (-not $match.Success) {
    throw "Tag '$Tag' is not of the form vMAJOR.MINOR.PATCH[-PRERELEASE]."
}
$version = $Tag.Substring(1)

# Read the version out of the [workspace.package] table only.
$inTable = $false
$workspaceVersion = $null
foreach ($line in Get-Content -LiteralPath $CargoToml) {
    if ($line -match '^\s*\[(?<name>[^\]]+)\]\s*(#.*)?$') {
        $inTable = ($Matches['name'].Trim() -eq 'workspace.package')
        continue
    }
    if ($inTable -and $line -match '^\s*version\s*=\s*"(?<v>[^"]+)"\s*(#.*)?$') {
        $workspaceVersion = $Matches['v']
        break
    }
}
if (-not $workspaceVersion) {
    throw "No version found under [workspace.package] in $CargoToml."
}
if ($workspaceVersion -ne $version) {
    throw "Tag '$Tag' expects workspace version '$version', but $CargoToml has '$workspaceVersion'."
}

$prerelease = ($match.Groups['major'].Value -eq '0') -or $match.Groups['pre'].Success
$prereleaseText = $prerelease.ToString().ToLowerInvariant()
Write-Host "Tag $Tag matches workspace version $version (prerelease: $prereleaseText)."

if ($env:GITHUB_OUTPUT) {
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "version=$version"
    Add-Content -LiteralPath $env:GITHUB_OUTPUT -Value "prerelease=$prereleaseText"
}
