param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [Parameter(Mandatory = $true)]
    [string]$Modules,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [string]$ArchivePath = ''
)

$ErrorActionPreference = 'Stop'

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
$shippedModules = Join-Path $packageRoot 'Modules'
Copy-Item -LiteralPath $modulesPath -Destination $shippedModules -Recurse

$walkerManifest = Join-Path $shippedModules 'Walker/module.ini'
if (-not (Test-Path -LiteralPath $walkerManifest -PathType Leaf)) {
    throw "Staging did not produce Modules/Walker/module.ini in $packageRoot"
}

# Keep checksums with the portable folder. No development/survey executables
# are inputs to this script, so only the viewer and installed module packages
# can enter the distribution.
$files = @((Get-Item -LiteralPath $shippedExecutable)) +
    @((Get-ChildItem -LiteralPath $shippedModules -Recurse -File | Sort-Object FullName))
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

# Make package-size regressions visible in CI/release logs without uploading
# an uncompressed diagnostic manifest. This quickly exposes accidentally
# staged SDK runtimes or other large files before anyone downloads the artifact.
$totalBytes = ($files | Measure-Object -Property Length -Sum).Sum
Write-Host ("Portable payload: {0:N1} MiB across {1} files" -f ($totalBytes / 1MB), $files.Count)
Write-Host 'Largest packaged files:'
foreach ($file in ($files | Sort-Object Length -Descending | Select-Object -First 10)) {
    $relative = [System.IO.Path]::GetRelativePath($packageRoot, $file.FullName).Replace('\', '/')
    Write-Host ("  {0,10:N0} bytes  {1}" -f $file.Length, $relative)
}

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
