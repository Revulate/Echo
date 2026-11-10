[CmdletBinding()]
param(
    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Scenario,

    [string]$RunDir = "",
    [string]$OutputRoot = "",

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$LaunchPath,

    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",

    [ValidateSet("process-start", "main-window", "input-idle", "app-log")]
    [string]$ReadyMode = "main-window",

    [ValidateRange(1, 300)]
    [int]$ReadyTimeoutSeconds = 30,

    [ValidateRange(1, 86400)]
    [int]$SampleDurationSeconds = 10,

    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 1000,

    [switch]$StopLaunchedProcess,

    [string]$EchoPath = "",
    [string]$EchoBuildType = "",
    [string]$CacheState = "",
    [string]$NetworkState = "",
    [string]$FixtureName = "",
    [string]$Notes = "",

    # Optional application-owned log used for semantic startup readiness.
    # Echo emits structured tracing fields containing:
    # startup_marker marker=<name> elapsed_ms=<n>.
    [string]$AppLogPath = "",
    [ValidateNotNullOrEmpty()]
    [string]$StartupMilestone = "ready",
    [string]$ObservationId = ""
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

    throw "LaunchPath '$Path' was not found as a file or executable command."
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

    $args = @("-Scenario", $Scenario)
    if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) { $args += @("-OutputRoot", $OutputRoot) }
    if (-not [string]::IsNullOrWhiteSpace($EchoPath)) { $args += @("-EchoPath", $EchoPath) }
    if (-not [string]::IsNullOrWhiteSpace($EchoBuildType)) { $args += @("-EchoBuildType", $EchoBuildType) }
    if (-not [string]::IsNullOrWhiteSpace($CacheState)) { $args += @("-CacheState", $CacheState) }
    if (-not [string]::IsNullOrWhiteSpace($NetworkState)) { $args += @("-NetworkState", $NetworkState) }
    if (-not [string]::IsNullOrWhiteSpace($FixtureName)) { $args += @("-FixtureName", $FixtureName) }
    if (-not [string]::IsNullOrWhiteSpace($Notes)) { $args += @("-Notes", $Notes) }

    $output = & powershell -NoProfile -ExecutionPolicy Bypass -File $initializer @args
    if ($LASTEXITCODE -ne 0) {
        throw "Benchmark run initializer failed."
    }

    $path = (($output | Select-Object -Last 1) | Out-String).Trim()
    if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path $path)) {
        throw "Benchmark run initializer did not return an existing path."
    }

    return (Resolve-Path $path).Path
}

function Ensure-Summary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (Test-Path $Path) {
        return Get-Content -Raw $Path | ConvertFrom-Json
    }

    return [pscustomobject]@{
        schema_version = 1
        run_id = Split-Path -Leaf (Split-Path -Parent $Path)
        scenario = $Scenario
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

function Set-JsonProperty {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Object,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [object]$Value
    )

    $property = $Object.PSObject.Properties.Match($Name) | Select-Object -First 1
    if ($null -ne $property) {
        $property.Value = $Value
    } else {
        $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value
    }
}

function Ensure-SummaryObjectProperty {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Summary,

        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $property = $Summary.PSObject.Properties.Match($Name) | Select-Object -First 1
    if ($null -eq $property -or $null -eq $property.Value) {
        Set-JsonProperty -Object $Summary -Name $Name -Value ([pscustomobject]@{})
    }
}

function Wait-ForStartupReady {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process,

        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Stopwatch]$Watch
    )

    $windowObserved = $false
    $windowMs = $null
    $inputIdleObserved = $false
    $inputIdleMs = $null
    $readyObserved = $false
    $readyMs = $null
    $timedOut = $false
    $exited = $false
    $deadlineMs = $ReadyTimeoutSeconds * 1000

    if ($ReadyMode -eq "app-log" -and [string]::IsNullOrWhiteSpace($AppLogPath)) {
        throw "-AppLogPath is required when -ReadyMode app-log is selected."
    }

    if ($ReadyMode -eq "process-start") {
        return [ordered]@{
            ready_observed = $true
            ready_elapsed_ms = [long]$Watch.Elapsed.TotalMilliseconds
            window_observed = $false
            window_elapsed_ms = $null
            input_idle_observed = $false
            input_idle_elapsed_ms = $null
            timed_out = $false
            process_exited = $false
        }
    }

    while ($Watch.Elapsed.TotalMilliseconds -lt $deadlineMs) {
        try {
            $Process.Refresh()
            if ($Process.HasExited) {
                $exited = $true
                break
            }
        } catch {
            $exited = $true
            break
        }

        if ($ReadyMode -eq "app-log" -and (Test-Path $AppLogPath)) {
            $logText = Get-Content -Raw -LiteralPath $AppLogPath -ErrorAction SilentlyContinue
            $markerLine = @($logText -split "`r?`n" | Where-Object {
                $_ -match "startup_marker" -and $_ -match ('marker="?' + [regex]::Escape($StartupMilestone) + '"?(?:\s|$)')
            } | Select-Object -First 1)
            $match = if ($markerLine.Count -gt 0) { [regex]::Match($markerLine[0], "elapsed_ms=(?<elapsed>[0-9]+(?:\.[0-9]+)?)") } else { $null }
            if ($null -ne $match -and $match.Success) {
                $readyObserved = $true
                $readyMs = [long]$Watch.Elapsed.TotalMilliseconds
                break
            }
        }

        if (-not $windowObserved -and $Process.MainWindowHandle -ne [IntPtr]::Zero) {
            $windowObserved = $true
            $windowMs = [long]$Watch.Elapsed.TotalMilliseconds
            if ($ReadyMode -eq "main-window") {
                $readyObserved = $true
                $readyMs = $windowMs
                break
            }
        }

        if ($ReadyMode -eq "input-idle") {
            try {
                if ($Process.WaitForInputIdle(100)) {
                    $inputIdleObserved = $true
                    $inputIdleMs = [long]$Watch.Elapsed.TotalMilliseconds
                    $readyObserved = $true
                    $readyMs = $inputIdleMs
                    break
                }
            } catch {
                if ($windowObserved) {
                    $readyObserved = $true
                    $readyMs = $windowMs
                    break
                }
            }
        }

        Start-Sleep -Milliseconds 50
    }

    if (-not $readyObserved -and -not $exited) {
        $timedOut = $true
    }

    return [ordered]@{
        ready_observed = $readyObserved
        ready_elapsed_ms = $readyMs
        window_observed = $windowObserved
        window_elapsed_ms = $windowMs
        input_idle_observed = $inputIdleObserved
        input_idle_elapsed_ms = $inputIdleMs
        timed_out = $timedOut
        process_exited = $exited
    }
}

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-RunDirectory -RepoRoot $repoRoot
$resolvedLaunchPath = Resolve-ExecutablePath -Path $LaunchPath

$startArgs = @{
    FilePath = $resolvedLaunchPath
    PassThru = $true
}
if ($LaunchArgument.Count -gt 0) { $startArgs.ArgumentList = $LaunchArgument }
if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
    $startArgs.WorkingDirectory = (New-Item -ItemType Directory -Force -Path $LaunchWorkingDirectory).FullName
}

$launchTimestamp = [DateTime]::UtcNow
$startupWatch = [System.Diagnostics.Stopwatch]::StartNew()
$priorLogFile = $env:ECHO_LOG_FILE
$priorLogFilePath = $env:ECHO_LOG_FILE_PATH
$priorLogUi = $env:ECHO_LOG_UI
try {
    if ($App -eq "echo" -and $ReadyMode -eq "app-log") {
        $logDirectory = Split-Path -Parent $AppLogPath
        if (-not [string]::IsNullOrWhiteSpace($logDirectory)) {
            New-Item -ItemType Directory -Force -Path $logDirectory | Out-Null
        }
        Remove-Item -LiteralPath $AppLogPath -Force -ErrorAction SilentlyContinue
        $env:ECHO_LOG_FILE = "1"
        $env:ECHO_LOG_FILE_PATH = $AppLogPath
        $env:ECHO_LOG_UI = "1"
    }
    $process = Start-Process @startArgs
} finally {
    $env:ECHO_LOG_FILE = $priorLogFile
    $env:ECHO_LOG_FILE_PATH = $priorLogFilePath
    $env:ECHO_LOG_UI = $priorLogUi
}
$processCreatedElapsedMs = [long]$startupWatch.Elapsed.TotalMilliseconds
$ready = Wait-ForStartupReady -Process $process -Watch $startupWatch
$startupWatch.Stop()

$appOwnedMilestones = [ordered]@{}
if ($App -eq "echo" -and -not [string]::IsNullOrWhiteSpace($AppLogPath) -and (Test-Path $AppLogPath)) {
    $logText = Get-Content -Raw -LiteralPath $AppLogPath -ErrorAction SilentlyContinue
    foreach ($name in @(
        "process_created", "prewarm_joined", "prewarm_detached", "gpui_http_client_ready", "user_colors_loaded",
        "settings_loaded", "backend_initialized", "caches_initialized", "theme_initialized",
        "main_window_opening", "main_window_created", "first_root_render", "layout_restored",
        "first_chat_view_usable",
        "first_text_paint", "first_visible_emote_paint", "visible_emotes_complete",
        "startup_network_fallback_complete"
    )) {
        $line = @($logText -split "`r?`n" | Where-Object {
            $_ -match "startup_marker" -and $_ -match ('marker="?' + [regex]::Escape($name) + '"?(?:\s|$)')
        } | Select-Object -First 1)
        $match = if ($line.Count -gt 0) { [regex]::Match($line[0], "elapsed_ms=(?<elapsed>[0-9]+(?:\.[0-9]+)?)") } else { $null }
        $appOwnedMilestones[$name] = if ($null -ne $match -and $match.Success) { [double]$match.Groups["elapsed"].Value } else { $null }
    }
}

    $suffix = if ([string]::IsNullOrWhiteSpace($ObservationId)) { "" } else { ".$ObservationId" }
    $startupRawPath = Join-Path $scenarioDir ("{0}.startup{1}.raw.txt" -f $App, $suffix)
$startupRecord = [ordered]@{
    schema_version = 1
    app = $App
    scenario = $Scenario
    launch_timestamp_utc = $launchTimestamp.ToString("o")
    launch_path = $resolvedLaunchPath
    launch_argument = $LaunchArgument
    process_id = $process.Id
    process_name = $process.ProcessName
    ready_mode = $ReadyMode
    ready_timeout_seconds = $ReadyTimeoutSeconds
    process_created_elapsed_ms = $processCreatedElapsedMs
    ready_observed = [bool]$ready.ready_observed
    ready_elapsed_ms = $ready.ready_elapsed_ms
    window_observed = [bool]$ready.window_observed
    window_elapsed_ms = $ready.window_elapsed_ms
    input_idle_observed = [bool]$ready.input_idle_observed
    input_idle_elapsed_ms = $ready.input_idle_elapsed_ms
    timed_out = [bool]$ready.timed_out
    process_exited_before_ready = [bool]$ready.process_exited
    observation_id = $ObservationId
    app_log_path = $AppLogPath
    startup_milestone = $StartupMilestone
    app_owned_milestones_ms = $appOwnedMilestones
}

$startupRecord | ConvertTo-Json -Depth 8 | Set-Content -Path $startupRawPath -Encoding utf8

$sampler = Join-Path $repoRoot "scripts\bench\sample-process.ps1"
if (-not (Test-Path $sampler)) {
    throw "Process sampler not found: $sampler"
}

$sampleArgs = @(
    "-App", $App,
    "-RunDir", $scenarioDir,
    "-TargetProcessId", "$($process.Id)",
    "-DurationSeconds", "$SampleDurationSeconds",
    "-IntervalMilliseconds", "$IntervalMilliseconds"
)
$sampleRawPath = (($(& powershell -NoProfile -ExecutionPolicy Bypass -File $sampler @sampleArgs) | Select-Object -Last 1) | Out-String).Trim()
if ($LASTEXITCODE -ne 0 -or [string]::IsNullOrWhiteSpace($sampleRawPath) -or -not (Test-Path $sampleRawPath)) {
    throw "Process sampler failed after startup measurement."
}

if ($StopLaunchedProcess) {
    try {
        $process.Refresh()
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force
        }
    } catch {
        # The process may have exited naturally after sampling.
    }
}

$summaryPath = Join-Path $scenarioDir "summary.json"
$summary = Ensure-Summary -Path $summaryPath
Ensure-SummaryObjectProperty -Summary $summary -Name "raw_artifacts"
Ensure-SummaryObjectProperty -Summary $summary -Name "metrics"

$startupKey = if ([string]::IsNullOrWhiteSpace($ObservationId)) { "{0}_startup" -f $App } else { "{0}_startup_{1}" -f $App, $ObservationId }
$startupMetric = [ordered]@{
    app = $App
    raw_artifact = $startupRawPath
    sample_raw_artifact = $sampleRawPath
    launch_path = $resolvedLaunchPath
    launch_argument = $LaunchArgument
    ready_mode = $ReadyMode
    ready_timeout_seconds = $ReadyTimeoutSeconds
    process_created_elapsed_ms = $processCreatedElapsedMs
    ready_observed = [bool]$ready.ready_observed
    ready_elapsed_ms = $ready.ready_elapsed_ms
    window_observed = [bool]$ready.window_observed
    window_elapsed_ms = $ready.window_elapsed_ms
    input_idle_observed = [bool]$ready.input_idle_observed
    input_idle_elapsed_ms = $ready.input_idle_elapsed_ms
    timed_out = [bool]$ready.timed_out
    process_exited_before_ready = [bool]$ready.process_exited
    observation_id = $ObservationId
    app_log_path = $AppLogPath
    startup_milestone = $StartupMilestone
    app_owned_milestones_ms = $appOwnedMilestones
}

Set-JsonProperty -Object $summary.raw_artifacts -Name $startupKey -Value $startupRawPath
Set-JsonProperty -Object $summary.metrics -Name $startupKey -Value ([pscustomobject]$startupMetric)
Set-JsonProperty -Object $summary -Name "measurements_recorded" -Value $true
Set-JsonProperty -Object $summary -Name "status" -Value "startup-sampled"
Set-JsonProperty -Object $summary -Name "decision" -Value "not compared"

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Startup sample: $App"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Startup raw artifact: $startupRawPath"
    Add-Content -Path $notesPath -Value "- Process raw artifact: $sampleRawPath"
    Add-Content -Path $notesPath -Value "- Ready mode: $ReadyMode"
    Add-Content -Path $notesPath -Value "- Ready observed: $($ready.ready_observed)"
    Add-Content -Path $notesPath -Value "- Decision: not compared"
}

Write-Output $startupRawPath
