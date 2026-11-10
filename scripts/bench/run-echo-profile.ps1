<#
.SYNOPSIS
Launch Echo with profiler logging enabled and record raw profile artifacts for
one run. run-echo-profile.py is the Python twin.
#>
[CmdletBinding()]
param(
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$Scenario = "echo profile run",

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$LaunchPath,

    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 12,

    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 10,

    [ValidateSet("close-main-window", "kill", "leave-running")]
    [string]$StopMode = "close-main-window",

    [switch]$EnableScrollSmoke,
    [string]$ScrollSmokeSequence = "",
    [int]$ScrollSmokeDelayMs = 0,
    [int]$ScrollSmokeStepDelayMs = 0,

    [string]$ProfileName = "echo-profile",
    [string]$ProfileLogCheckPath = "",
    [string]$EchoBuildType = "",
    [string]$CacheState = "",
    [string]$NetworkState = "",
    [string]$FixtureName = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function Stop-ProfileProcess {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    if ($StopMode -eq "leave-running") {
        return [ordered]@{
            requested = $false
            method = "leave-running"
            graceful_close_requested = $false
            killed = $false
            exited_after_stop = $false
        }
    }

    $gracefulCloseRequested = $false
    $killed = $false

    try {
        $Process.Refresh()
        if ($Process.HasExited) {
            return [ordered]@{
                requested = $false
                method = "already-exited"
                graceful_close_requested = $false
                killed = $false
                exited_after_stop = $true
            }
        }

        if ($StopMode -eq "close-main-window" -and $Process.MainWindowHandle -ne [IntPtr]::Zero) {
            $gracefulCloseRequested = [bool]$Process.CloseMainWindow()
        }

        if ($gracefulCloseRequested) {
            try {
                Wait-Process -Id $Process.Id -Timeout $ExitWaitSeconds -ErrorAction Stop | Out-Null
            } catch {
                # Fall through and kill if the graceful close did not complete.
            }
        }

        $Process.Refresh()
        if (-not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force
            $killed = $true
            try {
                Wait-Process -Id $Process.Id -Timeout $ExitWaitSeconds -ErrorAction Stop | Out-Null
            } catch {
                # The process may still be terminating; status is recorded below.
            }
        }

        $Process.Refresh()
        return [ordered]@{
            requested = $true
            method = $StopMode
            graceful_close_requested = $gracefulCloseRequested
            killed = $killed
            exited_after_stop = [bool]$Process.HasExited
        }
    } catch {
        return [ordered]@{
            requested = $true
            method = $StopMode
            graceful_close_requested = $gracefulCloseRequested
            killed = $killed
            exited_after_stop = $false
            error = $_.Exception.Message
        }
    }
}

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-BenchRunDirectory -RepoRoot $repoRoot -RunDir $RunDir -Metadata @{
    Scenario = $Scenario
    OutputRoot = $OutputRoot
    EchoPath = $LaunchPath
    EchoBuildType = $EchoBuildType
    CacheState = $CacheState
    NetworkState = $NetworkState
    FixtureName = $FixtureName
    Notes = $Notes
}
$resolvedLaunchPath = Resolve-ExecutablePath -Path $LaunchPath -Label "LaunchPath"
$resolvedWorkingDirectory = if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
    (New-Item -ItemType Directory -Force -Path $LaunchWorkingDirectory).FullName
} else {
    $repoRoot
}

$safeProfileName = ($ProfileName.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-").Trim("-")
if ([string]::IsNullOrWhiteSpace($safeProfileName)) {
    $safeProfileName = "echo-profile"
}

$stdoutPath = Join-Path $scenarioDir "$safeProfileName.stdout.raw.txt"
$stderrPath = Join-Path $scenarioDir "$safeProfileName.stderr.raw.txt"
$benchmarkProfileRoot = [Environment]::GetEnvironmentVariable("ECHO_BENCH_PROFILE_ROOT")
$logPath = if ([string]::IsNullOrWhiteSpace($benchmarkProfileRoot)) {
    Join-Path $scenarioDir "$safeProfileName.log"
} else {
    if (-not [System.IO.Path]::IsPathRooted($benchmarkProfileRoot)) {
        throw "ECHO_BENCH_PROFILE_ROOT must be absolute."
    }
    Join-Path ([System.IO.Path]::GetFullPath($benchmarkProfileRoot)) "logs/echo.log"
}
$runManifestPath = Join-Path $scenarioDir "echo-profile-run.json"
$profileLogCheckRawPath = Join-Path $scenarioDir "profile-log-check.raw.txt"

$profileEnv = [ordered]@{
    ECHO_FPS_PROFILER = "1"
    ECHO_PERF_MONITOR = "1"
    ECHO_LOG_FILE = "1"
    ECHO_LOG_FILE_PATH = $logPath
    ECHO_LOG_PERF = "1"
    ECHO_LOG_PERF_RENDER = "1"
    ECHO_LOG_PERF_EMOTES = "1"
    ECHO_LOG_IRC = "1"
    ECHO_LOG_EMOTES = "1"
    ECHO_LOG_NET = "1"
    ECHO_LOG_UI = "1"
}

if ($EnableScrollSmoke) {
    $profileEnv["ECHO_BENCH_SCROLL_SMOKE"] = "1"
    if (-not [string]::IsNullOrWhiteSpace($ScrollSmokeSequence)) {
        $profileEnv["ECHO_BENCH_SCROLL_SMOKE_SEQUENCE"] = $ScrollSmokeSequence
    }
    if ($ScrollSmokeDelayMs -gt 0) {
        $profileEnv["ECHO_BENCH_SCROLL_SMOKE_DELAY_MS"] = [string]$ScrollSmokeDelayMs
    }
    if ($ScrollSmokeStepDelayMs -gt 0) {
        $profileEnv["ECHO_BENCH_SCROLL_SMOKE_STEP_DELAY_MS"] = [string]$ScrollSmokeStepDelayMs
    }
}

$previousEnv = @{}
$process = $null
$launchTimestamp = [DateTime]::UtcNow
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$stopResult = $null
$exitedBeforeDeadline = $false

try {
    foreach ($item in $profileEnv.GetEnumerator()) {
        Set-TemporaryEnv -Previous $previousEnv -Name ([string]$item.Key) -Value ([string]$item.Value)
    }

    $startArgs = @{
        FilePath = $resolvedLaunchPath
        WorkingDirectory = $resolvedWorkingDirectory
        RedirectStandardOutput = $stdoutPath
        RedirectStandardError = $stderrPath
        PassThru = $true
    }
    if ($LaunchArgument.Count -gt 0) {
        $startArgs.ArgumentList = $LaunchArgument
    }

    $process = Start-Process @startArgs
    $deadline = [DateTime]::UtcNow.AddSeconds($DurationSeconds)
    while ([DateTime]::UtcNow -lt $deadline) {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            $exitedBeforeDeadline = $true
            break
        }
    }
    $stopResult = Stop-ProfileProcess -Process $process
    $watch.Stop()
} finally {
    Restore-Env -Previous $previousEnv
}

$processExitCode = $null
$processExited = $false
if ($null -ne $process) {
    try {
        $process.Refresh()
        $processExited = [bool]$process.HasExited
        if ($processExited) {
            $processExitCode = $process.ExitCode
        }
    } catch {
        $processExited = $true
    }
}

$logCheckExitCode = $null
if (Test-Path $logPath) {
    $checker = $ProfileLogCheckPath
    if ([string]::IsNullOrWhiteSpace($checker)) {
        $checkerCandidates = @(
            (Join-Path $repoRoot "target\release\profile-log-check.exe"),
            (Join-Path $repoRoot "target\debug\profile-log-check.exe"),
            (Join-Path $repoRoot "target\profile-log-check\release\profile-log-check.exe"),
            (Join-Path $repoRoot "target\profile-log-check\release\profile-log-check")
        )
        foreach ($candidate in $checkerCandidates) {
            if (Test-Path $candidate) {
                $checker = $candidate
                break
            }
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($checker) -and (Test-Path $checker)) {
        & $checker $logPath > $profileLogCheckRawPath
        $logCheckExitCode = $LASTEXITCODE
    }
}

$runManifest = [ordered]@{
    schema_version = 1
    app = "echo"
    scenario = $Scenario
    profile_name = $safeProfileName
    launched_at_utc = $launchTimestamp.ToString("o")
    duration_seconds = $DurationSeconds
    observed_duration_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
    launch_path = $resolvedLaunchPath
    launch_argument = $LaunchArgument
    launch_working_directory = $resolvedWorkingDirectory
    process_id = if ($null -ne $process) { $process.Id } else { $null }
    process_exited = $processExited
    process_exit_code = $processExitCode
    exited_before_deadline = $exitedBeforeDeadline
    stop = $stopResult
    environment = $profileEnv
    artifacts = [ordered]@{
        stdout = $stdoutPath
        stderr = $stderrPath
        log = $logPath
        profile_log_check = if (Test-Path $profileLogCheckRawPath) { $profileLogCheckRawPath } else { $null }
    }
    profile_log_check_exit_code = $logCheckExitCode
    decision = "Echo-only profile run; not compared"
}
$runManifest | ConvertTo-Json -Depth 10 | Set-Content -Path $runManifestPath -Encoding utf8

$summaryPath = Join-Path $scenarioDir "summary.json"
if (Test-Path $summaryPath) {
    $summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $summary = [pscustomobject]@{
        schema_version = 1
        scenario = $Scenario
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $summary -Name "raw_artifacts"
$metrics = Ensure-ObjectProperty -Object $summary -Name "metrics"

Set-JsonProperty -Object $rawArtifacts -Name "echo_profile_run" -Value $runManifestPath
Set-JsonProperty -Object $rawArtifacts -Name "echo_profile_stdout" -Value $stdoutPath
Set-JsonProperty -Object $rawArtifacts -Name "echo_profile_stderr" -Value $stderrPath
Set-JsonProperty -Object $rawArtifacts -Name "echo_profile_log" -Value $logPath
if (Test-Path $profileLogCheckRawPath) {
    Set-JsonProperty -Object $rawArtifacts -Name "profile_log_check" -Value $profileLogCheckRawPath
}

Set-JsonProperty -Object $metrics -Name "echo_profile" -Value ([pscustomobject]$runManifest)
$profileValid = (-not $exitedBeforeDeadline) -and
    (Test-Path $logPath) -and
    ((Get-Item $logPath).Length -gt 0) -and
    (($null -eq $logCheckExitCode) -or ($logCheckExitCode -eq 0))
Set-JsonProperty -Object $summary -Name "measurements_recorded" -Value $profileValid
Set-JsonProperty -Object $summary -Name "status" -Value $(if ($profileValid) { "echo-profiled" } else { "echo-profile-invalid" })
Set-JsonProperty -Object $summary -Name "decision" -Value $(if ($profileValid) { "Echo-only profile run; not compared" } else { "Invalid Echo profile run; child exited early, produced no runtime log, or failed log validation" })

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Echo profile run"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Run manifest: $runManifestPath"
    Add-Content -Path $notesPath -Value "- Stdout: $stdoutPath"
    Add-Content -Path $notesPath -Value "- Stderr: $stderrPath"
    Add-Content -Path $notesPath -Value "- Runtime log: $logPath"
    Add-Content -Path $notesPath -Value "- Duration seconds: $DurationSeconds"
    Add-Content -Path $notesPath -Value "- Stop mode: $StopMode"
    Add-Content -Path $notesPath -Value "- Decision: Echo-only profile run; not compared"
}

Write-Output $runManifestPath

if (-not $profileValid) {
    throw "Echo profile run was invalid: the child exited before the deadline, produced no runtime log, or failed log validation. See $runManifestPath"
}
