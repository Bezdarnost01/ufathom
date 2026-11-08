<#
.SYNOPSIS
    Collects the release binaries into upload-ready assets.

.DESCRIPTION
    Locates the built files through `cargo metadata` instead of hard-coding names:
    the bin target of package ufathom-cli and the cdylib target of package ufathom-dll.
    Fails if either package or target is missing, or the built file does not exist.

    Assets written to -OutDir (which must be empty or absent):
      ufathom-<tag>-<target>.zip   exe, dll, README.md, LICENSE-*, THIRD_PARTY.md, CHANGELOG.md
      ufathom.exe, ufathom.dll     the raw binaries
      SHA256SUMS                   sha256sum format, covering every asset above

    The DLL is always published as ufathom.dll, whatever the cdylib's file name is, so
    download links and documentation stay stable across a rename of the library target.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Tag,
    [Parameter(Mandatory)][string]$Target,
    [Parameter(Mandatory)][string]$OutDir,
    [string]$Root = '.'
)

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.IO.Compression
Add-Type -AssemblyName System.IO.Compression.FileSystem

$Root = (Resolve-Path -LiteralPath $Root).Path
Push-Location $Root
try {
    $metadataText = (& cargo metadata --format-version 1 --no-deps --locked) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw 'cargo metadata failed.' }
} finally {
    Pop-Location
}
$metadata = $metadataText | ConvertFrom-Json
$releaseDir = Join-Path (Join-Path $metadata.target_directory $Target) 'release'

function Find-Target([string]$PackageName, [string]$Kind) {
    $package = @($metadata.packages | Where-Object { $_.name -eq $PackageName })
    if ($package.Count -ne 1) {
        throw "Package '$PackageName' not found in the workspace (cargo metadata)."
    }
    $found = @($package[0].targets | Where-Object { $_.kind -contains $Kind })
    if ($found.Count -ne 1) {
        throw "Package '$PackageName' must have exactly one '$Kind' target, found $($found.Count)."
    }
    return $found[0].name
}

# Cargo replaces '-' with '_' in a library file name; a bin keeps its name.
$exeName = (Find-Target 'ufathom-cli' 'bin') + '.exe'
$dllName = ((Find-Target 'ufathom-dll' 'cdylib') -replace '-', '_') + '.dll'
$exeSource = Join-Path $releaseDir $exeName
$dllSource = Join-Path $releaseDir $dllName
foreach ($path in @($exeSource, $dllSource)) {
    if (-not (Test-Path -LiteralPath $path -PathType Leaf)) {
        throw "Expected build output is missing: $path"
    }
}
Write-Host "CLI: $exeSource"
Write-Host "DLL: $dllSource"

$docs = @('README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY.md', 'CHANGELOG.md')
foreach ($doc in $docs) {
    if (-not (Test-Path -LiteralPath (Join-Path $Root $doc) -PathType Leaf)) {
        throw "Required file is missing from the repository root: $doc"
    }
}

New-Item -ItemType Directory -Force -Path $OutDir | Out-Null
$OutDir = (Resolve-Path -LiteralPath $OutDir).Path
if (@(Get-ChildItem -LiteralPath $OutDir -Force).Count -ne 0) {
    throw "Output directory is not empty: $OutDir"
}

$zipName = "ufathom-$Tag-$Target.zip"
$zipPath = Join-Path $OutDir $zipName
$topDir = "ufathom-$Tag-$Target"
$entries = [ordered]@{
    'ufathom.exe' = $exeSource
    'ufathom.dll' = $dllSource
}
foreach ($doc in $docs) { $entries[$doc] = Join-Path $Root $doc }

# Entry names are written explicitly with '/' so the archive is portable.
$zip = [System.IO.Compression.ZipFile]::Open($zipPath, [System.IO.Compression.ZipArchiveMode]::Create)
try {
    foreach ($name in $entries.Keys) {
        [void][System.IO.Compression.ZipFileExtensions]::CreateEntryFromFile(
            $zip, $entries[$name], "$topDir/$name", [System.IO.Compression.CompressionLevel]::Optimal)
    }
} finally {
    $zip.Dispose()
}

Copy-Item -LiteralPath $exeSource -Destination (Join-Path $OutDir 'ufathom.exe')
Copy-Item -LiteralPath $dllSource -Destination (Join-Path $OutDir 'ufathom.dll')

$sumLines = foreach ($name in @($zipName, 'ufathom.exe', 'ufathom.dll')) {
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath (Join-Path $OutDir $name)).Hash.ToLowerInvariant()
    "$hash  $name"
}
$sumText = ($sumLines -join "`n") + "`n"
[System.IO.File]::WriteAllText((Join-Path $OutDir 'SHA256SUMS'), $sumText, (New-Object System.Text.UTF8Encoding($false)))

Write-Host "Assets in ${OutDir}:"
Get-ChildItem -LiteralPath $OutDir | ForEach-Object { Write-Host ("  {0}  ({1} bytes)" -f $_.Name, $_.Length) }
