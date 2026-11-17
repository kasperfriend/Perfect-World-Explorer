param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [Parameter(Mandatory = $true)]
    [string]$Modules,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [string]$ArchivePath = '',

    [string]$DirectStorageSdk = ''
)

$ErrorActionPreference = 'Stop'
if ([string]::IsNullOrWhiteSpace($DirectStorageSdk)) {
    if ($env:DIRECTSTORAGE_SDK_DIR) {
        $DirectStorageSdk = $env:DIRECTSTORAGE_SDK_DIR
    }
    else {
        $DirectStorageSdk = Join-Path $PSScriptRoot '..\sdk\DirectStorage\1.4.0-preview2-2606.904'
    }
}

$executablePath = (Resolve-Path -LiteralPath $Executable).Path
$modulesPath = (Resolve-Path -LiteralPath $Modules).Path
if (-not (Test-Path -LiteralPath $executablePath -PathType Leaf)) {
    throw "Viewer executable not found: $executablePath"
}
if (-not (Test-Path -LiteralPath (Join-Path $modulesPath 'Walker/module.ini') -PathType Leaf)) {
    throw "Walker module manifest not found under: $modulesPath"
}

$outputPath = [System.IO.Path]::GetFullPath($OutputDirectory)
$packageRoot = Join-Path $outputPath 'Perfect World Explorer'
if (Test-Path -LiteralPath $packageRoot) {
    Remove-Item -LiteralPath $packageRoot -Recurse -Force
}
New-Item -ItemType Directory -Force -Path $packageRoot | Out-Null

$shippedExecutable = Join-Path $packageRoot 'Perfect-World-Explorer.exe'
Copy-Item -LiteralPath $executablePath -Destination $shippedExecutable

# Ship the optional x64 DirectStorage runtime beside the executable, where
# LoadLibraryExW's application-directory search can find it. If the SDK headers
# are present but native/bin was omitted (for example, before the DLL ignore
# rule was fixed), the viewer remains valid and falls back to ordinary I/O.
$directStorageRuntime = Join-Path $DirectStorageSdk 'native/bin/x64'
$dstorageDll = Join-Path $directStorageRuntime 'dstorage.dll'
$dstorageCoreDll = Join-Path $directStorageRuntime 'dstoragecore.dll'
if ((Test-Path -LiteralPath $dstorageDll -PathType Leaf) -and
    (Test-Path -LiteralPath $dstorageCoreDll -PathType Leaf)) {
    Copy-Item -LiteralPath $dstorageDll -Destination $packageRoot
    Copy-Item -LiteralPath $dstorageCoreDll -Destination $packageRoot
    $directStorageNotices = Join-Path $packageRoot 'DirectStorage'
    New-Item -ItemType Directory -Force -Path $directStorageNotices | Out-Null
    foreach ($notice in @('LICENSE.txt', 'NOTICES.txt')) {
        $noticePath = Join-Path $DirectStorageSdk $notice
        if (-not (Test-Path -LiteralPath $noticePath -PathType Leaf)) {
            throw "DirectStorage runtime is present but its required notice is missing: $noticePath"
        }
        Copy-Item -LiteralPath $noticePath -Destination $directStorageNotices
    }
}
else {
    Write-Warning "DirectStorage runtime DLLs not found under $directStorageRuntime; package will use standard file I/O."
}

$shippedModules = Join-Path $packageRoot 'Modules'
Copy-Item -LiteralPath $modulesPath -Destination $shippedModules -Recurse

$walkerManifest = Join-Path $shippedModules 'Walker/module.ini'
if (-not (Test-Path -LiteralPath $walkerManifest -PathType Leaf)) {
    throw "Staging did not produce Modules/Walker/module.ini in $packageRoot"
}

# Keep checksums with the portable folder, including optional DirectStorage
# runtime binaries and their notices. No development/survey executables are
# inputs to this script.
$files = @(Get-ChildItem -LiteralPath $packageRoot -Recurse -File |
    Where-Object { $_.FullName -ne (Join-Path $packageRoot 'SHA256SUMS.txt') } |
    Sort-Object FullName)
$checksumLines = @(
    foreach ($file in $files) {
        $hash = (Get-FileHash -LiteralPath $file.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $relative = [System.IO.Path]::GetRelativePath($packageRoot, $file.FullName).Replace('\', '/')
        "$hash  $relative"
    }
)
[System.IO.File]::WriteAllLines(
    (Join-Path $packageRoot 'SHA256SUMS.txt'),
    [string[]]$checksumLines,
    [System.Text.Encoding]::ASCII
)

if (-not [string]::IsNullOrWhiteSpace($ArchivePath)) {
    $archiveFullPath = [System.IO.Path]::GetFullPath($ArchivePath)
    $archiveDirectory = Split-Path -Parent $archiveFullPath
    New-Item -ItemType Directory -Force -Path $archiveDirectory | Out-Null
    if (Test-Path -LiteralPath $archiveFullPath) {
        Remove-Item -LiteralPath $archiveFullPath -Force
    }

    # No trailing wildcard: Compress-Archive retains the top-level
    # "Perfect World Explorer" directory in the release ZIP.
    Compress-Archive -LiteralPath $packageRoot -DestinationPath $archiveFullPath -CompressionLevel Optimal

    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $archive = [System.IO.Compression.ZipFile]::OpenRead($archiveFullPath)
    try {
        $entries = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') })
        foreach ($required in @(
            'Perfect World Explorer/Perfect-World-Explorer.exe',
            'Perfect World Explorer/Modules/Walker/module.ini',
            'Perfect World Explorer/Modules/Populator/module.ini',
            'Perfect World Explorer/SHA256SUMS.txt'
        )) {
            if ($entries -notcontains $required) {
                throw "Release archive is missing required entry: $required"
            }
        }
        if ($entries | Where-Object { $_ -match '(?i)bmd.*survey|survey.*bmd' }) {
            throw 'A BMD survey executable was unexpectedly included in the release archive.'
        }
    }
    finally {
        $archive.Dispose()
    }

    $archiveHash = (Get-FileHash -LiteralPath $archiveFullPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $archiveName = [System.IO.Path]::GetFileName($archiveFullPath)
    [System.IO.File]::WriteAllLines(
        (Join-Path $archiveDirectory 'SHA256SUMS.txt'),
        [string[]]@("$archiveHash  $archiveName"),
        [System.Text.Encoding]::ASCII
    )
}

Write-Host "Staged portable viewer at $packageRoot"
