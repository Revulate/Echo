<#
.SYNOPSIS
Windows release perf rig: launches one Echo binary against a local chat replay
in an isolated profile and writes a per-split CPU, frame, GPU and memory summary.

.DESCRIPTION
  pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe <echo.exe> -OutDir <dir>

Without -Exe it builds `cargo build --profile dist --locked --bin echo` (unless
-SkipBuild) and copies target\dist\echo.exe into -WorkDir. For an A/B, run it
once per binary in ABBA order with the same settings and compare the
summary.json files.

The layout has three splits: split A shows fast animations (20-40 ms frames),
splits B and C show only slow ones (TeaTime ~120 ms, peepoPls 100-200 ms).
Records per-split `animation_waker_checks` when the binary logs it.
Fixtures live in benchmarks/fixtures/perf-rig/.
#>
param(
    [string] $OutDir = '',
    [string] $WorkDir = (Join-Path $env:TEMP 'echo-perf-rig'),
    [string] $Exe = '',
    [switch] $SkipBuild,
    [int] $WarmupRunSeconds = 45,
    [int] $SettleSeconds = 25,
    [int] $WindowSeconds = 75,
    [int] $SampleSeconds = 5,
    [int] $WindowWidth = 1200,
    [int] $WindowHeight = 960
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$rig = Join-Path $repo 'benchmarks\fixtures\perf-rig'
$scenarioName = '3-split-mixed-rate'
$tabsFixture = Join-Path $rig 'tabs-3split.json'
$replayFixture = Join-Path $rig 'replay-mixed.jsonl'
if (-not $OutDir) { $OutDir = Join-Path $repo ("benchmarks\results\{0}-perf-rig" -f (Get-Date -Format 'yyyyMMdd-HHmmss')) }
if (-not [IO.Path]::IsPathRooted($OutDir)) { $OutDir = Join-Path $repo $OutDir }
New-Item -ItemType Directory -Force $OutDir | Out-Null
New-Item -ItemType Directory -Force $WorkDir | Out-Null

Add-Type @"
using System;
using System.Runtime.InteropServices;
public static class EchoBenchWin {
  [DllImport("kernel32.dll", SetLastError=true)] public static extern IntPtr OpenThread(int a, bool i, int id);
  [DllImport("kernel32.dll")] public static extern bool CloseHandle(IntPtr h);
  [DllImport("kernel32.dll")] public static extern int GetThreadDescription(IntPtr h, out IntPtr d);
  [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr p);
  [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
  [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
  [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h, int cmd);
  [DllImport("user32.dll")] public static extern void keybd_event(byte vk, byte scan, uint flags, UIntPtr extra);
  public static bool Focus(IntPtr h) {
    ShowWindow(h, 9);
    keybd_event(0x12, 0, 0, UIntPtr.Zero); keybd_event(0x12, 0, 2, UIntPtr.Zero);
    return SetForegroundWindow(h);
  }
  [DllImport("user32.dll")] public static extern bool SetWindowPos(IntPtr h, IntPtr after, int x, int y, int cx, int cy, uint flags);
  public static string ThreadName(int id) {
    IntPtr h = OpenThread(0x0800, false, id); if (h == IntPtr.Zero) h = OpenThread(0x1000, false, id); if (h == IntPtr.Zero) return "";
    IntPtr d; string s = "";
    if (GetThreadDescription(h, out d) >= 0 && d != IntPtr.Zero) { s = Marshal.PtrToStringUni(d); LocalFree(d); }
    CloseHandle(h); return s;
  }
}
"@

$commit = (git -C $repo rev-parse HEAD).Trim()
$dirty = [bool](git -C $repo status --porcelain --untracked-files=no -- src crates Cargo.toml Cargo.lock)
if (-not $Exe) {
    if (-not $SkipBuild) {
        Push-Location $repo
        try {
            cargo build --profile dist --locked --bin echo 2>&1 | Tee-Object (Join-Path $OutDir 'build.log') | Select-Object -Last 3
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
        } finally { Pop-Location }
    }
    $built = Join-Path $repo 'target\dist\echo.exe'
    $binDir = Join-Path $WorkDir "bin-$($commit.Substring(0,8))"
    New-Item -ItemType Directory -Force $binDir | Out-Null
    $Exe = Join-Path $binDir 'echo.exe'
    Copy-Item -Force $built $Exe
}
$exeHash = (Get-FileHash $Exe -Algorithm SHA256).Hash

function Set-BenchEnv([string] $ProfileRoot, [string] $Replay, [string] $RunOut, [int] $QuitAfterMs) {
    Get-ChildItem Env: | Where-Object { $_.Name -like 'ECHO_*' -or $_.Name -eq 'RUST_LOG' } |
        ForEach-Object { Remove-Item "Env:$($_.Name)" }
    $env:ECHO_BENCH_PROFILE_ROOT = $ProfileRoot
    $env:ECHO_BENCH_ALLOW_MULTI_INSTANCE = '1'
    $env:ECHO_CHAT_REPLAY = $Replay
    $env:ECHO_CHAT_REPLAY_REPORT = Join-Path $RunOut 'replay-report.json'
    $env:ECHO_CHAT_REPLAY_DELAY_MS = '3000'
    $env:ECHO_FPS_PROFILER = '1'
    $env:ECHO_MEM_REPORT_SECS = "$SampleSeconds"
    $env:ECHO_LOG = 'perf,irc'
    $env:ECHO_LOG_FILE = '1'
    $env:ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS = "$QuitAfterMs"
}

function Write-Tabs([string] $ProfileRoot, [string] $TabsFile) {
    $layouts = Join-Path $ProfileRoot 'config\layouts'
    New-Item -ItemType Directory -Force $layouts | Out-Null
    Get-ChildItem $layouts -Filter 'tabs*' -ErrorAction SilentlyContinue | Remove-Item -Force
    Copy-Item -Force $TabsFile (Join-Path $layouts 'tabs.json')
}

function Wait-MainWindow($process) {
    for ($attempt = 0; $attempt -lt 100; $attempt++) {
        $process.Refresh()
        if ($process.MainWindowHandle -ne [IntPtr]::Zero) { return $process.MainWindowHandle }
        Start-Sleep -Milliseconds 200
    }
    return [IntPtr]::Zero
}

function Get-ThreadCpu($process) {
    $process.Refresh()
    $cpuByThread = @{}
    foreach ($thread in $process.Threads) {
        try { $cpuByThread[$thread.Id] = [pscustomobject]@{ ms = $thread.TotalProcessorTime.TotalMilliseconds; start = $thread.StartTime } } catch {}
    }
    $cpuByThread
}

function Get-GpuRunTime([int] $ProcessId) {
    $runTimeByEngine = @{}
    try {
        $counters = Get-Counter "\GPU Engine(pid_${ProcessId}_*)\Running Time" -ErrorAction Stop
        foreach ($sample in $counters.CounterSamples) {
            $engine = ($sample.InstanceName -split 'engtype_')[-1]
            $runTimeByEngine[$engine] = [double]$runTimeByEngine[$engine] + $sample.CookedValue
        }
    } catch {}
    $runTimeByEngine
}

function Stop-Run($process, [int] $TimeoutMs) {
    if ($process.WaitForExit($TimeoutMs)) { return $false }
    $null = $process.CloseMainWindow()
    if ($process.WaitForExit(10000)) { return $false }
    Stop-Process -Id $process.Id -Force
    return $true
}

function Get-LogKeyValues([string] $line) {
    $values = @{}
    foreach ($match in [regex]::Matches($line, '\s(\w+)=("([^"]*)"|[^\s"]+)')) {
        $value = if ($match.Groups[3].Success) { $match.Groups[3].Value } else { $match.Groups[2].Value }
        $values[$match.Groups[1].Value] = $value
    }
    $values
}

function Get-LogTimestamp([string] $line) {
    $null = $line -match '^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?)Z'
    [datetime]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal)
}

$template = Join-Path $WorkDir "profile-warm-$($commit.Substring(0,8))"
if (Test-Path $template) { Remove-Item -Recurse -Force $template }
New-Item -ItemType Directory -Force $template | Out-Null
Write-Tabs $template $tabsFixture
$warmOut = Join-Path $WorkDir 'warmup-run'
if (Test-Path $warmOut) { Remove-Item -Recurse -Force $warmOut }
New-Item -ItemType Directory -Force $warmOut | Out-Null
Set-BenchEnv $template $replayFixture $warmOut ($WarmupRunSeconds * 1000)
Write-Host "warm-up run ($WarmupRunSeconds s) -> $template"
$warmupProcess = Start-Process -FilePath $Exe -PassThru -RedirectStandardOutput (Join-Path $warmOut 'stdout.txt') -RedirectStandardError (Join-Path $warmOut 'stderr.txt')
$warmupWindow = Wait-MainWindow $warmupProcess
if ($warmupWindow -ne [IntPtr]::Zero) { $null = [EchoBenchWin]::SetWindowPos($warmupWindow, [IntPtr]::Zero, 80, 40, $WindowWidth, $WindowHeight, 0x0004) }
$null = Stop-Run $warmupProcess (($WarmupRunSeconds + 20) * 1000)

$profileRoot = Join-Path $WorkDir 'profile-run'
if (Test-Path $profileRoot) { Remove-Item -Recurse -Force $profileRoot }
Copy-Item -Recurse $template $profileRoot
Get-ChildItem $profileRoot -Recurse -Filter '*.log*' -ErrorAction SilentlyContinue | Remove-Item -Force
Write-Tabs $profileRoot $tabsFixture
$quitMs = ($SettleSeconds + $WindowSeconds + 5) * 1000
Set-BenchEnv $profileRoot $replayFixture $OutDir $quitMs

Write-Host "$scenarioName run: settle $SettleSeconds s, window $WindowSeconds s"
$launchTime = Get-Date
$stdio = Join-Path $WorkDir 'stdio-run'
New-Item -ItemType Directory -Force $stdio | Out-Null
$echoProcess = Start-Process -FilePath $Exe -PassThru -RedirectStandardOutput (Join-Path $stdio 'stdout.txt') -RedirectStandardError (Join-Path $stdio 'stderr.txt')
$mainWindow = Wait-MainWindow $echoProcess
if ($mainWindow -ne [IntPtr]::Zero) {
    $null = [EchoBenchWin]::SetWindowPos($mainWindow, [IntPtr]::Zero, 80, 40, $WindowWidth, $WindowHeight, 0x0004)
    $null = [EchoBenchWin]::Focus($mainWindow)
}
$remainingSettleSeconds = $SettleSeconds - ((Get-Date) - $launchTime).TotalSeconds
if ($remainingSettleSeconds -gt 0) { Start-Sleep -Milliseconds ([int]($remainingSettleSeconds * 1000)) }

$gpuRunTimeStart = Get-GpuRunTime $echoProcess.Id; $gpuWindowStart = Get-Date
$threadCpuStart = Get-ThreadCpu $echoProcess
$echoProcess.Refresh(); $cpuStartMs = $echoProcess.TotalProcessorTime.TotalMilliseconds; $windowStart = Get-Date
$samples = @()
$deadline = $windowStart.AddSeconds($WindowSeconds)
while ((Get-Date) -lt $deadline) {
    Start-Sleep -Seconds ([math]::Min($SampleSeconds, [math]::Max(1, ($deadline - (Get-Date)).TotalSeconds)))
    $echoProcess.Refresh()
    $samples += [pscustomobject]@{
        t_s = [math]::Round(((Get-Date) - $windowStart).TotalSeconds, 1)
        cpu_ms = [math]::Round($echoProcess.TotalProcessorTime.TotalMilliseconds - $cpuStartMs)
        ws_mb = [math]::Round($echoProcess.WorkingSet64 / 1MB, 1)
        private_mb = [math]::Round($echoProcess.PrivateMemorySize64 / 1MB, 1)
        foreground = ([EchoBenchWin]::GetForegroundWindow() -eq $mainWindow)
    }
}
$threadCpuEnd = Get-ThreadCpu $echoProcess
$gpuRunTimeEnd = Get-GpuRunTime $echoProcess.Id; $gpuWallMs = ((Get-Date) - $gpuWindowStart).TotalMilliseconds
$echoProcess.Refresh(); $cpuEndMs = $echoProcess.TotalProcessorTime.TotalMilliseconds; $windowEnd = Get-Date
$workingSetEnd = $echoProcess.WorkingSet64; $privateBytesEnd = $echoProcess.PrivateMemorySize64
$forced = Stop-Run $echoProcess 30000
$wallMs = ($windowEnd - $windowStart).TotalMilliseconds

$mainThreadId = ($threadCpuEnd.GetEnumerator() | Sort-Object { $_.Value.start } | Select-Object -First 1).Key
$threadRows = foreach ($threadId in $threadCpuEnd.Keys) {
    $cpuBeforeMs = if ($threadCpuStart.ContainsKey($threadId)) { $threadCpuStart[$threadId].ms } else { 0 }
    $cpuDeltaMs = $threadCpuEnd[$threadId].ms - $cpuBeforeMs
    if ($cpuDeltaMs -gt 0) {
        $threadName = [EchoBenchWin]::ThreadName($threadId)
        if (-not $threadName) { $threadName = if ($threadId -eq $mainThreadId) { 'main' } else { '(unnamed)' } }
        [pscustomobject]@{ name = $threadName; cpu_ms = $cpuDeltaMs }
    }
}
$byName = $threadRows | Group-Object name | ForEach-Object {
    [pscustomobject]@{ name = $_.Name; threads = $_.Count; cpu_ms = [math]::Round(($_.Group | Measure-Object cpu_ms -Sum).Sum) }
} | Sort-Object cpu_ms -Descending
$mainMs = ($byName | Where-Object name -eq 'main' | Measure-Object cpu_ms -Sum).Sum

$logPath = Join-Path $profileRoot 'logs\echo.log'
$windowStartUtc = $windowStart.ToUniversalTime(); $windowEndUtc = $windowEnd.ToUniversalTime()
$inWindow = New-Object System.Collections.Generic.List[string]
$perfLines = New-Object System.Collections.Generic.List[string]
$warnCount = 0; $errorCount = 0; $panic = $false; $ircJoins = 0
if (Test-Path $logPath) {
    foreach ($line in [IO.File]::ReadLines($logPath)) {
        if ($line -cmatch 'Z  WARN ') { $warnCount++ }
        if ($line -cmatch 'Z ERROR ') { $errorCount++ }
        if ($line -match 'panicked') { $panic = $true }
        if ($line -match 'Joined channel: ') { $ircJoins++ }
        if ($line -notmatch 'cadence_fps=|Canvas render/layout aggregate|memory breakdown') { continue }
        $perfLines.Add($line)
        if ($line -match '^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?)Z') {
            $timestamp = [datetime]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal)
            if ($timestamp -ge $windowStartUtc -and $timestamp -le $windowEndUtc) { $inWindow.Add($line) }
        }
    }
}
$perfLines | Set-Content (Join-Path $OutDir 'perf-lines.txt')

$cadenceLines = @($inWindow | Where-Object { $_ -match 'cadence_fps=' })
$profiler = [ordered]@{ lines = $cadenceLines.Count }
if ($cadenceLines.Count -ge 2) {
    $firstCadence = Get-LogKeyValues $cadenceLines[0]; $lastCadence = Get-LogKeyValues $cadenceLines[-1]
    $span = ((Get-LogTimestamp $cadenceLines[-1]) - (Get-LogTimestamp $cadenceLines[0])).TotalSeconds
    $cadenceValues = $cadenceLines | ForEach-Object { Get-LogKeyValues $_ }
    foreach ($key in 'cadence_fps', 'work_ms', 'canvas_cpu_ms', 'canvas_paint_ms', 'gpu_ms', 'present_latency_ms', 'p95_ms') {
        $values = @($cadenceValues | ForEach-Object { $_[$key] } | Where-Object { $_ -and $_ -ne 'NaN' } | ForEach-Object { [double]$_ })
        if ($values.Count) { $profiler["mean_$key"] = [math]::Round(($values | Measure-Object -Average).Average, 3) }
    }
    $profiler.span_s = [math]::Round($span, 2)
    if ($firstCadence.present_count -and $lastCadence.present_count -and $span -gt 0) {
        $profiler.presents_per_s = [math]::Round(([double]$lastCadence.present_count - [double]$firstCadence.present_count) / $span, 2)
    }
    if ($firstCadence.gpu_samples -and $lastCadence.gpu_samples -and $span -gt 0) {
        $profiler.gpu_samples_per_s = [math]::Round(([double]$lastCadence.gpu_samples - [double]$firstCadence.gpu_samples) / $span, 2)
    }
    $frameClassStart = $null; $frameClassEnd = $null
    $frameClassPattern = 'frames\[stable:(\d+) scroll:(\d+) rebuild:(\d+) anim:(\d+)\]'
    if ($cadenceLines[0] -match $frameClassPattern) { $frameClassStart = $Matches[1..4] | ForEach-Object { [double]$_ } }
    if ($cadenceLines[-1] -match $frameClassPattern) { $frameClassEnd = $Matches[1..4] | ForEach-Object { [double]$_ } }
    if ($frameClassStart -and $frameClassEnd -and $span -gt 0) {
        $profiler.profiler_frames_per_s = [math]::Round((($frameClassEnd | Measure-Object -Sum).Sum - ($frameClassStart | Measure-Object -Sum).Sum) / $span, 2)
        $profiler.frame_class_per_s = [ordered]@{
            stable = [math]::Round(($frameClassEnd[0] - $frameClassStart[0]) / $span, 2); scroll = [math]::Round(($frameClassEnd[1] - $frameClassStart[1]) / $span, 2)
            rebuild = [math]::Round(($frameClassEnd[2] - $frameClassStart[2]) / $span, 2); anim = [math]::Round(($frameClassEnd[3] - $frameClassStart[3]) / $span, 2)
        }
    }
    if ($cadenceLines[-1] -match 'gpui_draw=p50:([\d.]+) p95:([\d.]+) n:(\d+)') {
        $profiler.gpui_draw_p50_ms = [double]$Matches[1]; $profiler.gpui_draw_p95_ms = [double]$Matches[2]
    }
}

$aggregateLines = @($inWindow | Where-Object { $_ -match 'Canvas render/layout aggregate' })
$canvas = [ordered]@{}
foreach ($split in ($aggregateLines | ForEach-Object { Get-LogKeyValues $_ } | Group-Object { $_.channel_id })) {
    $frames = ($split.Group | ForEach-Object { [double]$_.frames } | Measure-Object -Sum).Sum
    $canvas[$split.Name] = [ordered]@{
        aggregate_lines = $split.Count
        frames = $frames
        frames_per_s = [math]::Round($frames / ($wallMs / 1000), 2)
        animation_frame_unchanged_skips = ($split.Group | ForEach-Object { [double]$_.animation_frame_unchanged_skips } | Measure-Object -Sum).Sum
        animation_waker_checks = ($split.Group | ForEach-Object { if ($_.animation_waker_checks) { [double]$_.animation_waker_checks } else { 0 } } | Measure-Object -Sum).Sum
        animation_visible_row_reuse = ($split.Group | ForEach-Object { [double]$_.animation_visible_row_reuse } | Measure-Object -Sum).Sum
        full_rebuilds = ($split.Group | ForEach-Object { [double]$_.full_rebuilds } | Measure-Object -Sum).Sum
    }
}

$memoryLines = @($inWindow | Where-Object { $_ -match 'memory breakdown' } | ForEach-Object { Get-LogKeyValues $_ })
$memory = [ordered]@{ samples = $memoryLines.Count }
foreach ($key in 'hot_frames', 'hot_frame_urls', 'hot_frames_max_per_url', 'hot_render_images', 'pending_render_image_drops', 'decoded_frames_mb', 'anim_sources_mb', 'decoded_total_mb', 'name_paint_composites', 'name_paint_composite_retirements', 'gpui_uri_entries', 'gpui_uri_retirements', 'private_mb', 'ws_mb') {
    $values = @($memoryLines | ForEach-Object { $_[$key] } | Where-Object { $_ -ne $null } | ForEach-Object { [double]$_ })
    if ($values.Count) {
        $stats = $values | Measure-Object -Average -Maximum
        $memory[$key] = [ordered]@{ mean = [math]::Round($stats.Average, 1); max = $stats.Maximum; last = $values[-1] }
    }
}

$memoryRawLines = @($inWindow | Where-Object { $_ -match 'memory breakdown' })
$counterRates = [ordered]@{}
if ($memoryRawLines.Count -ge 2) {
    $firstMemory = Get-LogKeyValues $memoryRawLines[0]; $lastMemory = Get-LogKeyValues $memoryRawLines[-1]
    $memorySpanSeconds = ((Get-LogTimestamp $memoryRawLines[-1]) - (Get-LogTimestamp $memoryRawLines[0])).TotalSeconds
    foreach ($key in 'animation_frame_decompressions', 'animation_refill_windows', 'animation_refill_frames', 'hot_frame_render_images_created', 'render_images_drained', 'render_image_drain_batches') {
        if ($null -ne $firstMemory[$key] -and $null -ne $lastMemory[$key] -and $memorySpanSeconds -gt 0) {
            $counterRates[$key] = [math]::Round(([double]$lastMemory[$key] - [double]$firstMemory[$key]) / $memorySpanSeconds, 2)
        }
    }
    if ($counterRates.Count) { $counterRates.span_s = [math]::Round($memorySpanSeconds, 2) }
}

# GPU Engine "Running Time" counters are in 100 ns units.
$gpuEngines = [ordered]@{}
foreach ($engine in $gpuRunTimeEnd.Keys) {
    $busyMs = ($gpuRunTimeEnd[$engine] - [double]$gpuRunTimeStart[$engine]) / 10000
    if ($busyMs -gt 0) { $gpuEngines[$engine] = [ordered]@{ busy_ms = [math]::Round($busyMs, 1); busy_pct = [math]::Round(100 * $busyMs / $gpuWallMs, 3) } }
}
$foregroundSamples = @($samples | Where-Object foreground).Count
$run = [ordered]@{
    scenario = $scenarioName
    window_s = [math]::Round($wallMs / 1000, 2)
    cpu_ms = [math]::Round($cpuEndMs - $cpuStartMs, 1)
    cpu_pct_one_core = [math]::Round(100 * ($cpuEndMs - $cpuStartMs) / $wallMs, 2)
    main_thread_cpu_ms = $mainMs
    main_thread_pct_one_core = [math]::Round(100 * $mainMs / $wallMs, 2)
    working_set_mb_end = [math]::Round($workingSetEnd / 1MB, 1)
    private_mb_end = [math]::Round($privateBytesEnd / 1MB, 1)
    working_set_mb_mean = [math]::Round(($samples | Measure-Object ws_mb -Average).Average, 1)
    private_mb_mean = [math]::Round(($samples | Measure-Object private_mb -Average).Average, 1)
    foreground_samples = "$foregroundSamples/$($samples.Count)"
    gpu_engine_running_time = $gpuEngines
    profiler = $profiler
    canvas_per_split = $canvas
    memory_breakdown = $memory
    counter_rates_per_s = $counterRates
    threads = @($byName)
    cpu_samples = @($samples)
    irc_channel_joins = $ircJoins
    log_warn_lines = $warnCount
    log_error_lines = $errorCount
    panic = $panic
    exit_code = $echoProcess.ExitCode
    forced_kill = $forced
}

$gpus = Get-CimInstance Win32_VideoController | ForEach-Object {
    [ordered]@{ name = $_.Name; driver = $_.DriverVersion; refresh_hz = $_.CurrentRefreshRate; resolution = "$($_.CurrentHorizontalResolution)x$($_.CurrentVerticalResolution)" }
}
$summary = [ordered]@{
    captured_at = (Get-Date).ToUniversalTime().ToString('o')
    commit = $commit
    tree_dirty_src = $dirty
    exe = $Exe
    exe_sha256 = $exeHash
    settle_s = $SettleSeconds
    window_s = $WindowSeconds
    warmup_run_s = $WarmupRunSeconds
    window_px = "${WindowWidth}x${WindowHeight}"
    machine = [ordered]@{
        cpu = (Get-CimInstance Win32_Processor | Select-Object -First 1).Name.Trim()
        ram_gb = [math]::Round((Get-CimInstance Win32_ComputerSystem).TotalPhysicalMemory / 1GB, 1)
        os = (Get-CimInstance Win32_OperatingSystem).Caption + ' ' + (Get-CimInstance Win32_OperatingSystem).BuildNumber
        gpus = @($gpus)
    }
    other_echo_processes = @(Get-Process echo -ErrorAction SilentlyContinue | Where-Object { $_.Path -ne $Exe } | ForEach-Object { $_.Path }).Count
    run = $run
}
$summary | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutDir 'summary.json')
'{0}: cpu {1}% main {2}% presents/s {3} profiler frames/s {4} private {5} MB ws {6} MB fg {7}' -f $run.scenario, $run.cpu_pct_one_core, $run.main_thread_pct_one_core, $run.profiler.presents_per_s, $run.profiler.profiler_frames_per_s, $run.private_mb_end, $run.working_set_mb_end, $run.foreground_samples
if ($run.counter_rates_per_s.Count) {
    'counters/s: {0}' -f (($run.counter_rates_per_s.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ')
}
