[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$LaunchPath,
    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [ValidateRange(20, 1000)]
    [int]$Cycles = 20,
    [ValidateRange(1, 3600)]
    [int]$WarmPlateauSeconds = 300,
    [ValidateRange(1, 3600)]
    [int]$SettlingSeconds = 300,
    [ValidateRange(10, 10000)]
    [int]$CycleDelayMilliseconds = 500,
    [ValidateRange(100, 60000)]
    [int]$SampleIntervalMilliseconds = 1000,
    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 15,
    [string]$ReplayPath = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot { (Resolve-Path (Join-Path (Split-Path -Parent $PSCommandPath) "..\..")).Path }
function Resolve-Executable([string]$Path) {
    if (Test-Path $Path) { return (Resolve-Path $Path).Path }
    $command = Get-Command $Path -ErrorAction SilentlyContinue
    if ($null -ne $command) { return $command.Source }
    throw "LaunchPath '$Path' was not found."
}
function Set-ProcessEnvironment([hashtable]$Previous, [string]$Name, [string]$Value) {
    $Previous[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
}
function Restore-ProcessEnvironment([hashtable]$Previous) {
    foreach ($entry in $Previous.GetEnumerator()) {
        [Environment]::SetEnvironmentVariable([string]$entry.Key, $entry.Value, "Process")
    }
}
function Get-LinearSlope([object[]]$Rows, [string]$Property) {
    if ($Rows.Count -lt 2) { return $null }
    $xs = @($Rows | ForEach-Object { [double]$_.elapsed_seconds })
    $ys = @($Rows | ForEach-Object { [double]$_.$Property })
    $meanX = ($xs | Measure-Object -Average).Average
    $meanY = ($ys | Measure-Object -Average).Average
    $numerator = 0.0; $denominator = 0.0
    for ($i = 0; $i -lt $xs.Count; $i++) {
        $dx = $xs[$i] - $meanX
        $numerator += $dx * ($ys[$i] - $meanY)
        $denominator += $dx * $dx
    }
    if ($denominator -eq 0.0) { return 0.0 }
    return $numerator / $denominator
}

$repoRoot = Resolve-RepoRoot
$launch = Resolve-Executable $LaunchPath
if ([string]::IsNullOrWhiteSpace($RunDir)) {
    $stamp = [DateTime]::Now.ToString("yyyyMMdd-HHmmss")
    $root = if ([string]::IsNullOrWhiteSpace($OutputRoot)) { Join-Path $repoRoot "benchmarks\results" } else { $OutputRoot }
    $RunDir = Join-Path $root "$stamp-lifecycle-churn"
}
$runPath = (New-Item -ItemType Directory -Force -Path $RunDir).FullName
$logPath = Join-Path $runPath "lifecycle-churn.log"
$stdoutPath = Join-Path $runPath "lifecycle-churn.stdout.raw.txt"
$stderrPath = Join-Path $runPath "lifecycle-churn.stderr.raw.txt"
$samplePath = Join-Path $runPath "lifecycle-process-samples.raw.csv"
$summaryPath = Join-Path $runPath "lifecycle-churn-summary.json"
$replayReportPath = Join-Path $runPath "echo-chat-replay-run.json"
if ([string]::IsNullOrWhiteSpace($ReplayPath)) {
    throw "-ReplayPath is required; generate a fixture with scripts\bench\new-chat-replay-fixture.ps1 -ReplayScenario high-volume."
}
if (-not (Test-Path $ReplayPath)) { throw "Lifecycle replay fixture not found: $ReplayPath" }
$ReplayPath = (Resolve-Path $ReplayPath).Path
$totalSeconds = $WarmPlateauSeconds + $SettlingSeconds + [Math]::Ceiling(($Cycles * $CycleDelayMilliseconds) / 1000.0) + 30

$previous = @{}
$process = $null
try {
    foreach ($pair in ([ordered]@{
        ECHO_BENCH_UI_FIXTURE = "1"
        ECHO_BENCH_LIFECYCLE_CHURN = "1"
        ECHO_BENCH_LIFECYCLE_CYCLES = [string]$Cycles
        ECHO_BENCH_LIFECYCLE_CHURN_DELAY_MS = [string]($WarmPlateauSeconds * 1000)
        ECHO_BENCH_LIFECYCLE_SETTLE_MS = [string]($SettlingSeconds * 1000)
        ECHO_BENCH_LIFECYCLE_CHURN_STEP_DELAY_MS = [string]$CycleDelayMilliseconds
        ECHO_LOG_FILE = "1"
        ECHO_LOG_FILE_PATH = $logPath
        ECHO_LOG_UI = "1"
        ECHO_LOG_PERF = "1"
        ECHO_LOG_PERF_RENDER = "1"
        ECHO_LOG_PERF_EMOTES = "1"
        ECHO_CHAT_REPLAY = $ReplayPath
        ECHO_CHAT_REPLAY_REPORT = $replayReportPath
        ECHO_CHAT_REPLAY_CHANNEL = "echo-offline-fixture"
        ECHO_CHAT_REPLAY_CLEAR_CHANNEL = "1"
    }).GetEnumerator()) { Set-ProcessEnvironment $previous ([string]$pair.Key) ([string]$pair.Value) }

    $start = @{ FilePath = $launch; PassThru = $true; RedirectStandardOutput = $stdoutPath; RedirectStandardError = $stderrPath }
    if ($LaunchArgument.Count -gt 0) { $start.ArgumentList = $LaunchArgument }
    $start.WorkingDirectory = if ([string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) { Split-Path -Parent $launch } else { (Resolve-Path $LaunchWorkingDirectory).Path }
    $process = Start-Process @start

    & (Join-Path $repoRoot "scripts\bench\sample-process.ps1") -App echo -RunDir $runPath -TargetProcessId $process.Id -DurationSeconds $totalSeconds -IntervalMilliseconds $SampleIntervalMilliseconds -MetricName lifecycle_process -RawFileName (Split-Path -Leaf $samplePath) | Out-Null
} finally {
    if ($null -ne $process) {
        $process.Refresh()
        if (-not $process.HasExited) {
            $null = $process.CloseMainWindow()
            if (-not $process.WaitForExit($ExitWaitSeconds * 1000)) { $process.Kill($true); $process.WaitForExit() }
        }
    }
    Restore-ProcessEnvironment $previous
}

if (-not (Test-Path $logPath)) { throw "Lifecycle driver did not produce $logPath" }
$markers = @(Select-String -Path $logPath -Pattern 'bench\.lifecycle_churn')
$cycleMarkers = @($markers | Where-Object { $_.Line -match 'phase=cycle' -and $_.Line -match 'acknowledged=true' })
$checkpointMarkers = @($markers | Where-Object {
    $_.Line -match 'phase=checkpoint' -and
    $_.Line -match 'acknowledged=true' -and
    $_.Line -match 'channel_removed=true' -and
    $_.Line -match 'popout_open=false' -and
    $_.Line -match 'queue_disconnected_total=0' -and
    $_.Line -match 'worker_panic_total=0'
})
$completeMarker = $markers | Where-Object { $_.Line -match 'phase=complete' } | Select-Object -Last 1
$samples = @(Import-Csv $samplePath)
for ($i = 0; $i -lt $samples.Count; $i++) {
    $samples[$i] | Add-Member -NotePropertyName elapsed_seconds -NotePropertyValue ($i * $SampleIntervalMilliseconds / 1000.0)
}
$lastElapsed = if ($samples.Count -gt 0) { [double]$samples[-1].elapsed_seconds } else { 0.0 }
$settleStart = [Math]::Max(0.0, $lastElapsed - $SettlingSeconds)
$settled = @($samples | Where-Object { [double]$_.elapsed_seconds -ge $settleStart })
$handleSlope = Get-LinearSlope $settled "handle_count"
$threadSlope = Get-LinearSlope $settled "thread_count"
$privateSlope = Get-LinearSlope $settled "private_memory_bytes"
$handleNet = if ($settled.Count -gt 1) { [int64]$settled[-1].handle_count - [int64]$settled[0].handle_count } else { 0 }
$threadNet = if ($settled.Count -gt 1) { [int64]$settled[-1].thread_count - [int64]$settled[0].thread_count } else { 0 }
$privateNet = if ($settled.Count -gt 1) { [int64]$settled[-1].private_memory_bytes - [int64]$settled[0].private_memory_bytes } else { 0 }
$monotonicHandles = $handleNet -gt 2 -or $handleSlope -gt 0.01
$monotonicThreads = $threadNet -gt 1 -or $threadSlope -gt 0.01
$persistentPrivateGrowth = $privateSlope -gt 16384 -or $privateNet -gt 16777216
$completePassed = $null -ne $completeMarker -and $completeMarker.Line -match 'passed=true'
$popoutMarkers = @($markers | Where-Object { $_.Line -match 'phase=popout' -and $_.Line -match 'closed=true' -and $_.Line -match 'open_after=false' })
$passed = $cycleMarkers.Count -ge $Cycles -and $checkpointMarkers.Count -ge $Cycles -and $popoutMarkers.Count -ge $Cycles -and $completePassed -and -not $monotonicHandles -and -not $monotonicThreads -and -not $persistentPrivateGrowth

$summary = [ordered]@{
    schema_version = 1; created_at_utc = [DateTime]::UtcNow.ToString("o"); status = if ($passed) { "pass" } else { "fail" }
    hermetic = $true; no_send = $true; requested_cycles = $Cycles; acknowledged_cycles = $cycleMarkers.Count
    warm_plateau_seconds = $WarmPlateauSeconds; settling_seconds = $SettlingSeconds; sample_count = $samples.Count
    complete_marker_observed = ($null -ne $completeMarker); complete_marker_passed = $completePassed; acknowledged_popout_closes = $popoutMarkers.Count; acknowledged_cleanup_checkpoints = $checkpointMarkers.Count; monotonically_increasing_handles = $monotonicHandles; monotonically_increasing_threads = $monotonicThreads; persistent_private_memory_growth = $persistentPrivateGrowth
    settled_slopes_per_second = [ordered]@{ private_memory_bytes = $privateSlope; handles = $handleSlope; threads = $threadSlope }
    settled_net_change = [ordered]@{ private_memory_bytes = $privateNet; handles = $handleNet; threads = $threadNet }
    acceptance_thresholds = [ordered]@{ private_memory_slope_bytes_per_second_max = 16384; private_memory_net_bytes_max = 16777216; handle_net_max = 2; thread_net_max = 1; handle_slope_per_second_max = 0.01; thread_slope_per_second_max = 0.01 }
    acceptance_note = "Fail-closed on missing cycle checkpoints, owner cleanup residue, worker failures, or persistent settled process-resource growth."
    artifacts = [ordered]@{ runtime_log = $logPath; process_samples = $samplePath; stdout = $stdoutPath; stderr = $stderrPath }
    replay = [ordered]@{ fixture = $ReplayPath; report = $replayReportPath; report_present = (Test-Path $replayReportPath) }
}
$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $summaryPath -Encoding utf8
if (-not $passed) { throw "Lifecycle churn acceptance failed. See $summaryPath" }
Write-Output $runPath
