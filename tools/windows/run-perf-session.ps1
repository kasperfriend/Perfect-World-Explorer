#Requires -Version 7.0
[CmdletBinding()]
param(
    [string]$KitDirectory = $PSScriptRoot,
    [string]$ClientRoot = 'E:\pw142',
    [string]$OutputDirectory = (Join-Path ([Environment]::GetFolderPath('UserProfile')) 'Downloads'),
    [string]$FsrDll = $env:PW_RAY_FSR_VK_DLL,
    [string]$RrBridge = $env:PW_RAY_RR_BRIDGE,
    [string]$RrSdkDirectory = $env:PW_RAY_RR_SDK_DIR,
    [string]$RrLoaderLib = '',
    [switch]$StartupOnly,
    [ValidateSet('portable','rr','fsr')][string]$StartupProvider = 'portable',
    [ValidateRange(35,50)][int]$RayPercent = 50,
    [ValidateRange(600,3600)][int]$MemorySeconds = 600,
    [ValidateRange(600,7200)][int]$TravelTimeoutSeconds = 2400
)
Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'
if (-not $IsWindows -or [System.Runtime.InteropServices.RuntimeInformation]::ProcessArchitecture.ToString() -ne 'X64') {
    throw 'Use PowerShell 7 x64 on the Windows RTX 3080 PC.'
}

function Hash-File([string]$Path) {
    (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}
function File-Receipt([string]$Path) {
    if (-not $Path -or -not (Test-Path -LiteralPath $Path -PathType Leaf)) { return $null }
    $file = Get-Item -LiteralPath $Path
    return @{ path = $file.FullName; bytes = $file.Length; sha256 = (Hash-File $file.FullName) }
}
function Store-Json([string]$Path, $Object) {
    ConvertTo-Json -InputObject $Object -Depth 16 | Set-Content -Encoding utf8NoBOM -LiteralPath $Path
}
function Utc([DateTimeOffset]$Date) { $Date.ToUniversalTime().ToString('o') }
function In-Tree([string]$Path, [string]$Root) {
    $p = [IO.Path]::TrimEndingDirectorySeparator($Path)
    $r = [IO.Path]::TrimEndingDirectorySeparator($Root)
    return ($p.Equals($r, 'OrdinalIgnoreCase') -or $p.StartsWith($r + [IO.Path]::DirectorySeparatorChar, 'OrdinalIgnoreCase'))
}

$kit = (Resolve-Path -LiteralPath $KitDirectory).ProviderPath
$client = (Resolve-Path -LiteralPath $ClientRoot).ProviderPath
$output = Get-Item -LiteralPath $OutputDirectory
if (-not $output.PSIsContainer) { throw 'Choose an existing output directory.' }
for ($ancestor = $output; $null -ne $ancestor; $ancestor = $ancestor.Parent) {
    if ($ancestor.Attributes -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Choose an output directory without symbolic links, junctions or cloud placeholders.'
    }
}
$scriptParent = Split-Path -Parent $PSScriptRoot
$repo = if ($scriptParent -and (Split-Path -Leaf $scriptParent) -eq 'tools') { Split-Path -Parent $scriptParent } else { '' }
if ((In-Tree $output.FullName $kit) -or (In-Tree $output.FullName $client) -or
    ($repo -and (In-Tree $output.FullName $repo))) {
    throw 'Output must be outside the kit, source checkout and retail client.'
}
$stamp = [DateTime]::UtcNow.ToString('yyyyMMdd-HHmmssZ') + '-' + [Guid]::NewGuid().ToString('N').Substring(0,8)
$root = Join-Path $output.FullName "Lighter-Perf-$stamp"
# No existing files/directories are reused. Runtime scratch and DLLs are not zipped.
[void][IO.Directory]::CreateDirectory($root)
$bundle = Join-Path $root 'bundle'
$scratch = Join-Path $root 'scratch'
[void][IO.Directory]::CreateDirectory($bundle)
[void][IO.Directory]::CreateDirectory($scratch)
$summary = [Collections.Generic.List[string]]::new()
$session = [ordered]@{
    schema = 1; started_utc = (Utc ([DateTimeOffset]::UtcNow)); platform = 'windows'; architecture = 'x86_64'
    machine = $env:COMPUTERNAME; os = [Environment]::OSVersion.VersionString
    powershell = $PSVersionTable.PSVersion.ToString(); client_root = $client; kit_directory = $kit
    startup_provider = $StartupProvider; ray_percent = $RayPercent; memory_goal_s = $MemorySeconds
    display = '2560x1440'; startup_only = [bool]$StartupOnly; runs = [Collections.Generic.List[object]]::new(); prerequisites_ok = $false
}

function Start-Logged([string]$Program, [string[]]$Arguments, [string]$Directory, [string]$LogBase, [hashtable]$Overrides) {
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = $Program
    $info.WorkingDirectory = $Directory
    $info.UseShellExecute = $false
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $info.CreateNoWindow = $true
    foreach ($a in $Arguments) { $info.ArgumentList.Add($a) }
    # Ambient PW/WGPU overrides would make two nominally equal captures differ.
    foreach ($name in @($info.Environment.Keys)) {
        if ($name -match '^(PW_|PWE_|WGPU_|RUST_LOG$|VK_|DXVK_|LP_|GALLIUM_|MESA_)') {
            [void]$info.Environment.Remove($name)
        }
    }
    foreach ($name in $Overrides.Keys) { $info.Environment[$name] = [string]$Overrides[$name] }
    $p = [Diagnostics.Process]::new()
    $p.StartInfo = $info
    $out = [IO.FileStream]::new("$LogBase.stdout.log", 'CreateNew', 'Write', 'Read', 1)
    $err = [IO.FileStream]::new("$LogBase.stderr.log", 'CreateNew', 'Write', 'Read', 1)
    try {
        [void]$p.Start()
        return @{ process = $p; streams = @($out,$err); tasks = @(
            $p.StandardOutput.BaseStream.CopyToAsync($out), $p.StandardError.BaseStream.CopyToAsync($err)
        ) }
    } catch {
        $out.Dispose(); $err.Dispose(); $p.Dispose(); throw
    }
}
function Finish-Logged($Child, [bool]$Stop) {
    if ($null -eq $Child) { return }
    try {
        if ($Stop -and -not $Child.process.HasExited) { $Child.process.Kill($true) }
        if (-not $Child.process.WaitForExit(10000)) { throw 'A capture subprocess could not be reaped.' }
        if (-not [Threading.Tasks.Task]::WaitAll([Threading.Tasks.Task[]]$Child.tasks, 10000)) {
            throw 'Child output did not drain (possible inherited pipe).'
        }
        return $Child.process.ExitCode
    } finally {
        foreach ($s in $Child.streams) { $s.Dispose() }
        $Child.process.Dispose()
    }
}

function Save-Events([string]$Directory, [DateTimeOffset]$Start, [DateTimeOffset]$End) {
    $statuses = [ordered]@{}
    foreach ($name in @('Application','System')) {
        $path = Join-Path $Directory "events-$name.txt"
        $pattern = if ($name -eq 'System') { '^(Display|nvlddmkm)$' } else { '^(Application Error|Windows Error Reporting|Application Hang)$' }
        $status = @{ query = 'ok'; count = 0; from_utc = (Utc $Start); through_utc = (Utc $End) }
        try {
            $hits = @(Get-WinEvent -FilterHashtable @{ LogName = $name; StartTime = $Start.LocalDateTime; EndTime = $End.LocalDateTime } -ErrorAction Stop |
                Where-Object { $_.ProviderName -match $pattern })
        } catch {
            $hits = @()
            if ($_.FullyQualifiedErrorId -notmatch '^NoMatchingEventsFound') {
                $status.query = 'error'; $status.error = $_.Exception.Message
            }
        }
        $status.count = $hits.Count
        "Window: $(Utc $Start) through $(Utc $End) (UTC); query=$($status.query); count=$($hits.Count)" | Set-Content -Encoding utf8NoBOM -LiteralPath $path
        if ($status.query -eq 'error') { Add-Content -LiteralPath $path -Value $status.error }
        foreach ($evt in $hits) {
            $message = $evt.ToXml()
            try { $message += "`n" + $evt.FormatDescription() } catch { $message += "`n[message formatting unavailable]" }
            Add-Content -Encoding utf8NoBOM -LiteralPath $path -Value ("`n" + $message)
        }
        $statuses[$name] = $status
    }
    return $statuses
}

function Make-Settings([string]$Mode, [bool]$Hybrid) {
    @"
mode=$Mode
hybrid_primary=$($Hybrid.ToString().ToLowerInvariant())
resolution=$RayPercent
samples=1
bounces=1
lighting=true
soft_sun=true
shadows=true
reflections=true
progressive=false
reference=false
single_path=true
denoiser=true
dynamic_resolution=false
fsr1_easu=false
physical_bsdf=false
"@
}

function Invoke-Run {
    param([string]$Id, [string]$Kind, [string]$Mode = 'hardware', [bool]$Hybrid = $false,
          [string]$Provider = 'portable', [string]$Policy = '', [string]$Skip = '',
          [switch]$Candidate, [switch]$BaselineStartup, [int]$Scale = 50, [string]$Backend = 'vulkan')
    $rel = "$Kind/$Id"
    $dir = Join-Path $bundle $rel
    $data = Join-Path $scratch $Id
    $config = Join-Path $data 'local/PerfectWorldViewer'
    foreach ($p in @($dir,$config,(Join-Path $data 'temp'),(Join-Path $data 'roaming'),(Join-Path $data 'program-data'))) {
        [void][IO.Directory]::CreateDirectory($p)
    }
    $cache = if ($BaselineStartup) { Join-Path $scratch 'baseline-startup-cache' } else { Join-Path $data 'cache' }
    $pass = $Provider -ne 'portable'
    $exe = if ($Kind -eq 'smoke') { $smokeExe } else { $viewerExe }
    $envs = @{
        LOCALAPPDATA = (Join-Path $data 'local'); APPDATA = (Join-Path $data 'roaming'); ProgramData = (Join-Path $data 'program-data')
        TEMP = (Join-Path $data 'temp'); TMP = (Join-Path $data 'temp'); PW_CACHE_DIR = $cache
        PW_MODULES_DIR = (Join-Path $scratch 'Modules'); WGPU_BACKEND = $Backend; WGPU_POWER_PREF = 'high'
        WGPU_ADAPTER_NAME = 'NVIDIA GeForce RTX 3080'
        PW_RAY_RECONSTRUCTION = $(if ($pass) { $Provider } else { 'off' })
    }
    # Input files are copied for provenance/read-only use. Missing or unpinned
    # SDKs are explicit failures, not an invitation to load from PATH.
    if ($Provider -eq 'rr' -and $sdkReady.rr) {
        $envs.PW_RAY_RR_BRIDGE = $sdkStage.rr_bridge
        $envs.PW_RAY_RR_SDK_DIR = $sdkStage.rr_directory
    }
    if ($Provider -eq 'fsr' -and $sdkReady.fsr) { $envs.PW_RAY_FSR_VK_DLL = $sdkStage.fsr }
    if ($Skip) { $envs.PW_RAY_RECONSTRUCTION_SKIP = $Skip }
    $run = [ordered]@{
        id = $Id; dir = $rel; kind = $Kind; mode = $Mode; hybrid = $Hybrid; requested_provider = $Provider
        backend = $Backend; policy = $Policy; candidate = [bool]$Candidate; skip = $Skip; smoke_percent = $Scale
        executable_sha256 = (Hash-File $exe); started_utc = $null; ended_utc = $null; first_present_utc = $null
        first_present_ms = $null; exit_code = $null; exit_hex = $null; timeout = $null; last_stage = $null; result = 'failed'
    }
    if ($Kind -eq 'smoke') {
        $argv = @('modules::lighter::reconstruction::tests::lighter_temporal_vendor_dispatch','--exact','--ignored','--nocapture','--test-threads=1')
        $envs.PW_TEST_RAY_RESOLUTION = "$Scale"
        if ($Mode -eq 'hardware') { $envs.PW_TEST_RAY_QUERY = '1' }
    } else {
        $lighter = Make-Settings $Mode $Hybrid
        $lighter | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $dir 'lighter.ini')
        $viewerSettings = @"
resolution_scale=100
fullscreen=false
weather_override=Clear
upscaling=off
frame_generation=off
latency=off
all_to_vram=false
music_enabled=false
ambience_enabled=false
effects_enabled=false
show_zone_entry=false
logging_mask=0
"@
        $viewerSettings | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $dir 'settings.txt')
        ($viewerSettings + "`nlogging_dir=$dir`nupscale_sdk_dir=$data") | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $config 'viewer-settings.txt')
        $envs.PW_PERF_CAPTURE = '1'
        $envs.PWE_WORLD_RELOAD = '1'
        $envs.PW_GPU_RECOVERY_LIGHTER = $lighter
        $envs.PW_GPU_RECOVERY_VIEW = "$map $firstPose $hour"
        $envs.PWE_WORLD_HANDOFF_READY_FILE = Join-Path $dir 'present.ready'
        $envs.PWE_WORLD_HANDOFF_STATUS_FILE = Join-Path $dir 'last-stage.txt'
        $envs.PWE_WORLD_HANDOFF_HWND_FILE = Join-Path $data 'window.hwnd'
        $argv = @($client,'--world',$map)
        if ($Kind -eq 'startup') {
            $envs.PW_PERF_START_ONLY = '1'
            $envs.PW_RAY_PACED_STARTUP = '1'
        } else {
            $envs.PW_LIGHTER_TRAVEL_PLAN = $sessionPlan
            $envs.PW_LIGHTER_TRAVEL_ROUTE = $(if ($Hybrid) { 'hybrid' } else { $Mode })
            $envs.PW_LIGHTER_TRAVEL_CSV = Join-Path $dir $(if ($Kind -eq 'memory') { 'heap.csv' } else { 'frames.csv' })
            $envs.PW_LIGHTER_TRAVEL_SYNC = $(if ($Policy -eq 'synchronized') { '1' } else { '0' })
            if ($Kind -eq 'memory') { $envs.PW_PERF_MEMORY_SECONDS = "$MemorySeconds" }
        }
        if ($Candidate) {
            foreach ($flag in @('PW_RAY_GAME_PROFILE','PW_RAY_PACED_STARTUP','PW_RAY_EXACT_PROXY','PW_RAY_COMPACT_BLAS','PW_RAY_BLUE_NOISE','PW_RAY_BC_TEXTURES')) {
                $envs[$flag] = '1'
            }
            $envs.PW_RAY_EFFECT_REFLECTION = 'near'
            $envs.PW_RAY_EFFECT_SUN = 'near'
            $envs.PW_RAY_EFFECT_BOUNCE = 'near'
        }
    }
    $run.environment = $envs
    $run.argv = $argv
    Write-Host "Running $rel ($Provider, $Backend)"
    if ($pass -and -not $sdkReady[$Provider]) {
        $run.result = 'sdk_unavailable'
        $run.reason = 'Missing or unpinned SDK input. No SDK dispatch attempted.'
        Store-Json (Join-Path $dir 'run.json') $run
        $session.runs.Add($run)
        $summary.Add("${rel}: SDK unavailable")
        return $run
    }
    $proc = $null; $collector = $null; $start = $null; $finish = $null; $ready = $null
    $stages = [Collections.Generic.List[string]]::new()
    $ordinal = 0L; $lastProgress = $null; $stage = ''
    try {
        if ($Kind -eq 'startup') {
            try {
                $collector = Start-Logged $smi @('-i',"$($gpu.index)",'--query-gpu=timestamp,index,uuid,memory.total,memory.used','--format=csv,noheader,nounits','-l','1') $data (Join-Path $dir 'nvidia-memory') $envs
                Start-Sleep -Seconds 3
            } catch { $run.sampler_error = $_.Exception.Message }
        }
        $start = [DateTimeOffset]::UtcNow
        $run.started_utc = Utc $start
        $proc = Start-Logged $exe $argv $data (Join-Path $dir 'process') $envs
        $run.pid = $proc.process.Id
        $clock = [Diagnostics.Stopwatch]::StartNew()
        $eventReady = $envs['PWE_WORLD_HANDOFF_READY_FILE']
        $statusPath = $envs['PWE_WORLD_HANDOFF_STATUS_FILE']
        $run.local_utc_offset_minutes = [TimeZoneInfo]::Local.GetUtcOffset($start).TotalMinutes
        while (-not $proc.process.HasExited) {
            if ($eventReady -and -not $ready -and (Test-Path -LiteralPath $eventReady)) {
                $ready = [DateTimeOffset](Get-Item -LiteralPath $eventReady).LastWriteTimeUtc
                $run.first_present_utc = Utc $ready
                $run.first_present_ms = [Math]::Max(0, ($ready - $start).TotalMilliseconds)
            }
            if ($statusPath -and (Test-Path -LiteralPath $statusPath)) {
                # Writer replaces a short text file; one transient read error
                # may occur while it is being truncated and does not kill the run.
                try { $line = [IO.File]::ReadAllText($statusPath) } catch { $line = '' }
                if ($line -and $line -cne $stage) {
                    $stage = $line
                    $stages.Add("$([Math]::Round($clock.Elapsed.TotalSeconds,3))`t$stage")
                    if ($stage -match '^first-frame:(\d+)\t' -and [long]$Matches[1] -gt $ordinal) {
                        $ordinal = [long]$Matches[1]; $lastProgress = $clock.Elapsed
                    }
                }
            }
            if ($Kind -eq 'smoke') {
                if ($clock.Elapsed.TotalSeconds -ge 600) { $run.timeout = 'sdk_smoke'; break }
            } elseif (-not $ready) {
                if ($clock.Elapsed.TotalSeconds -ge 1200) { $run.timeout = 'world_load'; break }
                if ($null -ne $lastProgress -and ($clock.Elapsed - $lastProgress).TotalSeconds -ge 120) { $run.timeout = 'first_frame_no_progress'; break }
            } elseif ($Kind -eq 'startup') {
                if (([DateTimeOffset]::UtcNow - $ready).TotalSeconds -ge 60) { $run.timeout = 'startup_teardown'; break }
            } elseif (([DateTimeOffset]::UtcNow - $ready).TotalSeconds -ge $TravelTimeoutSeconds) {
                $run.timeout = 'travel'; break
            }
            Start-Sleep -Milliseconds 100
        }
        if ($proc.process.HasExited) { $finish = [DateTimeOffset]$proc.process.ExitTime.ToUniversalTime() }
        try { $run.exit_code = Finish-Logged $proc ($null -ne $run.timeout) } finally { $proc = $null }
        if ($null -eq $finish) { $finish = [DateTimeOffset]::UtcNow }
        $run.ended_utc = Utc $finish
        $run.exit_hex = '0x{0:x8}' -f [int]$run.exit_code
        if ($eventReady -and -not $ready -and (Test-Path -LiteralPath $eventReady)) {
            $ready = [DateTimeOffset](Get-Item -LiteralPath $eventReady).LastWriteTimeUtc
            $run.first_present_utc = Utc $ready
            $run.first_present_ms = [Math]::Max(0, ($ready - $start).TotalMilliseconds)
        }
        if ($statusPath -and (Test-Path -LiteralPath $statusPath)) { $stage = Get-Content -LiteralPath $statusPath -Raw }
        $run.last_stage = $stage
        $stages | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $dir 'stages.txt')
        if ($run.exit_code -eq 0 -and $null -eq $run.timeout) {
            $stderr = Get-Content -LiteralPath (Join-Path $dir 'process.stderr.log') -Raw
            switch ($Kind) {
                'smoke' {
                    $label = if ($Provider -eq 'rr') { 'Vulkan DLSS Ray Reconstruction' } else { 'Vulkan temporal FSR' }
                    if ($stderr -match "(?m)^$label vendor smoke 512²: [0-9]+/[0-9]+ opaque pixels have positive SDK radiance" -and
                        $stderr -match "(?m)^$label vendor smoke 640²: [0-9]+/[0-9]+ opaque pixels have positive SDK radiance") {
                        $run.result = 'completed'
                    }
                }
                'startup' {
                    $want = if ($Skip -or $Provider -eq 'portable') { 'portable' } elseif ($Provider -eq 'rr') { 'Vulkan DLSS Ray Reconstruction' } else { 'Vulkan temporal FSR' }
                    if ($ready -and $stderr -match ('(?m)^Lighter perf first present: 2560x1440 mode=Hardware provider=' + [regex]::Escape($want) + ' adapter=')) { $run.result = 'completed' }
                }
                default {
                    if ($ready -and (Test-Path -LiteralPath $envs.PW_LIGHTER_TRAVEL_CSV -PathType Leaf)) { $run.result = 'completed' }
                }
            }
        }
    } catch {
        $run.reason = $_.Exception.Message
        $run.result = 'runner_error'
    } finally {
        if ($null -ne $proc) {
            try { [void](Finish-Logged $proc $true) } catch { $run.reap_error = $_.Exception.Message }
        }
        if ($null -ne $start -and $Kind -eq 'startup') {
            # Keep post-exit samples and let WER finish writing, but query up to
            # the process exit timestamp. Host events after exit are not blamed.
            Start-Sleep -Seconds 4
            $end = if ($finish) { $finish } else { [DateTimeOffset]::UtcNow }
            $run.events = Save-Events $dir $start $end
        }
        if ($null -ne $collector) {
            $run.sampler_exited_early = $collector.process.HasExited
            try { [void](Finish-Logged $collector $true) } catch { $run.sampler_error = $_.Exception.Message }
            $mem = Join-Path $dir 'nvidia-memory.stdout.log'
            if (Test-Path -LiteralPath $mem) {
                'timestamp_local,gpu_index,gpu_uuid,total_MiB,used_MiB' | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $dir 'nvidia-memory.csv')
                Get-Content -LiteralPath $mem | Add-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $dir 'nvidia-memory.csv')
            }
        }
        Store-Json (Join-Path $dir 'run.json') $run
        $session.runs.Add($run)
        $summary.Add("${rel}: $($run.result); exit=$($run.exit_hex); ready_ms=$($run.first_present_ms); timeout=$($run.timeout)")
    }
    return $run
}

$failure = $null
try {
    $pre = Join-Path $bundle 'prerequisites'
    [void][IO.Directory]::CreateDirectory($pre)
    $build = Get-Content -LiteralPath (Join-Path $kit 'build.json') -Raw | ConvertFrom-Json -AsHashtable
    if ($build.schema -ne 1 -or $build.source_commit -notmatch '^[0-9a-f]{40}$') { throw 'Kit needs a v1 build.json with an exact git revision.' }
    foreach ($fn in @('Perfect-World-Explorer.exe','lighter-sdk-tests.exe','a07-travel.plan')) {
        if ((Hash-File (Join-Path $kit $fn)) -cne $build.files[$fn]) { throw "Kit file hash mismatch: $fn" }
    }
    if ((Hash-File $PSCommandPath) -cne $build.files['run-perf-session.ps1']) { throw 'This script is not from the same kit as the binaries.' }
    Copy-Item -LiteralPath (Join-Path $kit 'build.json') -Destination $pre
    $session.build = $build
    $session.runner = File-Receipt $PSCommandPath
    $session.viewer = File-Receipt (Join-Path $kit 'Perfect-World-Explorer.exe')
    $session.smoke = File-Receipt (Join-Path $kit 'lighter-sdk-tests.exe')
    $runtime = Join-Path $scratch 'viewer'
    [void][IO.Directory]::CreateDirectory($runtime)
    foreach ($fn in @('Perfect-World-Explorer.exe','lighter-sdk-tests.exe','dstorage.dll','dstoragecore.dll')) {
        $source = Join-Path $kit $fn
        if (Test-Path -LiteralPath $source -PathType Leaf) {
            if (-not $build.files.Contains($fn) -or (Hash-File $source) -cne $build.files[$fn]) { throw "Unbound runtime file: $fn" }
            Copy-Item -LiteralPath $source -Destination $runtime
        }
    }
    $viewerExe = Join-Path $runtime 'Perfect-World-Explorer.exe'
    $smokeExe = Join-Path $runtime 'lighter-sdk-tests.exe'
    $module = Join-Path $scratch 'Modules/Lighter'
    [void][IO.Directory]::CreateDirectory($module)
    if ((Hash-File (Join-Path $kit 'lighter-module.ini')) -cne $build.files['lighter-module.ini']) { throw 'Lighter manifest hash mismatch.' }
    Copy-Item -LiteralPath (Join-Path $kit 'lighter-module.ini') -Destination (Join-Path $module 'module.ini')
    $sessionPlan = Join-Path $bundle 'travel.plan'
    Copy-Item -LiteralPath (Join-Path $kit 'a07-travel.plan') -Destination $sessionPlan
    $planLines = @(Get-Content -LiteralPath $sessionPlan | Where-Object { $_ -and $_ -notmatch '^\s*#' })
    $map, $hour, $warmup = $planLines[0] -split '\s+'
    $firstPose = ($planLines[1] -split '\s+',2)[1]
    if ($map -cne 'a07' -or $firstPose -notmatch '^[0-9.eE+ -]+$') { throw 'Kit travel plan is not the a07 recipe.' }
    $session.plan = @{ sha256 = (Hash-File $sessionPlan); map = $map; hour = $hour; warmup = $warmup }
    $element = if (Test-Path -LiteralPath (Join-Path $client 'element/elementclient.exe')) { Join-Path $client 'element' } else { $client }
    $clientFiles = [ordered]@{}
    foreach ($file in @('elementclient.exe','speedtreert.dll',"maps/$map/$map.trn2","maps/$map/$map.ecwld","maps/$map/$map.ecbsd","maps/$map/${map}_1.t2bk")) {
        $rec = File-Receipt (Join-Path $element $file)
        if (-not $rec) { throw "Missing retail file: $file" }
        $clientFiles[$file] = $rec
    }
    foreach ($file in @('building.pck','textures.pck','trees.pck','gfx.pck','configs.pck','surfaces.pck')) {
        $entry = Get-Item -LiteralPath (Join-Path $element $file)
        $clientFiles[$file] = @{ path = $entry.FullName; bytes = $entry.Length; last_write_utc = $entry.LastWriteTimeUtc.ToString('o') }
    }
    $session.client = $clientFiles
    if ($clientFiles['elementclient.exe'].sha256 -cne 'a290998dc13cc5558b54bda3f65d099f95bc0702e54a0613c6d7669f35c6cdb0') {
        throw 'The elementclient.exe SHA-256 does not match the supported PW 1.4.2 client.'
    }
    $smi = (Get-Command nvidia-smi.exe -CommandType Application -ErrorAction Stop | Select-Object -First 1).Source
    $gpuRaw = @(& $smi '--query-gpu=index,name,uuid,pci.bus_id,pci.device_id,driver_version,memory.total' '--format=csv,noheader,nounits')
    if ($LASTEXITCODE -ne 0) { throw 'nvidia-smi cannot query the installed NVIDIA driver.' }
    $gpuRaw | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $pre 'nvidia-gpu.csv')
    $gpus = @($gpuRaw | ConvertFrom-Csv -Header @('index','name','uuid','pci_bus','device_id','driver','memory_mib') |
        Where-Object { $_.name.Trim() -ceq 'NVIDIA GeForce RTX 3080' -and [int]$_.memory_mib -ge 9728 -and [int]$_.memory_mib -le 10240 })
    if ($gpus.Count -ne 1) { throw 'Expected exactly one NVIDIA GeForce RTX 3080 with 10 GiB dedicated memory.' }
    $gpu = $gpus[0]
    $session.gpu = @{}
    foreach ($field in $gpu.PSObject.Properties) { $session.gpu[$field.Name] = $field.Value.Trim() }
    $session.nvidia_smi = File-Receipt $smi
    & $smi '-q' | Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $pre 'nvidia-driver.txt')
    $session.timezone = [TimeZoneInfo]::Local.Id
    $session.sdk = @{
        fsr = (File-Receipt $FsrDll); rr_bridge = (File-Receipt $RrBridge); rr_loader_lib = (File-Receipt $RrLoaderLib)
        rr_feature = $(if ($RrSdkDirectory) { File-Receipt (Join-Path $RrSdkDirectory 'nvngx_dlssd.dll') } else { $null })
    }
    $sdkReady = @{
        fsr = ($session.sdk.fsr -and $session.sdk.fsr.bytes -eq 36135696 -and $session.sdk.fsr.sha256 -ceq '4a7e895af6e0b615d40a84d59eb8c2b1054ad91cdd0c739567940270d51bdab7')
        rr = ($session.sdk.rr_bridge -and $session.sdk.rr_feature -and $session.sdk.rr_feature.bytes -eq 48343664 -and $session.sdk.rr_feature.sha256 -ceq 'b49435ae346f227feafe7bbe937d88277ea0ee7323975ec06488f377fdb46cee' -and
            (-not $RrLoaderLib -or ($session.sdk.rr_loader_lib -and $session.sdk.rr_loader_lib.sha256 -ceq '7b9a5239c1145af5f645ab472aa066f9ea79db36ac819562312aae69d65f3765')))
    }
    $session.sdk_ready = $sdkReady
    $sdkStage = @{}
    if ($sdkReady.fsr) {
        $sdkStage.fsr = Join-Path $scratch 'amd_fidelityfx_upscaler_vk.dll'
        Copy-Item -LiteralPath $session.sdk.fsr.path -Destination $sdkStage.fsr
        if ((Hash-File $sdkStage.fsr) -cne $session.sdk.fsr.sha256) { throw 'FSR SDK changed during staging.' }
    }
    if ($sdkReady.rr) {
        $sdkStage.rr_directory = Join-Path $scratch 'rr-sdk'
        [void][IO.Directory]::CreateDirectory($sdkStage.rr_directory)
        $sdkStage.rr_bridge = Join-Path $sdkStage.rr_directory 'pwe_ray_rr.dll'
        $feature = Join-Path $sdkStage.rr_directory 'nvngx_dlssd.dll'
        Copy-Item -LiteralPath $session.sdk.rr_bridge.path -Destination $sdkStage.rr_bridge
        Copy-Item -LiteralPath $session.sdk.rr_feature.path -Destination $feature
        if ((Hash-File $sdkStage.rr_bridge) -cne $session.sdk.rr_bridge.sha256 -or (Hash-File $feature) -cne $session.sdk.rr_feature.sha256) { throw 'RR SDK changed during staging.' }
    }
    $session.prerequisites_ok = $true
    $first = Invoke-Run -Id hardware-1 -Kind startup -Provider $StartupProvider -BaselineStartup
    # A full minute after completion, not just after the first process started.
    Write-Host 'Waiting 60 seconds before the second Hardware start…'
    Start-Sleep -Seconds 60
    $second = Invoke-Run -Id hardware-2 -Kind startup -Provider $StartupProvider -BaselineStartup
    if ($StartupProvider -eq 'portable' -and ($first.result -ne 'completed' -or $second.result -ne 'completed')) {
        $summary.Add('Skipped SDK bisection: neither baseline launch requested an SDK. Re-run on the originally failing SDK route if applicable.')
    }
    if ($StartupProvider -ne 'portable' -and $sdkReady[$StartupProvider] -and
        ($first.result -ne 'completed' -or $second.result -ne 'completed')) {
        $loader = if ($StartupProvider -eq 'rr') { 'ngx-loader' } else { 'ffx-loader' }
        foreach ($skip in @('bootstrap','raw-device',$loader,'interop')) {
            [void](Invoke-Run -Id "bisect-$skip" -Kind startup -Provider $StartupProvider -Skip $skip)
        }
    }
    if (-not $StartupOnly) {
        $routes = @(
            @{ Id='native'; Mode='off' }, @{ Id='software'; Mode='software' }, @{ Id='hardware'; Mode='hardware' },
            @{ Id='hybrid'; Mode='hardware'; Hybrid=$true }, @{ Id='hardware-rr'; Mode='hardware'; Provider='rr' },
            @{ Id='software-fsr'; Mode='software'; Provider='fsr' }, @{ Id='candidate'; Mode='hardware'; Hybrid=$true; Candidate=$true },
            @{ Id='native-dx12'; Mode='off'; Backend='dx12' }
        )
        foreach ($route in $routes) {
            foreach ($policy in @('normal_present','synchronized')) {
                $opts = $route.Clone(); $opts.Id += '-' + $policy
                [void](Invoke-Run @opts -Kind travel -Policy $policy)
            }
        }
        foreach ($provider in @('rr','fsr')) {
            foreach ($mode in @('software','hardware')) {
                foreach ($scale in @(50,100)) {
                    [void](Invoke-Run -Id "$provider-$mode-$scale" -Kind smoke -Mode $mode -Provider $provider -Scale $scale)
                }
            }
        }
        [void](Invoke-Run -Id candidate-memory -Kind memory -Hybrid $true -Candidate -Policy 'memory_travel')
    }
} catch {
    $failure = $_.Exception.Message
    $session.runner_error = $failure
    $summary.Add("Runner stopped: $failure")
} finally {
    $session.finished_utc = Utc ([DateTimeOffset]::UtcNow)
    Store-Json (Join-Path $bundle 'session.json') $session
    @('Lighter performance collection (raw; not P/V sign-off)', "Session: $stamp", "Host: $($session.machine)",
        'Timing policies are separate. A requested SDK must actually dispatch. Upload the ZIP, not scratch.', '') + $summary |
        Set-Content -Encoding utf8NoBOM -LiteralPath (Join-Path $bundle 'SUMMARY.txt')
    $inventory = [ordered]@{}
    Get-ChildItem -LiteralPath $bundle -Recurse -File | Sort-Object FullName | ForEach-Object {
        $inventory[[IO.Path]::GetRelativePath($bundle,$_.FullName).Replace('\','/')] = Hash-File $_.FullName
    }
    Store-Json (Join-Path $bundle 'SHA256SUMS.json') $inventory
    $zip = Join-Path $root "Lighter-Perf-$stamp.zip"
    [IO.Compression.ZipFile]::CreateFromDirectory($bundle,$zip,[IO.Compression.CompressionLevel]::Optimal,$false)
    "$((Hash-File $zip))  $([IO.Path]::GetFileName($zip))" | Set-Content -Encoding utf8NoBOM -LiteralPath "$zip.sha256"
    Write-Host "Upload: $zip"
    Write-Host "SHA-256: $zip.sha256"
}
if ($failure) { throw $failure }
