<#
Windows release perf rig: launches one Echo binary against a local chat replay
in an isolated profile and writes a per-split CPU, frame, GPU and memory summary.

  pwsh -NoProfile -File scripts\bench\run-perf-rig.ps1 -Exe <echo.exe> -OutDir <dir>

Without -Exe it builds `cargo build --release --bin echo` (unless -SkipBuild) and
copies the binary into -WorkDir. For an A/B, run it once per binary in ABBA order
with the same settings and compare the summary.json files.

Scenario M: three splits; split A shows fast animations (20-40 ms frames),
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
    [int] $WindowHeight = 960,
    [string[]] $Scenarios = @('M')
)
$ErrorActionPreference = 'Stop'
$repo = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$rig = Join-Path $repo 'benchmarks\fixtures\perf-rig'
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
            cargo build --release --bin echo 2>&1 | Tee-Object (Join-Path $OutDir 'build.log') | Select-Object -Last 3
            if ($LASTEXITCODE -ne 0) { throw "cargo build failed ($LASTEXITCODE)" }
        } finally { Pop-Location }
    }
    $built = Join-Path $repo 'target\release\echo.exe'
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

function Wait-MainWindow($p) {
    for ($i = 0; $i -lt 100; $i++) {
        $p.Refresh()
        if ($p.MainWindowHandle -ne [IntPtr]::Zero) { return $p.MainWindowHandle }
        Start-Sleep -Milliseconds 200
    }
    return [IntPtr]::Zero
}

function Get-ThreadCpu($p) {
    $p.Refresh()
    $m = @{}
    foreach ($t in $p.Threads) {
        try { $m[$t.Id] = [pscustomobject]@{ ms = $t.TotalProcessorTime.TotalMilliseconds; start = $t.StartTime } } catch {}
    }
    $m
}

function Get-GpuRunTime([int] $ProcessId) {
    $h = @{}
    try {
        $c = Get-Counter "\GPU Engine(pid_${ProcessId}_*)\Running Time" -ErrorAction Stop
        foreach ($x in $c.CounterSamples) {
            $eng = ($x.InstanceName -split 'engtype_')[-1]
            $h[$eng] = [double]$h[$eng] + $x.CookedValue
        }
    } catch {}
    $h
}

function Stop-Run($p, [int] $TimeoutMs) {
    if ($p.WaitForExit($TimeoutMs)) { return $false }
    $null = $p.CloseMainWindow()
    if ($p.WaitForExit(10000)) { return $false }
    Stop-Process -Id $p.Id -Force
    return $true
}

$template = Join-Path $WorkDir "profile-warm-$($commit.Substring(0,8))"
if (Test-Path $template) { Remove-Item -Recurse -Force $template }
New-Item -ItemType Directory -Force $template | Out-Null
Write-Tabs $template (Join-Path $rig 'tabs-3split.json')
$warmOut = Join-Path $WorkDir 'warmup-run'
if (Test-Path $warmOut) { Remove-Item -Recurse -Force $warmOut }
New-Item -ItemType Directory -Force $warmOut | Out-Null
Set-BenchEnv $template (Join-Path $rig 'replay-mixed.jsonl') $warmOut ($WarmupRunSeconds * 1000)
Write-Host "warm-up run ($WarmupRunSeconds s) -> $template"
$wp = Start-Process -FilePath $Exe -PassThru -RedirectStandardOutput (Join-Path $warmOut 'stdout.txt') -RedirectStandardError (Join-Path $warmOut 'stderr.txt')
$wh = Wait-MainWindow $wp
if ($wh -ne [IntPtr]::Zero) { $null = [EchoBenchWin]::SetWindowPos($wh, [IntPtr]::Zero, 80, 40, $WindowWidth, $WindowHeight, 0x0004) }
$null = Stop-Run $wp (($WarmupRunSeconds + 20) * 1000)

$scenarioDefs = @{
    M = @{ name = '3-split-mixed-rate'; tabs = 'tabs-3split.json'; replay = 'replay-mixed.jsonl' }
}

$results = [ordered]@{}
foreach ($s in $Scenarios) {
    $def = $scenarioDefs[$s]
    $runOut = Join-Path $OutDir "scenario-$s"
    if (Test-Path $runOut) { Remove-Item -Recurse -Force $runOut }
    New-Item -ItemType Directory -Force $runOut | Out-Null
    $profileRoot = Join-Path $WorkDir "profile-$s"
    if (Test-Path $profileRoot) { Remove-Item -Recurse -Force $profileRoot }
    Copy-Item -Recurse $template $profileRoot
    Get-ChildItem $profileRoot -Recurse -Filter '*.log*' -ErrorAction SilentlyContinue | Remove-Item -Force
    Write-Tabs $profileRoot (Join-Path $rig $def.tabs)
    $quitMs = ($SettleSeconds + $WindowSeconds + 5) * 1000
    Set-BenchEnv $profileRoot (Join-Path $rig $def.replay) $runOut $quitMs

    Write-Host "scenario $s ($($def.name)): settle $SettleSeconds s, window $WindowSeconds s"
    $launch = Get-Date
    $stdio = Join-Path $WorkDir "stdio-$s"
    New-Item -ItemType Directory -Force $stdio | Out-Null
    $p = Start-Process -FilePath $Exe -PassThru -RedirectStandardOutput (Join-Path $stdio 'stdout.txt') -RedirectStandardError (Join-Path $stdio 'stderr.txt')
    $hwnd = Wait-MainWindow $p
    if ($hwnd -ne [IntPtr]::Zero) {
        $null = [EchoBenchWin]::SetWindowPos($hwnd, [IntPtr]::Zero, 80, 40, $WindowWidth, $WindowHeight, 0x0004)
        $null = [EchoBenchWin]::Focus($hwnd)
    }
    $remaining = $SettleSeconds - ((Get-Date) - $launch).TotalSeconds
    if ($remaining -gt 0) { Start-Sleep -Milliseconds ([int]($remaining * 1000)) }

    $gpu0 = Get-GpuRunTime $p.Id; $gt0 = Get-Date
    $threads0 = Get-ThreadCpu $p
    $p.Refresh(); $cpu0 = $p.TotalProcessorTime.TotalMilliseconds; $t0 = Get-Date
    $samples = @()
    $deadline = $t0.AddSeconds($WindowSeconds)
    while ((Get-Date) -lt $deadline) {
        Start-Sleep -Seconds ([math]::Min($SampleSeconds, [math]::Max(1, ($deadline - (Get-Date)).TotalSeconds)))
        $p.Refresh()
        $samples += [pscustomobject]@{
            t_s = [math]::Round(((Get-Date) - $t0).TotalSeconds, 1)
            cpu_ms = [math]::Round($p.TotalProcessorTime.TotalMilliseconds - $cpu0)
            ws_mb = [math]::Round($p.WorkingSet64 / 1MB, 1)
            private_mb = [math]::Round($p.PrivateMemorySize64 / 1MB, 1)
            foreground = ([EchoBenchWin]::GetForegroundWindow() -eq $hwnd)
        }
    }
    $threads1 = Get-ThreadCpu $p
    $gpu1 = Get-GpuRunTime $p.Id; $gpuWallMs = ((Get-Date) - $gt0).TotalMilliseconds
    $p.Refresh(); $cpu1 = $p.TotalProcessorTime.TotalMilliseconds; $t1 = Get-Date
    $ws1 = $p.WorkingSet64; $priv1 = $p.PrivateMemorySize64
    $forced = Stop-Run $p 30000
    $wallMs = ($t1 - $t0).TotalMilliseconds

    $mainId = ($threads1.GetEnumerator() | Sort-Object { $_.Value.start } | Select-Object -First 1).Key
    $threadRows = foreach ($id in $threads1.Keys) {
        $before = if ($threads0.ContainsKey($id)) { $threads0[$id].ms } else { 0 }
        $d = $threads1[$id].ms - $before
        if ($d -gt 0) {
            $n = [EchoBenchWin]::ThreadName($id)
            if (-not $n) { $n = if ($id -eq $mainId) { 'main' } else { '(unnamed)' } }
            [pscustomobject]@{ name = $n; cpu_ms = $d }
        }
    }
    $byName = $threadRows | Group-Object name | ForEach-Object {
        [pscustomobject]@{ name = $_.Name; threads = $_.Count; cpu_ms = [math]::Round(($_.Group | Measure-Object cpu_ms -Sum).Sum) }
    } | Sort-Object cpu_ms -Descending
    $mainMs = ($byName | Where-Object name -eq 'main' | Measure-Object cpu_ms -Sum).Sum

    $logPath = Join-Path $profileRoot 'logs\echo.log'
    $t0u = $t0.ToUniversalTime(); $t1u = $t1.ToUniversalTime()
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
                $ts = [datetime]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal)
                if ($ts -ge $t0u -and $ts -le $t1u) { $inWindow.Add($line) }
            }
        }
    }
    $perfLines | Set-Content (Join-Path $runOut 'perf-lines.txt')

    function Get-Kv([string] $line) {
        $h = @{}
        foreach ($m in [regex]::Matches($line, '\s(\w+)=("([^"]*)"|[^\s"]+)')) {
            $v = if ($m.Groups[3].Success) { $m.Groups[3].Value } else { $m.Groups[2].Value }
            $h[$m.Groups[1].Value] = $v
        }
        $h
    }
    function Get-Ts([string] $line) {
        $null = $line -match '^(\d{4}-\d\d-\d\dT\d\d:\d\d:\d\d(\.\d+)?)Z'
        [datetime]::Parse($Matches[1], [Globalization.CultureInfo]::InvariantCulture, [Globalization.DateTimeStyles]::AssumeUniversal -bor [Globalization.DateTimeStyles]::AdjustToUniversal)
    }

    $cad = @($inWindow | Where-Object { $_ -match 'cadence_fps=' })
    $profiler = [ordered]@{ lines = $cad.Count }
    if ($cad.Count -ge 2) {
        $first = Get-Kv $cad[0]; $last = Get-Kv $cad[-1]
        $span = ((Get-Ts $cad[-1]) - (Get-Ts $cad[0])).TotalSeconds
        $kvs = $cad | ForEach-Object { Get-Kv $_ }
        foreach ($k in 'cadence_fps', 'work_ms', 'canvas_cpu_ms', 'canvas_paint_ms', 'gpu_ms', 'present_latency_ms', 'p95_ms') {
            $vals = @($kvs | ForEach-Object { $_[$k] } | Where-Object { $_ -and $_ -ne 'NaN' } | ForEach-Object { [double]$_ })
            if ($vals.Count) { $profiler["mean_$k"] = [math]::Round(($vals | Measure-Object -Average).Average, 3) }
        }
        $profiler.span_s = [math]::Round($span, 2)
        if ($first.present_count -and $last.present_count -and $span -gt 0) {
            $profiler.presents_per_s = [math]::Round(([double]$last.present_count - [double]$first.present_count) / $span, 2)
        }
        if ($first.gpu_samples -and $last.gpu_samples -and $span -gt 0) {
            $profiler.gpu_samples_per_s = [math]::Round(([double]$last.gpu_samples - [double]$first.gpu_samples) / $span, 2)
        }
        $c0 = $null; $c1 = $null
        $re = 'frames\[stable:(\d+) scroll:(\d+) rebuild:(\d+) anim:(\d+)\]'
        if ($cad[0] -match $re) { $c0 = $Matches[1..4] | ForEach-Object { [double]$_ } }
        if ($cad[-1] -match $re) { $c1 = $Matches[1..4] | ForEach-Object { [double]$_ } }
        if ($c0 -and $c1 -and $span -gt 0) {
            $profiler.profiler_frames_per_s = [math]::Round((($c1 | Measure-Object -Sum).Sum - ($c0 | Measure-Object -Sum).Sum) / $span, 2)
            $profiler.frame_class_per_s = [ordered]@{
                stable = [math]::Round(($c1[0] - $c0[0]) / $span, 2); scroll = [math]::Round(($c1[1] - $c0[1]) / $span, 2)
                rebuild = [math]::Round(($c1[2] - $c0[2]) / $span, 2); anim = [math]::Round(($c1[3] - $c0[3]) / $span, 2)
            }
        }
        if ($cad[-1] -match 'gpui_draw=p50:([\d.]+) p95:([\d.]+) n:(\d+)') {
            $profiler.gpui_draw_p50_ms = [double]$Matches[1]; $profiler.gpui_draw_p95_ms = [double]$Matches[2]
        }
    }

    $aggLines = @($inWindow | Where-Object { $_ -match 'Canvas render/layout aggregate' })
    $canvas = [ordered]@{}
    foreach ($g in ($aggLines | ForEach-Object { Get-Kv $_ } | Group-Object { $_.channel_id })) {
        $frames = ($g.Group | ForEach-Object { [double]$_.frames } | Measure-Object -Sum).Sum
        $canvas[$g.Name] = [ordered]@{
            aggregate_lines = $g.Count
            frames = $frames
            frames_per_s = [math]::Round($frames / ($wallMs / 1000), 2)
            animation_frame_unchanged_skips = ($g.Group | ForEach-Object { [double]$_.animation_frame_unchanged_skips } | Measure-Object -Sum).Sum
            animation_waker_checks = ($g.Group | ForEach-Object { if ($_.animation_waker_checks) { [double]$_.animation_waker_checks } else { 0 } } | Measure-Object -Sum).Sum
            animation_visible_row_reuse = ($g.Group | ForEach-Object { [double]$_.animation_visible_row_reuse } | Measure-Object -Sum).Sum
            full_rebuilds = ($g.Group | ForEach-Object { [double]$_.full_rebuilds } | Measure-Object -Sum).Sum
        }
    }

    $memLines = @($inWindow | Where-Object { $_ -match 'memory breakdown' } | ForEach-Object { Get-Kv $_ })
    $mem = [ordered]@{ samples = $memLines.Count }
    foreach ($k in 'hot_frames', 'hot_frame_urls', 'hot_frames_max_per_url', 'hot_render_images', 'pending_render_image_drops', 'decoded_frames_mb', 'anim_sources_mb', 'decoded_total_mb', 'name_paint_composites', 'name_paint_composite_retirements', 'gpui_uri_entries', 'gpui_uri_retirements', 'private_mb', 'ws_mb') {
        $vals = @($memLines | ForEach-Object { $_[$k] } | Where-Object { $_ -ne $null } | ForEach-Object { [double]$_ })
        if ($vals.Count) {
            $st = $vals | Measure-Object -Average -Maximum
            $mem[$k] = [ordered]@{ mean = [math]::Round($st.Average, 1); max = $st.Maximum; last = $vals[-1] }
        }
    }

    $memRaw = @($inWindow | Where-Object { $_ -match 'memory breakdown' })
    $counterRates = [ordered]@{}
    if ($memRaw.Count -ge 2) {
        $m0 = Get-Kv $memRaw[0]; $m1 = Get-Kv $memRaw[-1]
        $mspan = ((Get-Ts $memRaw[-1]) - (Get-Ts $memRaw[0])).TotalSeconds
        foreach ($k in 'animation_frame_decompressions', 'animation_refill_windows', 'animation_refill_frames', 'hot_frame_render_images_created', 'render_images_drained', 'render_image_drain_batches') {
            if ($null -ne $m0[$k] -and $null -ne $m1[$k] -and $mspan -gt 0) {
                $counterRates[$k] = [math]::Round(([double]$m1[$k] - [double]$m0[$k]) / $mspan, 2)
            }
        }
        if ($counterRates.Count) { $counterRates.span_s = [math]::Round($mspan, 2) }
    }

    $gpuEngines = [ordered]@{}
    foreach ($eng in $gpu1.Keys) {
        $ms = ($gpu1[$eng] - [double]$gpu0[$eng]) / 10000
        if ($ms -gt 0) { $gpuEngines[$eng] = [ordered]@{ busy_ms = [math]::Round($ms, 1); busy_pct = [math]::Round(100 * $ms / $gpuWallMs, 3) } }
    }
    $fg = @($samples | Where-Object foreground).Count
    $results[$s] = [ordered]@{
        scenario = $def.name
        window_s = [math]::Round($wallMs / 1000, 2)
        cpu_ms = [math]::Round($cpu1 - $cpu0, 1)
        cpu_pct_one_core = [math]::Round(100 * ($cpu1 - $cpu0) / $wallMs, 2)
        main_thread_cpu_ms = $mainMs
        main_thread_pct_one_core = [math]::Round(100 * $mainMs / $wallMs, 2)
        working_set_mb_end = [math]::Round($ws1 / 1MB, 1)
        private_mb_end = [math]::Round($priv1 / 1MB, 1)
        working_set_mb_mean = [math]::Round(($samples | Measure-Object ws_mb -Average).Average, 1)
        private_mb_mean = [math]::Round(($samples | Measure-Object private_mb -Average).Average, 1)
        foreground_samples = "$fg/$($samples.Count)"
        gpu_engine_running_time = $gpuEngines
        profiler = $profiler
        canvas_per_split = $canvas
        memory_breakdown = $mem
        counter_rates_per_s = $counterRates
        threads = @($byName)
        cpu_samples = @($samples)
        irc_channel_joins = $ircJoins
        log_warn_lines = $warnCount
        log_error_lines = $errorCount
        panic = $panic
        exit_code = $p.ExitCode
        forced_kill = $forced
    }
    $results[$s] | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $runOut 'summary.json')
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
    scenarios = $results
}
$summary | ConvertTo-Json -Depth 8 | Set-Content (Join-Path $OutDir 'summary.json')
foreach ($k in $results.Keys) {
    $r = $results[$k]
    '{0} {1}: cpu {2}% main {3}% presents/s {4} profiler frames/s {5} private {6} MB ws {7} MB fg {8}' -f $k, $r.scenario, $r.cpu_pct_one_core, $r.main_thread_pct_one_core, $r.profiler.presents_per_s, $r.profiler.profiler_frames_per_s, $r.private_mb_end, $r.working_set_mb_end, $r.foreground_samples
    if ($r.counter_rates_per_s.Count) {
        '{0} counters/s: {1}' -f $k, (($r.counter_rates_per_s.GetEnumerator() | ForEach-Object { "$($_.Key)=$($_.Value)" }) -join ' ')
    }
}
