[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$PresentMonPath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$LaunchPath,

    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$Scenario = "echo gpu frame capture",

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 12,

    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 10,

    [ValidateSet("close-main-window", "kill", "leave-running")]
    [string]$StopMode = "close-main-window",

    [switch]$CaptureCpu,
    [ValidateRange(100, 60000)]
    [int]$CpuIntervalMilliseconds = 1000,

    [string]$App = "echo",
    [string]$EchoBuildType = "",
    [string]$CacheState = "",
    [string]$NetworkState = "",
    [string]$FixtureName = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
    $scriptRoot = Split-Path -Parent $PSCommandPath
    return (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
}

function Resolve-ExecutablePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (Test-Path $Path) {
        return (Resolve-Path $Path).Path
    }

    $command = Get-Command $Path -ErrorAction SilentlyContinue
    if ($null -ne $command -and -not [string]::IsNullOrWhiteSpace($command.Source)) {
        return $command.Source
    }

    throw "Executable '$Path' was not found as a file or command."
}

function Initialize-RunDirectory {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepoRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
        return (New-Item -ItemType Directory -Force -Path $RunDir).FullName
    }

    $initializer = Join-Path $RepoRoot "scripts\bench\new-benchmark-run.ps1"
    if (-not (Test-Path $initializer)) {
        throw "Benchmark run initializer not found: $initializer"
    }

    $args = @{
        Scenario = $Scenario
        EchoPath = $LaunchPath
        EchoBuildType = $EchoBuildType
        CacheState = $CacheState
        NetworkState = $NetworkState
        FixtureName = $FixtureName
        Notes = $Notes
    }
    if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) {
        $args.OutputRoot = $OutputRoot
    }

    $output = & $initializer @args
    $path = (($output | Select-Object -Last 1) | Out-String).Trim()
    if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path $path)) {
        throw "Benchmark run initializer did not return an existing path."
    }

    return (Resolve-Path $path).Path
}

function Stop-LaunchedProcess {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process,

        [Parameter(Mandatory = $true)]
        [string]$Mode,

        [Parameter(Mandatory = $true)]
        [int]$WaitSeconds
    )

    if ($Process.HasExited -or $Mode -eq "leave-running") {
        return
    }

    if ($Mode -eq "close-main-window") {
        $null = $Process.CloseMainWindow()
        if ($Process.WaitForExit($WaitSeconds * 1000)) {
            return
        }
    }

    if (-not $Process.HasExited) {
        $Process.Kill($true)
        $Process.WaitForExit()
    }
}

function Stop-PresentMonProcess {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    if ($Process.HasExited) {
        return
    }

    $Process.Kill($true)
    $Process.WaitForExit()
}

$repoRoot = Resolve-RepoRoot
$presentMon = Resolve-ExecutablePath -Path $PresentMonPath
$launch = Resolve-ExecutablePath -Path $LaunchPath
$runPath = Initialize-RunDirectory -RepoRoot $repoRoot

$rawTrace = Join-Path $runPath "gpu-presentmon-trace.csv"
$summaryPath = Join-Path $runPath "gpu-presentmon-summary.json"
$metadataPath = Join-Path $runPath "gpu-presentmon-capture.json"
$presentMonStdoutPath = Join-Path $runPath "gpu-presentmon.stdout.raw.txt"
$presentMonStderrPath = Join-Path $runPath "gpu-presentmon.stderr.raw.txt"

$savedErrorActionPreference = $ErrorActionPreference
$ErrorActionPreference = "Continue"
$versionOutput = (& $presentMon --help 2>&1 | Select-Object -First 6) -join "`n"
$ErrorActionPreference = $savedErrorActionPreference
$presentMonHash = (Get-FileHash -Algorithm SHA256 -Path $presentMon).Hash
$presentMonInfo = [ordered]@{
    path = $presentMon
    version_probe = $versionOutput
    sha256 = $presentMonHash
    arguments = @()
    trace_path = $rawTrace
    session_name = $null
    exit_code = $null
    timed_out = $false
}

$startInfo = [System.Diagnostics.ProcessStartInfo]::new()
$startInfo.FileName = $launch
foreach ($argument in $LaunchArgument) {
    $startInfo.ArgumentList.Add($argument)
}
if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
    $startInfo.WorkingDirectory = (Resolve-Path $LaunchWorkingDirectory).Path
} else {
    $startInfo.WorkingDirectory = Split-Path -Parent $launch
}
$startInfo.UseShellExecute = $false

$launchedProcess = $null
$launchedProcessName = $null
$presentMonProcess = $null
$cpuSamplerProcess = $null
$captureStartedAt = $null
$captureFinishedAt = $null
try {
    $launchedProcess = [System.Diagnostics.Process]::Start($startInfo)
    $launchedProcess.Refresh()
    $launchedProcessName = $launchedProcess.ProcessName

    # Never contend with a stale/global PresentMon ETW session. An interrupted
    # capture can leave the default "PresentMon" session registered, and the
    # wrapper would then spend the requested duration sampling CPU without
    # collecting a frame trace.
    $presentMonSession = "Echo-$($launchedProcess.Id)-$([Guid]::NewGuid().ToString('N').Substring(0, 8))"
    $presentMonArgs = @(
        "--process_id", "$($launchedProcess.Id)",
        "--output_file", $rawTrace,
        "--timed", "$DurationSeconds",
        "--terminate_after_timed",
        "--session_name", $presentMonSession,
        "--stop_existing_session"
    )
    $presentMonInfo.arguments = $presentMonArgs
    $presentMonInfo.session_name = $presentMonSession

    $presentMonStartInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $presentMonStartInfo.FileName = $presentMon
    foreach ($argument in $presentMonArgs) {
        $presentMonStartInfo.ArgumentList.Add($argument)
    }
    $presentMonStartInfo.WorkingDirectory = $runPath
    $presentMonStartInfo.UseShellExecute = $false
    $presentMonStartInfo.RedirectStandardOutput = $true
    $presentMonStartInfo.RedirectStandardError = $true
    $presentMonStartInfo.CreateNoWindow = $true

    if ($CaptureCpu) {
        $sampler = Join-Path $repoRoot "scripts\bench\sample-process.ps1"
        $cpuArgs = @(
            "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", $sampler,
            "-App", $App,
            "-RunDir", $runPath,
            "-TargetProcessId", "$($launchedProcess.Id)",
            "-DurationSeconds", "$DurationSeconds",
            "-IntervalMilliseconds", "$CpuIntervalMilliseconds",
            "-MetricName", "synchronized_cpu",
            "-RawFileName", "synchronized-cpu-samples.raw.csv"
        )
        $cpuSamplerProcess = Start-Process -FilePath "powershell" -ArgumentList $cpuArgs -PassThru -WindowStyle Hidden
    }

    $captureStartedAt = [DateTime]::UtcNow
    $presentMonProcess = [System.Diagnostics.Process]::Start($presentMonStartInfo)
    $presentMonExited = $presentMonProcess.WaitForExit(($DurationSeconds + 15) * 1000)
    if (-not $presentMonExited) {
        $presentMonInfo.timed_out = $true
        Stop-PresentMonProcess -Process $presentMonProcess
    }
    if ($presentMonProcess.HasExited) {
        $presentMonInfo.exit_code = $presentMonProcess.ExitCode
    }
    $presentMonProcess.StandardOutput.ReadToEnd() | Set-Content -Path $presentMonStdoutPath -Encoding utf8
    $presentMonProcess.StandardError.ReadToEnd() | Set-Content -Path $presentMonStderrPath -Encoding utf8
    if ($null -ne $cpuSamplerProcess) {
        $cpuSamplerProcess.WaitForExit(($DurationSeconds + 15) * 1000) | Out-Null
    }
    $captureFinishedAt = [DateTime]::UtcNow
} finally {
    if ($null -ne $launchedProcess) {
        Stop-LaunchedProcess -Process $launchedProcess -Mode $StopMode -WaitSeconds $ExitWaitSeconds
    }
    if ($null -ne $presentMonProcess) {
        Stop-PresentMonProcess -Process $presentMonProcess
    }
}

$traceRecorded = (Test-Path $rawTrace) -and (Get-Item $rawTrace).Length -gt 0
if (-not $traceRecorded) {
    $stderrText = (Get-Content -Raw -ErrorAction SilentlyContinue $presentMonStderrPath)
    $failureReason = if (-not [string]::IsNullOrWhiteSpace($stderrText)) {
        "PresentMon did not produce a PID-scoped trace: $($stderrText.Trim())"
    } elseif ($null -ne $presentMonInfo.exit_code) {
        "PresentMon exited with code $($presentMonInfo.exit_code) without producing a PID-scoped trace"
    } else {
        "PresentMon did not produce a PID-scoped trace"
    }
    $failureMetadata = [ordered]@{
        tool = "PresentMon"; presentmon = $presentMonInfo; app = $App
        launched_process_id = if ($null -ne $launchedProcess) { $launchedProcess.Id } else { $null }
        status = "tool-gated"; measurements_recorded = $false
        stdout = $presentMonStdoutPath; stderr = $presentMonStderrPath
        reason = $failureReason
        captured_at_utc = [DateTime]::UtcNow.ToString("o")
    }
    $failureMetadata | ConvertTo-Json -Depth 8 | Set-Content -Path $metadataPath -Encoding utf8
    throw "PresentMon did not produce a trace at $rawTrace. See $metadataPath"
}

$summarizer = Join-Path $repoRoot "scripts\bench\summarize-gpu-trace.ps1"
& $summarizer -TracePath $rawTrace -App $App -RunDir $runPath -OutputFile $summaryPath -TargetProcessId $launchedProcess.Id

$metadata = [pscustomobject]@{
    tool = "PresentMon"
    presentmon = $presentMonInfo
    app = $App
    launch_path = $launch
    launch_arguments = $LaunchArgument
    launched_process_id = $launchedProcess.Id
    launched_process_name = $launchedProcessName
    duration_seconds = $DurationSeconds
    capture_started_at_utc = if ($null -ne $captureStartedAt) { $captureStartedAt.ToString("o") } else { $null }
    capture_finished_at_utc = if ($null -ne $captureFinishedAt) { $captureFinishedAt.ToString("o") } else { $null }
    cpu_capture_enabled = [bool]$CaptureCpu
    cpu_interval_milliseconds = if ($CaptureCpu) { $CpuIntervalMilliseconds } else { $null }
    cpu_raw_artifact = if ($CaptureCpu) { Join-Path $runPath "synchronized-cpu-samples.raw.csv" } else { $null }
    stop_mode = $StopMode
    raw_trace = $rawTrace
    summary = $summaryPath
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
}
$metadata | ConvertTo-Json -Depth 8 | Set-Content -Path $metadataPath -Encoding UTF8

Write-Output $summaryPath
