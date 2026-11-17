[CmdletBinding()]
param(
    [ValidateSet('1.4.0-preview1-2603.504', '1.4.0-preview2-2606.904')]
    [string]$Version = '1.4.0-preview2-2606.904',
    [switch]$Force
)

$ErrorActionPreference = 'Stop'
$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$packageId = 'microsoft.direct3d.directstorage'
$packageName = 'Microsoft.Direct3D.DirectStorage'
$destination = Join-Path $repoRoot "sdk/DirectStorage/$Version"
$expected = @(
    'native/include/dstorage.h',
    'native/bin/x64/dstorage.dll',
    'native/bin/x64/dstoragecore.dll',
    'LICENSE.txt',
    'LICENSE-CODE.txt'
)

if (-not $Force -and (Test-Path (Join-Path $destination 'native/include/dstorage.h'))) {
    foreach ($relative in $expected) {
        if (-not (Test-Path (Join-Path $destination $relative))) {
            throw "Existing DirectStorage package is incomplete ($relative missing). Re-run with -Force."
        }
    }
    Write-Host "DirectStorage $Version is already extracted at $destination"
    exit 0
}

$tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("pwe-dstorage-" + [guid]::NewGuid().ToString('N'))
$zipPath = Join-Path $tempRoot 'package.zip'
$unpackPath = Join-Path $tempRoot 'unpacked'
$download = "https://api.nuget.org/v3-flatcontainer/$packageId/$Version/$packageId.$Version.nupkg"

try {
    New-Item -ItemType Directory -Force -Path $unpackPath | Out-Null
    Write-Host "Downloading official NuGet package: $packageName $Version"
    if ($PSVersionTable.PSEdition -eq 'Desktop') {
        [Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
    }
    Invoke-WebRequest -Uri $download -OutFile $zipPath
    if (-not (Test-Path $zipPath) -or (Get-Item $zipPath).Length -lt 100000) {
        throw 'NuGet download is missing or unexpectedly small; refusing to extract it.'
    }
    Expand-Archive -LiteralPath $zipPath -DestinationPath $unpackPath -Force

    foreach ($relative in $expected) {
        if (-not (Test-Path (Join-Path $unpackPath $relative))) {
            throw "The official package did not contain the expected file: $relative"
        }
    }
    $nuspec = Get-ChildItem -LiteralPath $unpackPath -Filter '*.nuspec' -File | Select-Object -First 1
    if (-not $nuspec) {
        throw 'The downloaded archive does not contain a NuGet package manifest.'
    }
    [xml]$manifest = Get-Content -LiteralPath $nuspec.FullName
    $metadata = $manifest.package.metadata
    if ($metadata.id -ne $packageName -or $metadata.version -ne $Version) {
        throw "Package identity mismatch: expected $packageName $Version, got $($metadata.id) $($metadata.version)."
    }

    if (Test-Path $destination) {
        Remove-Item -LiteralPath $destination -Recurse -Force
    }
    New-Item -ItemType Directory -Force -Path $destination | Out-Null
    Copy-Item -Path (Join-Path $unpackPath '*') -Destination $destination -Recurse -Force

    Write-Host "Extracted DirectStorage $Version to $destination"
    Write-Host 'The full package and both applicable license files are retained. Review those licenses before redistributing the extracted files.'
}
finally {
    if (Test-Path $tempRoot) {
        Remove-Item -LiteralPath $tempRoot -Recurse -Force
    }
}
