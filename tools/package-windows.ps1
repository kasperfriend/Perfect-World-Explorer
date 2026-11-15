param(
    [Parameter(Mandatory = $true)]
    [string]$Executable,

    [Parameter(Mandatory = $true)]
    [string]$Modules,

    [Parameter(Mandatory = $true)]
    [string]$OutputDirectory,

    [string]$ArchivePath = '',

    # Optional: folder containing a *user-supplied* vendor SDK (never part of
# this repository). Only the MIT-licensed AMD FidelityFX/FSR and Intel
# XeSS runtime DLLs (Intel Simplified Software License) are staged, and
# only from this folder. NVIDIA DLSS/Streamline binaries are never staged
# by this script: users who are licensed to redistribute them must stage
# them manually and review the NVIDIA license themselves. See
# THIRD_PARTY_NOTICES.md.
    [string]$SdkDirectory = ''
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

$repoRoot = Split-Path -Parent $PSScriptRoot
$noticesPath = Join-Path $repoRoot 'THIRD_PARTY_NOTICES.md'
if (-not (Test-Path -LiteralPath $noticesPath -PathType Leaf)) {
    throw "THIRD_PARTY_NOTICES.md is missing from the repository root: $noticesPath"
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
Copy-Item -LiteralPath $noticesPath -Destination (Join-Path $packageRoot 'THIRD_PARTY_NOTICES.md')

$walkerManifest = Join-Path $shippedModules 'Walker/module.ini'
if (-not (Test-Path -LiteralPath $walkerManifest -PathType Leaf)) {
    throw "Staging did not produce Modules/Walker/module.ini in $packageRoot"
}
if (-not (Test-Path -LiteralPath (Join-Path $packageRoot 'THIRD_PARTY_NOTICES.md') -PathType Leaf)) {
    throw "Staging did not produce THIRD_PARTY_NOTICES.md in $packageRoot"
}

# Vendored AMD FSR SDK 2.4.0 DX12 runtime (MIT-style licensed; see
# THIRD_PARTY_NOTICES.md and sdk/AMD_FidelityFX-SDK-2.4.0-LICENSE.md). These
# three DLLs are committed to the repository under sdk/ and are staged into
# every package by default unless the caller passes -SdkDirectory (an
# explicit folder then takes precedence) or the DLLs are absent.
$vendoredSdk = Join-Path $repoRoot 'sdk'
if ([string]::IsNullOrWhiteSpace($SdkDirectory) -and (Test-Path -LiteralPath $vendoredSdk -PathType Container)) {
    $vendoredAllowlist = @(
        'amd_fidelityfx_loader_dx12.dll',
        'amd_fidelityfx_upscaler_dx12.dll',
        'amd_fidelityfx_framegeneration_dx12.dll'
    )
    $sdkTarget = Join-Path $packageRoot 'sdk'
    New-Item -ItemType Directory -Force -Path $sdkTarget | Out-Null
    $stagedVendored = 0
    foreach ($name in $vendoredAllowlist) {
        $source = Join-Path $vendoredSdk $name
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            Copy-Item -LiteralPath $source -Destination (Join-Path $sdkTarget $name)
            $stagedVendored++
            Write-Host "Staged vendored AMD FSR binary: sdk/$name"
        }
    }
    if ($stagedVendored -eq 0) {
        Remove-Item -LiteralPath $sdkTarget -Force
        Write-Host "No vendored AMD FSR DLLs found under sdk/; nothing staged."
    } else {
        # Ship the AMD license + third-party notice inside sdk/ so the staged
        # binaries carry their permission notice with them (MIT-style terms).
        foreach ($extra in @('AMD_FidelityFX-SDK-2.4.0-LICENSE.md', 'AMD_FidelityFX-SDK-2.4.0-notices.md')) {
            $extraPath = Join-Path $vendoredSdk $extra
            if (Test-Path -LiteralPath $extraPath -PathType Leaf) {
                Copy-Item -LiteralPath $extraPath -Destination (Join-Path $sdkTarget $extra)
            }
        }
    }
}

# Optional: stage the vendor runtime DLLs from a user-supplied
# SDK folder (licenses: MIT-style for AMD, Intel Simplified Software License for XeSS). Everything else in the folder is ignored; NVIDIA proprietary
# DLLs are refused outright so they can never enter a package by accident.
if (-not [string]::IsNullOrWhiteSpace($SdkDirectory)) {
    $sdkRoot = (Resolve-Path -LiteralPath $SdkDirectory).Path
    # Intel XeSS SDK 3.0.2: libxess.dll (super resolution), libxess_fg.dll
    # (frame generation), libxell.dll (low latency; mandatory for XeSS-FG on
    # every GPU, Intel or not). Staged only from the user-supplied folder;
    # never committed to the repository.
    $amdNames = @(
        'amd_fidelityfx_loader_dx12.dll',
        'amd_fidelityfx_upscaler_dx12.dll',
        'amd_fidelityfx_framegeneration_dx12.dll'
    )
    $intelNames = @(
        'libxess.dll',
        'libxess_fg.dll',
        'libxell.dll'
    )
    $forbidden = @(Get-ChildItem -LiteralPath $sdkRoot -Recurse -File -ErrorAction SilentlyContinue |
        Where-Object { $_.Name -match '(?i)^(nvngx.*|sl\..*)\.dll$' })
    if ($forbidden.Count -gt 0) {
        $names = ($forbidden | ForEach-Object { $_.Name } | Sort-Object) -join ', '
        throw "The SDK folder contains NVIDIA proprietary DLLs ($names). This script never stages them; remove them or review and stage them manually under the NVIDIA DLSS license (see THIRD_PARTY_NOTICES.md)."
    }
    $sdkTarget = Join-Path $packageRoot 'sdk'
    New-Item -ItemType Directory -Force -Path $sdkTarget | Out-Null
    $staged = 0
    $stagedAmd = 0
    $stagedIntel = 0
    foreach ($name in $amdNames) {
        $source = Get-ChildItem -LiteralPath $sdkRoot -Recurse -File -Filter $name -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($null -ne $source) {
            Copy-Item -LiteralPath $source.FullName -Destination (Join-Path $sdkTarget $name)
            $staged++
            $stagedAmd++
            Write-Host "Staged MIT-licensed SDK binary: sdk/$name"
        }
    }
    foreach ($name in $intelNames) {
        $source = Get-ChildItem -LiteralPath $sdkRoot -Recurse -File -Filter $name -ErrorAction SilentlyContinue |
            Select-Object -First 1
        if ($null -ne $source) {
            Copy-Item -LiteralPath $source.FullName -Destination (Join-Path $sdkTarget $name)
            $staged++
            $stagedIntel++
            Write-Host "Staged Intel Simplified Software License SDK binary: sdk/$name"
        }
    }
    if ($staged -eq 0) {
        Remove-Item -LiteralPath $sdkTarget -Force
        Write-Host "No allowlisted vendor SDK DLLs found under $sdkRoot; nothing staged."
    } else {
        # Ship the license of each vendor whose binaries actually entered the
        # package, so the binaries carry their permission notices with them.
        $extras = @()
        if ($stagedAmd -gt 0) {
            $extras += @('AMD_FidelityFX-SDK-2.4.0-LICENSE.md', 'AMD_FidelityFX-SDK-2.4.0-notices.md')
        }
        if ($stagedIntel -gt 0) {
            $extras += @('Intel_XeSS-SDK-3.0.2-LICENSE.txt')
        }
        foreach ($extra in $extras) {
            $extraPath = Join-Path $vendoredSdk $extra
            if (Test-Path -LiteralPath $extraPath -PathType Leaf) {
                Copy-Item -LiteralPath $extraPath -Destination (Join-Path $sdkTarget $extra)
            }
        }
    }
}

# Hard stop regardless of staging mode: NVIDIA proprietary runtime DLLs must
# never enter the package.
$nvidiaBinaries = @(Get-ChildItem -LiteralPath $packageRoot -Recurse -File |
    Where-Object { $_.Name -match '(?i)^(nvngx.*|sl\..*)\.dll$' })
if ($nvidiaBinaries.Count -gt 0) {
    $names = ($nvidiaBinaries | ForEach-Object { $_.FullName }) -join ', '
    throw "Package contains NVIDIA proprietary DLLs: $names"
}

# Keep checksums with the portable folder. No development/survey executables
# are inputs to this script, so only the viewer, the installed module packages,
# the notices file and any explicitly staged MIT SDK DLLs can enter the
# distribution. The checksum list is computed after every staging step above.
$sdkStagedFiles = @()
$sdkDir = Join-Path $packageRoot 'sdk'
if (Test-Path -LiteralPath $sdkDir -PathType Container) {
    $sdkStagedFiles = @(Get-ChildItem -LiteralPath $sdkDir -Recurse -File | Sort-Object FullName)
}
$files = @((Get-Item -LiteralPath $shippedExecutable)) +
    @((Get-ChildItem -LiteralPath $shippedModules -Recurse -File | Sort-Object FullName)) +
    @((Get-Item -LiteralPath (Join-Path $packageRoot 'THIRD_PARTY_NOTICES.md'))) +
    $sdkStagedFiles
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
        $entries = @($archive.Entries | ForEach-Object { $_.FullName.Replace('\', '/') } |
            Where-Object { -not $_.EndsWith('/') })
        foreach ($required in @(
            'Perfect World Explorer/Perfect-World-Explorer.exe',
            'Perfect World Explorer/Modules/Walker/module.ini',
            'Perfect World Explorer/Modules/Populator/module.ini',
            'Perfect World Explorer/SHA256SUMS.txt',
            'Perfect World Explorer/THIRD_PARTY_NOTICES.md'
        )) {
            if ($entries -notcontains $required) {
                throw "Release archive is missing required entry: $required"
            }
        }
        if ($entries | Where-Object { $_ -match '(?i)bmd.*survey|survey.*bmd' }) {
            throw 'A BMD survey executable was unexpectedly included in the release archive.'
        }
        if ($entries | Where-Object { $_ -match '(?i)(nvngx.*|sl\..*)\.dll$' }) {
            throw 'A NVIDIA proprietary DLL was unexpectedly included in the release archive.'
        }
        # The checksum manifest must name exactly the non-manifest files in the archive.
        $manifestEntry = $archive.Entries | Where-Object { $_.FullName.Replace('\', '/') -eq 'Perfect World Explorer/SHA256SUMS.txt' } | Select-Object -First 1
        $manifestReader = New-Object System.IO.StreamReader($manifestEntry.Open())
        try {
            $manifestLines = @($manifestReader.ReadToEnd() -split "`n" | ForEach-Object { $_.Trim() } | Where-Object { $_.Length -gt 0 })
        }
        finally {
            $manifestReader.Dispose()
        }
        $manifestPaths = @($manifestLines | ForEach-Object { ($_ -split '\s+', 2)[1] })
        foreach ($entry in $entries) {
            $relative = $entry.Substring('Perfect World Explorer/'.Length)
            if ($relative -eq 'SHA256SUMS.txt') { continue }
            if ($manifestPaths -notcontains $relative) {
                throw "Release archive entry is not covered by SHA256SUMS.txt: $relative"
            }
        }
        if ($manifestPaths.Count + 1 -ne $entries.Count) {
            throw "SHA256SUMS.txt lists $($manifestPaths.Count) files but the archive contains $($entries.Count) entries including the manifest itself."
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
