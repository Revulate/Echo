[CmdletBinding()]
param(
    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$MetricName = "",
    [string]$RawFileName = "",

    [string]$Scenario = "",
    [string]$RunDir = "",
    [string]$OutputRoot = "",

    [int]$TargetProcessId = 0,
    [string]$ProcessName = "",
    [string]$LaunchPath = "",
    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",
    [string[]]$LaunchEnvironment = @(),

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 30,

    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 1000,

    [ValidateRange(0, 3600)]
    [int]$StartupWaitSeconds = 0,

    [switch]$StopLaunchedProcess,

    [string]$EchoPath = "",
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

    if ([string]::IsNullOrWhiteSpace($Scenario)) {
        throw "Scenario is required when RunDir is not provided."
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

function Get-TargetProcess {
    $selectorCount = 0
    if ($TargetProcessId -gt 0) { $selectorCount += 1 }
    if (-not [string]::IsNullOrWhiteSpace($ProcessName)) { $selectorCount += 1 }
    if (-not [string]::IsNullOrWhiteSpace($LaunchPath)) { $selectorCount += 1 }

    if ($selectorCount -ne 1) {
        throw "Provide exactly one target selector: TargetProcessId, ProcessName, or LaunchPath."
    }
    if ($LaunchEnvironment.Count -gt 0 -and [string]::IsNullOrWhiteSpace($LaunchPath)) {
        throw "LaunchEnvironment can only be used with LaunchPath."
    }

    if ($TargetProcessId -gt 0) {
        return [ordered]@{
            process = Get-Process -Id $TargetProcessId -ErrorAction Stop
            launched = $false
            launch_path = ""
            launch_argument = @()
            launch_environment_keys = @()
        }
    }

    if (-not [string]::IsNullOrWhiteSpace($ProcessName)) {
        $normalizedName = [System.IO.Path]::GetFileNameWithoutExtension($ProcessName)
        $matches = @(Get-Process -Name $normalizedName -ErrorAction Stop)
        if ($matches.Count -ne 1) {
            throw "ProcessName '$ProcessName' matched $($matches.Count) processes. Use TargetProcessId for an exact target."
        }

        return [ordered]@{
            process = $matches[0]
            launched = $false
            launch_path = ""
            launch_argument = @()
            launch_environment_keys = @()
        }
    }

    $resolvedLaunchPath = Resolve-ExecutablePath -Path $LaunchPath
    $environment = @{}
    foreach ($entry in $LaunchEnvironment) {
        $parts = $entry -split "=", 2
        if ($parts.Count -ne 2 -or [string]::IsNullOrWhiteSpace($parts[0])) {
            throw "LaunchEnvironment entries must use KEY=VALUE syntax: '$entry'"
        }
        $environment[$parts[0]] = $parts[1]
    }
    $startArgs = @{
        FilePath = $resolvedLaunchPath
        PassThru = $true
    }
    if ($LaunchArgument.Count -gt 0) { $startArgs.ArgumentList = $LaunchArgument }
    if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
        $startArgs.WorkingDirectory = (New-Item -ItemType Directory -Force -Path $LaunchWorkingDirectory).FullName
    }

    $previousEnvironment = @{}
    try {
        foreach ($entry in $environment.GetEnumerator()) {
            $previousEnvironment[$entry.Key] = [Environment]::GetEnvironmentVariable($entry.Key, "Process")
            [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
        }
        $process = Start-Process @startArgs
    } finally {
        foreach ($entry in $previousEnvironment.GetEnumerator()) {
            [Environment]::SetEnvironmentVariable($entry.Key, $entry.Value, "Process")
        }
    }
    return [ordered]@{
        process = $process
        launched = $true
        launch_path = $resolvedLaunchPath
        launch_argument = $LaunchArgument
        launch_environment_keys = @($environment.Keys | Sort-Object)
    }
}

function New-NullStats {
    return [ordered]@{
        min = $null
        avg = $null
        max = $null
    }
}

function Get-SeriesStats {
    param(
        [object[]]$Values,

        [ValidateRange(0, 9)]
        [int]$DecimalPlaces = 0
    )

    $clean = @($Values | Where-Object { $null -ne $_ -and $_ -ne "" } | ForEach-Object { [double]$_ })
    if ($clean.Count -eq 0) {
        return New-NullStats
    }

    $measure = $clean | Measure-Object -Minimum -Maximum -Average
    return [ordered]@{
        min = if ($DecimalPlaces -eq 0) { [long][Math]::Round($measure.Minimum) } else { [Math]::Round([double]$measure.Minimum, $DecimalPlaces) }
        avg = if ($DecimalPlaces -eq 0) { [long][Math]::Round($measure.Average) } else { [Math]::Round([double]$measure.Average, $DecimalPlaces) }
        max = if ($DecimalPlaces -eq 0) { [long][Math]::Round($measure.Maximum) } else { [Math]::Round([double]$measure.Maximum, $DecimalPlaces) }
    }
}

function Ensure-Summary {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [string]$ScenarioName
    )

    if (Test-Path $Path) {
        return Get-Content -Raw $Path | ConvertFrom-Json
    }

    return [pscustomobject]@{
        schema_version = 1
        run_id = Split-Path -Leaf (Split-Path -Parent $Path)
        scenario = $ScenarioName
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

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-RunDirectory -RepoRoot $repoRoot
$scenarioName = if (-not [string]::IsNullOrWhiteSpace($Scenario)) { $Scenario } else { Split-Path -Leaf $scenarioDir }
$resolvedMetricName = if ([string]::IsNullOrWhiteSpace($MetricName)) { $App } else { $MetricName }
if ($resolvedMetricName -notmatch "^[A-Za-z0-9._-]+$") {
    throw "MetricName may contain only letters, numbers, dot, underscore, and hyphen."
}
if (-not [string]::IsNullOrWhiteSpace($RawFileName) -and [System.IO.Path]::GetFileName($RawFileName) -ne $RawFileName) {
    throw "RawFileName must be a file name without directory components."
}
$target = Get-TargetProcess
$process = $target.process

if ($StartupWaitSeconds -gt 0) {
    Start-Sleep -Seconds $StartupWaitSeconds
}

$processorCount = [Environment]::ProcessorCount
$samples = [System.Collections.Generic.List[object]]::new()
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$previousElapsedMs = $null
$previousCpuMs = $null
$sampleIndex = 0

while ($watch.Elapsed.TotalSeconds -lt $DurationSeconds) {
    $timestamp = [DateTime]::UtcNow
    $elapsedMs = [long]$watch.Elapsed.TotalMilliseconds
    $hasExited = $false

    try {
        $process.Refresh()
        $hasExited = $process.HasExited
    } catch {
        $hasExited = $true
    }

    $workingSet = $null
    $privateBytes = $null
    $pagedBytes = $null
    $virtualBytes = $null
    $handleCount = $null
    $threadCount = $null
    $cpuMs = $null

    if (-not $hasExited) {
        $workingSet = $process.WorkingSet64
        $privateBytes = $process.PrivateMemorySize64
        $pagedBytes = $process.PagedMemorySize64
        $virtualBytes = $process.VirtualMemorySize64
        $handleCount = $process.HandleCount
        $threadCount = $process.Threads.Count
        $cpuMs = [double]$process.TotalProcessorTime.TotalMilliseconds
    }

    $cpuAll = $null
    $cpuSingle = $null
    if ($null -ne $previousElapsedMs -and $null -ne $previousCpuMs -and $null -ne $cpuMs) {
        $elapsedDelta = [double]($elapsedMs - $previousElapsedMs)
        $cpuDelta = [double]($cpuMs - $previousCpuMs)
        if ($elapsedDelta -gt 0) {
            $cpuSingle = [Math]::Round(($cpuDelta / $elapsedDelta) * 100.0, 3)
            $cpuAll = [Math]::Round($cpuSingle / [Math]::Max(1, $processorCount), 3)
        }
    }

    $samples.Add([pscustomobject]@{
        sample_index = $sampleIndex
        timestamp_utc = $timestamp.ToString("o")
        elapsed_ms = $elapsedMs
        process_id = $process.Id
        process_name = $process.ProcessName
        has_exited = $hasExited
        working_set_bytes = $workingSet
        private_memory_bytes = $privateBytes
        paged_memory_bytes = $pagedBytes
        virtual_memory_bytes = $virtualBytes
        handle_count = $handleCount
        thread_count = $threadCount
        total_cpu_ms = $cpuMs
        cpu_percent_all_processors = $cpuAll
        cpu_percent_single_core = $cpuSingle
    }) | Out-Null

    if ($hasExited) {
        break
    }

    $previousElapsedMs = $elapsedMs
    $previousCpuMs = $cpuMs
    $sampleIndex += 1
    Start-Sleep -Milliseconds $IntervalMilliseconds
}

$watch.Stop()

if ($target.launched -and $StopLaunchedProcess) {
    try {
        $process.Refresh()
        if (-not $process.HasExited) {
            Stop-Process -Id $process.Id -Force
        }
    } catch {
        # The launched process may have exited between the last sample and cleanup.
    }
}

$resolvedRawFileName = if ([string]::IsNullOrWhiteSpace($RawFileName)) {
    "{0}.raw.txt" -f $resolvedMetricName
} else {
    $RawFileName
}
$rawPath = Join-Path $scenarioDir $resolvedRawFileName
$samples | Export-Csv -Path $rawPath -NoTypeInformation -Encoding utf8

$summaryPath = Join-Path $scenarioDir "summary.json"
$summary = Ensure-Summary -Path $summaryPath -ScenarioName $scenarioName

$rawArtifactsProperty = $summary.PSObject.Properties.Match("raw_artifacts") | Select-Object -First 1
if ($null -eq $rawArtifactsProperty -or $null -eq $rawArtifactsProperty.Value) {
    Set-JsonProperty -Object $summary -Name "raw_artifacts" -Value ([pscustomobject]@{})
}
$metricsProperty = $summary.PSObject.Properties.Match("metrics") | Select-Object -First 1
if ($null -eq $metricsProperty -or $null -eq $metricsProperty.Value) {
    Set-JsonProperty -Object $summary -Name "metrics" -Value ([pscustomobject]@{})
}

$metric = [ordered]@{
    app = $App
    metric_name = $resolvedMetricName
    raw_artifact = $rawPath
    sample_count = $samples.Count
    requested_duration_seconds = $DurationSeconds
    observed_duration_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
    interval_milliseconds = $IntervalMilliseconds
    processor_count = $processorCount
    launched_by_sampler = [bool]$target.launched
    launch_path = $target.launch_path
    launch_argument = $target.launch_argument
    launch_environment_keys = @($target.launch_environment_keys)
    process_id = $process.Id
    process_name = $process.ProcessName
    process_exited = [bool]($samples.Count -gt 0 -and $samples[$samples.Count - 1].has_exited)
    working_set_bytes = Get-SeriesStats -Values @($samples | ForEach-Object { $_.working_set_bytes })
    private_memory_bytes = Get-SeriesStats -Values @($samples | ForEach-Object { $_.private_memory_bytes })
    paged_memory_bytes = Get-SeriesStats -Values @($samples | ForEach-Object { $_.paged_memory_bytes })
    virtual_memory_bytes = Get-SeriesStats -Values @($samples | ForEach-Object { $_.virtual_memory_bytes })
    cpu_percent_all_processors = Get-SeriesStats -Values @($samples | ForEach-Object { $_.cpu_percent_all_processors }) -DecimalPlaces 3
    cpu_percent_single_core = Get-SeriesStats -Values @($samples | ForEach-Object { $_.cpu_percent_single_core }) -DecimalPlaces 3
}

Set-JsonProperty -Object $summary.raw_artifacts -Name $resolvedMetricName -Value $rawPath
Set-JsonProperty -Object $summary.metrics -Name $resolvedMetricName -Value ([pscustomobject]$metric)
Set-JsonProperty -Object $summary -Name "measurements_recorded" -Value $true
Set-JsonProperty -Object $summary -Name "status" -Value "sampled"
Set-JsonProperty -Object $summary -Name "decision" -Value "not compared"

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Process sample: $resolvedMetricName"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Raw artifact: $rawPath"
    Add-Content -Path $notesPath -Value "- Samples: $($samples.Count)"
    Add-Content -Path $notesPath -Value "- Decision: not compared"
}

Write-Output $rawPath
