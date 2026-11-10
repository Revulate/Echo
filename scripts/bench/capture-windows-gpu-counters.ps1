[CmdletBinding()]
param(
    [int]$TargetProcessId = 0,

    [string]$LaunchPath = "",
    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",
    [hashtable]$LaunchEnvironment = @{},

    [string]$RunDir = "",

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 60,

    [ValidateRange(1, 300)]
    [double]$IntervalSeconds = 5,

    [ValidateSet("close-main-window", "kill", "leave-running")]
    [string]$StopMode = "close-main-window",

    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 15,

    [ValidateRange(60, 86400)]
    [int]$PlateauWindowSeconds = 1800,

    [ValidateRange(0, 86400)]
    [int]$MemoryMapIntervalSeconds = 0,

    [ValidateRange(0, 86400)]
    [int]$MemoryMapWarmupSeconds = 300,

    [ValidateRange(1, 200)]
    [int]$MemoryMapTopAllocationCount = 30,

    [ValidateRange(0, 600)]
    [int]$WatchdogGraceSeconds = 30,

    [string]$RawFileName = "windows-gpu-process-samples.raw.csv",
    [string]$SummaryFileName = "windows-gpu-process-summary.json"
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class EchoGpuCounterWindowCloser
{
    private const uint WmClose = 0x0010;
    public delegate bool EnumWindowsProc(IntPtr window, IntPtr parameter);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool EnumWindows(EnumWindowsProc callback, IntPtr parameter);

    [DllImport("user32.dll")]
    private static extern uint GetWindowThreadProcessId(IntPtr window, out uint processId);

    [DllImport("user32.dll")]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool IsWindowVisible(IntPtr window);

    [DllImport("user32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);

    public static int CloseVisibleWindows(uint targetProcessId)
    {
        int requested = 0;
        EnumWindows((window, parameter) => {
            uint ownerProcessId;
            GetWindowThreadProcessId(window, out ownerProcessId);
            if (ownerProcessId == targetProcessId && IsWindowVisible(window)) {
                if (PostMessage(window, WmClose, IntPtr.Zero, IntPtr.Zero)) {
                    requested++;
                }
            }
            return true;
        }, IntPtr.Zero);
        return requested;
    }
}
"@

function Resolve-RepoRoot {
    return (Resolve-Path (Join-Path (Split-Path -Parent $PSCommandPath) "..\..")).Path
}

function Resolve-ExecutablePath {
    param([Parameter(Mandatory = $true)][string]$Path)

    if (Test-Path $Path) {
        return (Resolve-Path $Path).Path
    }
    $command = Get-Command $Path -ErrorAction SilentlyContinue
    if ($null -ne $command -and -not [string]::IsNullOrWhiteSpace($command.Source)) {
        return $command.Source
    }
    throw "Executable '$Path' was not found."
}

function Stop-OwnedProcess {
    param(
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][string]$Mode,
        [Parameter(Mandatory = $true)][int]$WaitSeconds
    )

    $result = [ordered]@{
        mode = $Mode
        close_requested = $false
        main_window_close_accepted = $false
        visible_windows_close_requested = 0
        force_killed = $false
        exited = $Process.HasExited
        exit_code = if ($Process.HasExited) { $Process.ExitCode } else { $null }
    }
    if ($Process.HasExited -or $Mode -eq "leave-running") {
        return [pscustomobject]$result
    }
    if ($Mode -eq "close-main-window") {
        $result.close_requested = $true
        $Process.Refresh()
        $result.main_window_close_accepted = $Process.CloseMainWindow()
        $result.visible_windows_close_requested =
            [EchoGpuCounterWindowCloser]::CloseVisibleWindows([uint32]$Process.Id)
        if ($Process.WaitForExit($WaitSeconds * 1000)) {
            $result.exited = $true
            $result.exit_code = $Process.ExitCode
            return [pscustomobject]$result
        }
    }
    if (-not $Process.HasExited) {
        $result.force_killed = $true
        $Process.Kill($true)
        $Process.WaitForExit()
    }
    $result.exited = $Process.HasExited
    $result.exit_code = if ($Process.HasExited) { $Process.ExitCode } else { $null }
    return [pscustomobject]$result
}

function Start-OwnedProcessWatchdog {
    param(
        [Parameter(Mandatory = $true)][string]$ScriptPath,
        [Parameter(Mandatory = $true)][System.Diagnostics.Process]$Process,
        [Parameter(Mandatory = $true)][int]$DelaySeconds,
        [Parameter(Mandatory = $true)][int]$ExitWaitSeconds,
        [Parameter(Mandatory = $true)][string]$OutputPath
    )

    $hostExecutable = (Get-Process -Id $PID).Path
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $hostExecutable
    foreach ($argument in @(
        "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $ScriptPath,
        "-TargetProcessId", "$($Process.Id)",
        "-ExpectedStartTimeFileTimeUtc", "$($Process.StartTime.ToUniversalTime().ToFileTimeUtc())",
        "-DelaySeconds", "$DelaySeconds",
        "-ExitWaitSeconds", "$ExitWaitSeconds",
        "-OutputPath", $OutputPath
    )) {
        $startInfo.ArgumentList.Add($argument)
    }
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    return [System.Diagnostics.Process]::Start($startInfo)
}

function Invoke-MemoryMapSnapshot {
    param(
        [Parameter(Mandatory = $true)][string]$ScriptPath,
        [Parameter(Mandatory = $true)][int]$ProcessId,
        [Parameter(Mandatory = $true)][string]$RunPath,
        [Parameter(Mandatory = $true)][string]$Label,
        [Parameter(Mandatory = $true)][int]$TopAllocationCount
    )

    $output = @(& $ScriptPath -TargetProcessId $ProcessId -RunDir $RunPath -Label $Label -TopAllocationCount $TopAllocationCount)
    if ($output.Count -eq 0) {
        throw "Memory-map snapshot '$Label' did not return an artifact path."
    }
    return [string]$output[-1]
}

function Get-Percentile {
    param([double[]]$Values, [double]$Percentile)

    if ($Values.Count -eq 0) { return $null }
    $sorted = [double[]]@($Values | Sort-Object)
    if ($sorted.Count -eq 1) { return $sorted[0] }
    $rank = ($Percentile / 100.0) * ($sorted.Count - 1)
    $lower = [Math]::Floor($rank)
    $upper = [Math]::Ceiling($rank)
    if ($lower -eq $upper) { return $sorted[$lower] }
    $weight = $rank - $lower
    return ($sorted[$lower] * (1.0 - $weight)) + ($sorted[$upper] * $weight)
}

function Get-SeriesSummary {
    param([object[]]$Rows, [Parameter(Mandatory = $true)][string]$Property)

    $values = [double[]]@($Rows | ForEach-Object {
        $value = $_.$Property
        if ($null -ne $value) { [double]$value }
    })
    if ($values.Count -eq 0) {
        return $null
    }
    return [ordered]@{
        sample_count = $values.Count
        min = ($values | Measure-Object -Minimum).Minimum
        avg = ($values | Measure-Object -Average).Average
        p95 = Get-Percentile -Values $values -Percentile 95
        max = ($values | Measure-Object -Maximum).Maximum
        final = $values[-1]
    }
}

function Get-LinearSlope {
    param(
        [object[]]$Rows,
        [Parameter(Mandatory = $true)][string]$Property,
        [Parameter(Mandatory = $true)][int]$WindowSeconds
    )

    if ($Rows.Count -lt 2) { return $null }
    $endSeconds = [double]$Rows[-1].elapsed_seconds
    $startSeconds = [Math]::Max(0.0, $endSeconds - $WindowSeconds)
    $points = @($Rows | Where-Object {
        [double]$_.elapsed_seconds -ge $startSeconds -and $null -ne $_.$Property
    })
    if ($points.Count -lt 2) { return $null }

    $meanX = ($points | ForEach-Object { [double]$_.elapsed_seconds } | Measure-Object -Average).Average
    $meanY = ($points | ForEach-Object { [double]$_.$Property } | Measure-Object -Average).Average
    $numerator = 0.0
    $denominator = 0.0
    foreach ($point in $points) {
        $dx = [double]$point.elapsed_seconds - $meanX
        $numerator += $dx * ([double]$point.$Property - $meanY)
        $denominator += $dx * $dx
    }
    if ($denominator -eq 0.0) { return $null }
    return [ordered]@{
        window_seconds = [Math]::Min($WindowSeconds, $endSeconds)
        sample_count = $points.Count
        bytes_per_second = $numerator / $denominator
        mebibytes_per_minute = (($numerator / $denominator) * 60.0) / 1MB
        net_bytes = [double]$points[-1].$Property - [double]$points[0].$Property
    }
}

function Get-GpuCounterSample {
    param([Parameter(Mandatory = $true)][int]$ProcessId)

    $paths = @(
        "\GPU Process Memory(pid_${ProcessId}_*)\Dedicated Usage",
        "\GPU Process Memory(pid_${ProcessId}_*)\Shared Usage",
        "\GPU Process Memory(pid_${ProcessId}_*)\Local Usage",
        "\GPU Process Memory(pid_${ProcessId}_*)\Non Local Usage",
        "\GPU Process Memory(pid_${ProcessId}_*)\Total Committed",
        "\GPU Engine(pid_${ProcessId}_*engtype_3D)\Utilization Percentage"
    )
    try {
        $samples = @(Get-Counter -Counter $paths -ErrorAction Stop).CounterSamples
    } catch {
        return [ordered]@{
            dedicated = $null; shared = $null; local = $null; non_local = $null
            total_committed = $null; utilization_3d = $null; error = $_.Exception.Message
        }
    }

    function Sum-Counter([string]$suffix) {
        $matching = @($samples | Where-Object { $_.Path.EndsWith($suffix, [StringComparison]::OrdinalIgnoreCase) })
        if ($matching.Count -eq 0) { return $null }
        return [double](($matching | Measure-Object -Property CookedValue -Sum).Sum)
    }

    return [ordered]@{
        dedicated = Sum-Counter "\dedicated usage"
        shared = Sum-Counter "\shared usage"
        local = Sum-Counter "\local usage"
        non_local = Sum-Counter "\non local usage"
        total_committed = Sum-Counter "\total committed"
        utilization_3d = Sum-Counter "\utilization percentage"
        error = $null
    }
}

$repoRoot = Resolve-RepoRoot
if ([string]::IsNullOrWhiteSpace($RunDir)) {
    $RunDir = Join-Path $repoRoot ("benchmarks\results\{0}-windows-gpu-counters" -f (Get-Date -Format "yyyyMMdd-HHmmss"))
}
$runPath = (New-Item -ItemType Directory -Force -Path $RunDir).FullName
$rawPath = Join-Path $runPath $RawFileName
$summaryPath = Join-Path $runPath $SummaryFileName
$memoryMapScript = Join-Path $repoRoot "scripts\bench\capture-process-memory-map.ps1"
$watchdogScript = Join-Path $repoRoot "scripts\bench\watch-owned-process.ps1"
if ($MemoryMapIntervalSeconds -gt 0 -and -not (Test-Path $memoryMapScript)) {
    throw "Memory-map script was not found at '$memoryMapScript'."
}
if (-not (Test-Path $watchdogScript)) {
    throw "Owned-process watchdog was not found at '$watchdogScript'."
}

if ($TargetProcessId -gt 0 -and -not [string]::IsNullOrWhiteSpace($LaunchPath)) {
    throw "Specify either -TargetProcessId or -LaunchPath, not both."
}
if ($TargetProcessId -le 0 -and [string]::IsNullOrWhiteSpace($LaunchPath)) {
    throw "Specify -TargetProcessId or -LaunchPath."
}

$launched = $false
$process = $null
$watchdogProcess = $null
$watchdogPath = Join-Path $runPath "owned-process-watchdog.json"
$shutdown = $null
if (-not [string]::IsNullOrWhiteSpace($LaunchPath)) {
    $launch = Resolve-ExecutablePath -Path $LaunchPath
    $startInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $startInfo.FileName = $launch
    foreach ($argument in $LaunchArgument) { $startInfo.ArgumentList.Add($argument) }
    $startInfo.WorkingDirectory = if ([string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
        Split-Path -Parent $launch
    } else {
        (Resolve-Path $LaunchWorkingDirectory).Path
    }
    $startInfo.UseShellExecute = $false
    foreach ($entry in $LaunchEnvironment.GetEnumerator()) {
        $startInfo.Environment[[string]$entry.Key] = [string]$entry.Value
    }
    $process = [System.Diagnostics.Process]::Start($startInfo)
    $process.Refresh()
    $TargetProcessId = $process.Id
    $launched = $true
    $watchdogProcess = Start-OwnedProcessWatchdog `
        -ScriptPath $watchdogScript `
        -Process $process `
        -DelaySeconds ($DurationSeconds + $WatchdogGraceSeconds) `
        -ExitWaitSeconds $ExitWaitSeconds `
        -OutputPath $watchdogPath
} else {
    $process = Get-Process -Id $TargetProcessId -ErrorAction Stop
}

$rows = [System.Collections.Generic.List[object]]::new()
$counterErrors = [System.Collections.Generic.List[string]]::new()
$memoryMapArtifacts = [System.Collections.Generic.List[object]]::new()
$memoryMapErrors = [System.Collections.Generic.List[string]]::new()
$clock = [System.Diagnostics.Stopwatch]::StartNew()
$nextMemoryMapAt = if ($MemoryMapIntervalSeconds -gt 0) { [double]$MemoryMapWarmupSeconds } else { [double]::PositiveInfinity }
$lastMemoryMapAt = [double]::NegativeInfinity
$lastCpu = $process.TotalProcessorTime.TotalSeconds
$lastCpuElapsed = 0.0
$writer = [System.IO.StreamWriter]::new($rawPath, $false, [System.Text.UTF8Encoding]::new($false))
$wroteHeader = $false
try {
    while ($clock.Elapsed.TotalSeconds -lt $DurationSeconds -and -not $process.HasExited) {
        $sampleStarted = $clock.Elapsed.TotalSeconds
        $process.Refresh()
        $gpu = Get-GpuCounterSample -ProcessId $TargetProcessId
        $process.Refresh()

        $elapsed = $clock.Elapsed.TotalSeconds
        $cpuNow = $process.TotalProcessorTime.TotalSeconds
        $cpuElapsed = $elapsed - $lastCpuElapsed
        $cpuPercent = if ($cpuElapsed -gt 0.0) {
            (($cpuNow - $lastCpu) / $cpuElapsed / [Environment]::ProcessorCount) * 100.0
        } else { 0.0 }
        $lastCpu = $cpuNow
        $lastCpuElapsed = $elapsed

        if (-not [string]::IsNullOrWhiteSpace($gpu.error) -and -not $counterErrors.Contains($gpu.error)) {
            $counterErrors.Add($gpu.error)
        }
        $row = [pscustomobject][ordered]@{
            sample_index = $rows.Count
            timestamp_utc = [DateTime]::UtcNow.ToString("o")
            elapsed_seconds = [Math]::Round($elapsed, 6)
            process_id = $TargetProcessId
            working_set_bytes = [double]$process.WorkingSet64
            private_memory_bytes = [double]$process.PrivateMemorySize64
            handle_count = [double]$process.HandleCount
            thread_count = [double]$process.Threads.Count
            cpu_percent_all_processors = [double]$cpuPercent
            gpu_dedicated_bytes = $gpu.dedicated
            gpu_shared_bytes = $gpu.shared
            gpu_local_bytes = $gpu.local
            gpu_non_local_bytes = $gpu.non_local
            gpu_total_committed_bytes = $gpu.total_committed
            gpu_3d_utilization_percent = $gpu.utilization_3d
        }
        $rows.Add($row)
        $csv = @($row | ConvertTo-Csv -NoTypeInformation)
        if (-not $wroteHeader) {
            $writer.WriteLine($csv[0])
            $wroteHeader = $true
        }
        $writer.WriteLine($csv[-1])
        $writer.Flush()

        if ($MemoryMapIntervalSeconds -gt 0 -and $elapsed -ge $nextMemoryMapAt -and -not $process.HasExited) {
            $minutes = [int][Math]::Floor($elapsed / 60.0)
            $seconds = [int][Math]::Floor($elapsed)
            $label = "elapsed-{0:D5}s-{1:D4}m" -f $seconds, $minutes
            try {
                $artifact = Invoke-MemoryMapSnapshot `
                    -ScriptPath $memoryMapScript `
                    -ProcessId $TargetProcessId `
                    -RunPath $runPath `
                    -Label $label `
                    -TopAllocationCount $MemoryMapTopAllocationCount
                $memoryMapArtifacts.Add([pscustomobject][ordered]@{
                    label = $label
                    elapsed_seconds = $elapsed
                    summary_path = $artifact
                })
                $lastMemoryMapAt = $elapsed
            } catch {
                $memoryMapErrors.Add("${label}: $($_.Exception.Message)")
            }
            do {
                $nextMemoryMapAt += $MemoryMapIntervalSeconds
            } while ($nextMemoryMapAt -le $elapsed)
        }

        $nextAt = $sampleStarted + $IntervalSeconds
        $remainingMs = [Math]::Floor(($nextAt - $clock.Elapsed.TotalSeconds) * 1000.0)
        if ($remainingMs -gt 0) { Start-Sleep -Milliseconds $remainingMs }
    }
} finally {
    $finalElapsed = $clock.Elapsed.TotalSeconds
    if (
        $MemoryMapIntervalSeconds -gt 0 -and
        $null -ne $process -and
        -not $process.HasExited -and
        ($finalElapsed - $lastMemoryMapAt) -ge [Math]::Max(15.0, $IntervalSeconds * 2.0)
    ) {
        $minutes = [int][Math]::Floor($finalElapsed / 60.0)
        $seconds = [int][Math]::Floor($finalElapsed)
        $label = "final-{0:D5}s-{1:D4}m" -f $seconds, $minutes
        try {
            $artifact = Invoke-MemoryMapSnapshot `
                -ScriptPath $memoryMapScript `
                -ProcessId $TargetProcessId `
                -RunPath $runPath `
                -Label $label `
                -TopAllocationCount $MemoryMapTopAllocationCount
            $memoryMapArtifacts.Add([pscustomobject][ordered]@{
                label = $label
                elapsed_seconds = $finalElapsed
                summary_path = $artifact
            })
            $lastMemoryMapAt = $finalElapsed
        } catch {
            $memoryMapErrors.Add("${label}: $($_.Exception.Message)")
        }
    }
    $clock.Stop()
    $writer.Dispose()
    if ($launched -and $null -ne $process) {
        $shutdown = Stop-OwnedProcess -Process $process -Mode $StopMode -WaitSeconds $ExitWaitSeconds
    }
    if ($null -ne $watchdogProcess -and -not $watchdogProcess.HasExited) {
        $watchdogProcess.Kill($true)
        $watchdogProcess.WaitForExit()
    }
}

$rowArray = [object[]]$rows.ToArray()
$gpuSampleCount = @($rowArray | Where-Object { $null -ne $_.gpu_dedicated_bytes }).Count
$summary = [ordered]@{
    schema_version = 1
    tool = "Windows GPU Process Memory and GPU Engine performance counters"
    status = if ($gpuSampleCount -gt 0) { "sampled" } else { "tool-gated" }
    measurements_recorded = $gpuSampleCount -gt 0
    process_id = $TargetProcessId
    launched = $launched
    launch_path = if ($launched) { $launch } else { $null }
    duration_requested_seconds = $DurationSeconds
    duration_observed_seconds = $clock.Elapsed.TotalSeconds
    interval_seconds = $IntervalSeconds
    sample_count = $rowArray.Count
    gpu_sample_count = $gpuSampleCount
    raw_artifact = $rawPath
    counter_errors = $counterErrors
    memory_map = [ordered]@{
        enabled = $MemoryMapIntervalSeconds -gt 0
        interval_seconds = if ($MemoryMapIntervalSeconds -gt 0) { $MemoryMapIntervalSeconds } else { $null }
        warmup_seconds = if ($MemoryMapIntervalSeconds -gt 0) { $MemoryMapWarmupSeconds } else { $null }
        artifacts = $memoryMapArtifacts
        errors = $memoryMapErrors
    }
    owned_process_shutdown = if ($launched) { $shutdown } else { $null }
    watchdog = [ordered]@{
        armed = $launched
        delay_seconds = if ($launched) { $DurationSeconds + $WatchdogGraceSeconds } else { $null }
        process_id = if ($null -ne $watchdogProcess) { $watchdogProcess.Id } else { $null }
        fired_artifact = $watchdogPath
        fired = Test-Path $watchdogPath
        stopped_after_owned_shutdown = $launched -and -not (Test-Path $watchdogPath)
    }
    metrics = [ordered]@{
        working_set_bytes = Get-SeriesSummary -Rows $rowArray -Property "working_set_bytes"
        private_memory_bytes = Get-SeriesSummary -Rows $rowArray -Property "private_memory_bytes"
        handle_count = Get-SeriesSummary -Rows $rowArray -Property "handle_count"
        thread_count = Get-SeriesSummary -Rows $rowArray -Property "thread_count"
        cpu_percent_all_processors = Get-SeriesSummary -Rows $rowArray -Property "cpu_percent_all_processors"
        gpu_dedicated_bytes = Get-SeriesSummary -Rows $rowArray -Property "gpu_dedicated_bytes"
        gpu_shared_bytes = Get-SeriesSummary -Rows $rowArray -Property "gpu_shared_bytes"
        gpu_local_bytes = Get-SeriesSummary -Rows $rowArray -Property "gpu_local_bytes"
        gpu_non_local_bytes = Get-SeriesSummary -Rows $rowArray -Property "gpu_non_local_bytes"
        gpu_total_committed_bytes = Get-SeriesSummary -Rows $rowArray -Property "gpu_total_committed_bytes"
        gpu_3d_utilization_percent = Get-SeriesSummary -Rows $rowArray -Property "gpu_3d_utilization_percent"
    }
    plateau = [ordered]@{
        working_set = Get-LinearSlope -Rows $rowArray -Property "working_set_bytes" -WindowSeconds $PlateauWindowSeconds
        private_memory = Get-LinearSlope -Rows $rowArray -Property "private_memory_bytes" -WindowSeconds $PlateauWindowSeconds
        dedicated_gpu_memory = Get-LinearSlope -Rows $rowArray -Property "gpu_dedicated_bytes" -WindowSeconds $PlateauWindowSeconds
    }
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
}
$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding utf8
Write-Output $summaryPath
