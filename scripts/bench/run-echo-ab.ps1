[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path $_ -PathType Leaf })]
    [string]$BaselinePath,

    [Parameter(Mandatory = $true)]
    [ValidateScript({ Test-Path $_ -PathType Leaf })]
    [string]$CandidatePath,

    [string]$BaselineLabel = "baseline",
    [string]$CandidateLabel = "candidate",
    [ValidatePattern("^[AB]+$")]
    [string]$Sequence = "ABBA",
    [ValidateRange(1, 20)]
    [int]$RepeatCount = 2,
    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 20,
    [ValidateRange(100, 60000)]
    [int]$IntervalMilliseconds = 500,
    [ValidateRange(0.1, 1.0)]
    [double]$MinimumSampleFraction = 0.75,
    [ValidateRange(0, 3600)]
    [int]$StartupWaitSeconds = 5,
    [string[]]$LaunchArgument = @(),
    [string[]]$LaunchEnvironment = @(),
    [string]$LaunchWorkingDirectory = "",
    [string]$EchoChatReplay = "",
    [ValidateRange(0.0, 10000.0)]
    [double]$EchoChatReplaySpeedup = 0.0,
    [string]$Scenario = "Echo release variant A/B",
    [string]$OutputRoot = "",
    [string]$CacheState = "preserved",
    [string]$NetworkState = "local-or-user-configured",
    [string]$FixtureName = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function ConvertTo-Slug {
    param([Parameter(Mandatory = $true)][string]$Value)
    $slug = ($Value.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-").Trim("-")
    if ([string]::IsNullOrWhiteSpace($slug)) { return "variant" }
    return $slug
}

function Get-JsonProperty {
    param([Parameter(Mandatory = $true)][object]$Object, [Parameter(Mandatory = $true)][string]$Name)
    if ($Object -is [System.Collections.IDictionary]) {
        if ($Object.Contains($Name)) { return $Object[$Name] }
        return $null
    }
    $property = $Object.PSObject.Properties.Match($Name) | Select-Object -First 1
    if ($null -eq $property) { return $null }
    return $property.Value
}

function Set-JsonProperty {
    param([Parameter(Mandatory = $true)][psobject]$Object, [Parameter(Mandatory = $true)][string]$Name, [object]$Value)
    $property = $Object.PSObject.Properties.Match($Name) | Select-Object -First 1
    if ($null -ne $property) { $property.Value = $Value } else { $Object | Add-Member -NotePropertyName $Name -NotePropertyValue $Value }
}

function Get-Median {
    param([object[]]$Values)
    $numbers = @($Values | Where-Object { $null -ne $_ -and $_ -ne "" } | ForEach-Object { [double]$_ } | Sort-Object)
    if ($numbers.Count -eq 0) { return $null }
    $middle = [Math]::Floor($numbers.Count / 2)
    if ($numbers.Count % 2 -eq 1) { return [Math]::Round($numbers[$middle], 3) }
    return [Math]::Round(($numbers[$middle - 1] + $numbers[$middle]) / 2.0, 3)
}

function Get-NestedValue {
    param([Parameter(Mandatory = $true)][object]$Object, [Parameter(Mandatory = $true)][string[]]$Path)
    $value = $Object
    foreach ($part in $Path) {
        if ($null -eq $value) { return $null }
        $value = Get-JsonProperty -Object $value -Name $part
    }
    return $value
}

function New-AggregatedMetric {
    param([Parameter(Mandatory = $true)][string]$Label, [Parameter(Mandatory = $true)][object[]]$Runs)
    $metric = [ordered]@{ label = $Label; run_count = $Runs.Count; aggregation = "median_across_runs" }
    foreach ($group in @("working_set_bytes", "private_memory_bytes", "paged_memory_bytes", "virtual_memory_bytes", "cpu_percent_all_processors", "cpu_percent_single_core")) {
        $stats = [ordered]@{}
        foreach ($field in @("min", "avg", "max")) {
            $stats[$field] = Get-Median -Values @($Runs | ForEach-Object { Get-NestedValue -Object $_.metric -Path @($group, $field) })
        }
        $metric[$group] = $stats
    }
    return [pscustomobject]$metric
}

function New-ComparisonRow {
    param(
        [Parameter(Mandatory = $true)][string]$Name,
        [Parameter(Mandatory = $true)][string[]]$Path,
        [Parameter(Mandatory = $true)][psobject]$Baseline,
        [Parameter(Mandatory = $true)][psobject]$Candidate
    )
    $baselineValue = Get-NestedValue -Object $Baseline -Path $Path
    $candidateValue = Get-NestedValue -Object $Candidate -Path $Path
    $delta = if ($null -ne $baselineValue -and $null -ne $candidateValue) { [double]$candidateValue - [double]$baselineValue } else { $null }
    $deltaPercent = if ($null -ne $delta -and [double]$baselineValue -ne 0.0) { [Math]::Round(($delta / [double]$baselineValue) * 100.0, 3) } else { $null }
    return [pscustomobject][ordered]@{
        name = $Name
        path = $Path -join "."
        baseline = $baselineValue
        candidate = $candidateValue
        delta_candidate_minus_baseline = if ($null -ne $delta) { [Math]::Round($delta, 3) } else { $null }
        delta_percent = $deltaPercent
        lower_is_better = $true
        candidate_lower = if ($null -ne $delta) { $delta -lt 0.0 } else { $null }
        missing = @($(if ($null -eq $baselineValue) { "baseline" }; if ($null -eq $candidateValue) { "candidate" }))
    }
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
$sampler = Join-Path $scriptRoot "sample-process.ps1"
$initializer = Join-Path $scriptRoot "new-benchmark-run.ps1"
$baseline = (Resolve-Path $BaselinePath).Path
$candidate = (Resolve-Path $CandidatePath).Path
$baselineSlug = ConvertTo-Slug $BaselineLabel
$candidateSlug = ConvertTo-Slug $CandidateLabel
if ($baselineSlug -eq $candidateSlug) { throw "BaselineLabel and CandidateLabel must produce different slugs." }
if (-not $Sequence.Contains("A") -or -not $Sequence.Contains("B")) {
    throw "Sequence must contain at least one baseline run (A) and one candidate run (B)."
}
foreach ($entry in $LaunchEnvironment) {
    $parts = $entry -split "=", 2
    if ($parts.Count -ne 2 -or [string]::IsNullOrWhiteSpace($parts[0])) {
        throw "LaunchEnvironment entries must use KEY=VALUE syntax: '$entry'"
    }
}

$initArgs = @{
    Scenario = $Scenario
    EchoPath = $candidate
    EchoBuildType = "Echo A/B candidate"
    CacheState = $CacheState
    NetworkState = $NetworkState
    FixtureName = $FixtureName
    Notes = "Balanced same-machine Echo variant comparison.`n$Notes"
}
if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) { $initArgs.OutputRoot = $OutputRoot }
$runDir = ((& $initializer @initArgs | Select-Object -Last 1) | Out-String).Trim()
if ([string]::IsNullOrWhiteSpace($runDir) -or -not (Test-Path $runDir)) { throw "Benchmark initializer did not return a run directory." }

$resolvedReplay = ""
if (-not [string]::IsNullOrWhiteSpace($EchoChatReplay)) { $resolvedReplay = (Resolve-Path $EchoChatReplay).Path }
$expandedSequence = (($Sequence * $RepeatCount) -join "")
$expectedSamplesPerRun = [Math]::Max(1, [Math]::Floor(($DurationSeconds * 1000.0) / $IntervalMilliseconds))
$minimumSamplesPerRun = [Math]::Max(1, [Math]::Ceiling($expectedSamplesPerRun * $MinimumSampleFraction))
$runs = [System.Collections.Generic.List[object]]::new()
$variantOrdinals = @{ A = 0; B = 0 }

for ($index = 0; $index -lt $expandedSequence.Length; $index += 1) {
    $variant = [string]$expandedSequence[$index]
    $variantOrdinals[$variant] += 1
    $isBaseline = $variant -eq "A"
    $label = if ($isBaseline) { $BaselineLabel } else { $CandidateLabel }
    $slug = if ($isBaseline) { $baselineSlug } else { $candidateSlug }
    $path = if ($isBaseline) { $baseline } else { $candidate }
    $metricName = "{0}-run-{1:D2}" -f $slug, $variantOrdinals[$variant]
    $environment = [System.Collections.Generic.List[string]]::new()
    foreach ($entry in $LaunchEnvironment) { $environment.Add($entry) }
    $replayReport = $null
    if (-not [string]::IsNullOrWhiteSpace($resolvedReplay)) {
        $replayReport = Join-Path $runDir ("{0}-replay.json" -f $metricName)
        $environment.Add("ECHO_CHAT_REPLAY=$resolvedReplay")
        $environment.Add("ECHO_CHAT_REPLAY_REPORT=$replayReport")
        $environment.Add("ECHO_CHAT_REPLAY_CLEAR_CHANNEL=1")
        $environment.Add("ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES=1")
        if ($EchoChatReplaySpeedup -gt 0.0) { $environment.Add("ECHO_CHAT_REPLAY_SPEEDUP=$EchoChatReplaySpeedup") }
    }

    $sampleArgs = @{
        App = "other"
        MetricName = $metricName
        RawFileName = "$metricName.raw.txt"
        RunDir = $runDir
        LaunchPath = $path
        LaunchArgument = $LaunchArgument
        LaunchEnvironment = @($environment)
        DurationSeconds = $DurationSeconds
        IntervalMilliseconds = $IntervalMilliseconds
        StartupWaitSeconds = $StartupWaitSeconds
        StopLaunchedProcess = $true
    }
    if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) { $sampleArgs.LaunchWorkingDirectory = $LaunchWorkingDirectory }
    $rawPath = ((& $sampler @sampleArgs | Select-Object -Last 1) | Out-String).Trim()
    $summary = Get-Content -Raw (Join-Path $runDir "summary.json") | ConvertFrom-Json
    $metric = Get-JsonProperty -Object $summary.metrics -Name $metricName
    if ($null -eq $metric -or [int]$metric.sample_count -lt $minimumSamplesPerRun) {
        $actualSamples = if ($null -eq $metric) { 0 } else { [int]$metric.sample_count }
        throw "Run '$metricName' produced $actualSamples samples; at least $minimumSamplesPerRun are required."
    }
    if ($null -ne $replayReport -and -not (Test-Path $replayReport -PathType Leaf)) {
        throw "Run '$metricName' did not produce its required replay report: $replayReport"
    }
    $runs.Add([pscustomobject][ordered]@{
        order = $index + 1
        variant = $variant
        label = $label
        metric_name = $metricName
        executable = $path
        raw_artifact = $rawPath
        replay_report = $replayReport
        metric = $metric
    })
}

$baselineRuns = @($runs | Where-Object { $_.variant -eq "A" })
$candidateRuns = @($runs | Where-Object { $_.variant -eq "B" })
$baselineAggregate = New-AggregatedMetric -Label $BaselineLabel -Runs $baselineRuns
$candidateAggregate = New-AggregatedMetric -Label $CandidateLabel -Runs $candidateRuns
$specs = @(
    [pscustomobject]@{ Name = "working_set_avg_bytes"; Path = @("working_set_bytes", "avg") },
    [pscustomobject]@{ Name = "working_set_max_bytes"; Path = @("working_set_bytes", "max") },
    [pscustomobject]@{ Name = "private_memory_avg_bytes"; Path = @("private_memory_bytes", "avg") },
    [pscustomobject]@{ Name = "private_memory_max_bytes"; Path = @("private_memory_bytes", "max") },
    [pscustomobject]@{ Name = "cpu_all_processors_avg_percent"; Path = @("cpu_percent_all_processors", "avg") },
    [pscustomobject]@{ Name = "cpu_all_processors_max_percent"; Path = @("cpu_percent_all_processors", "max") },
    [pscustomobject]@{ Name = "cpu_single_core_avg_percent"; Path = @("cpu_percent_single_core", "avg") },
    [pscustomobject]@{ Name = "cpu_single_core_max_percent"; Path = @("cpu_percent_single_core", "max") }
)
$rows = @($specs | ForEach-Object { New-ComparisonRow -Name $_.Name -Path $_.Path -Baseline $baselineAggregate -Candidate $candidateAggregate })

$comparisonPath = Join-Path $runDir "echo-ab-comparison.json"
$manifestPath = Join-Path $runDir "echo-ab-run.json"
$comparison = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    baseline_label = $BaselineLabel
    candidate_label = $CandidateLabel
    aggregation = "median_across_runs"
    baseline = $baselineAggregate
    candidate = $candidateAggregate
    metrics = $rows
    decision = "Echo variant A/B recorded; review required"
    note = "Resource deltas apply only to these Echo binaries, machine, ordering, and stimulus."
}
$manifest = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    scenario = $Scenario
    sequence = $expandedSequence
    base_sequence = $Sequence
    repeat_count = $RepeatCount
    duration_seconds = $DurationSeconds
    interval_milliseconds = $IntervalMilliseconds
    expected_samples_per_run = $expectedSamplesPerRun
    minimum_sample_fraction = $MinimumSampleFraction
    minimum_samples_per_run = $minimumSamplesPerRun
    startup_wait_seconds = $StartupWaitSeconds
    baseline = [ordered]@{ label = $BaselineLabel; path = $baseline; sha256 = (Get-FileHash -Algorithm SHA256 $baseline).Hash }
    candidate = [ordered]@{ label = $CandidateLabel; path = $candidate; sha256 = (Get-FileHash -Algorithm SHA256 $candidate).Hash }
    launch_argument = $LaunchArgument
    launch_environment_keys = @($LaunchEnvironment | ForEach-Object { ($_ -split "=", 2)[0] } | Sort-Object -Unique)
    replay_environment_keys = if ([string]::IsNullOrWhiteSpace($resolvedReplay)) { @() } else { @("ECHO_CHAT_REPLAY", "ECHO_CHAT_REPLAY_REPORT", "ECHO_CHAT_REPLAY_CLEAR_CHANNEL", "ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES", "ECHO_CHAT_REPLAY_SPEEDUP") }
    chat_replay = if ([string]::IsNullOrWhiteSpace($resolvedReplay)) { $null } else { [ordered]@{ path = $resolvedReplay; sha256 = (Get-FileHash -Algorithm SHA256 $resolvedReplay).Hash; speedup = $EchoChatReplaySpeedup } }
    runs = $runs
    comparison = $comparisonPath
    decision = "Echo variant A/B recorded; review required"
}
$comparison | ConvertTo-Json -Depth 14 | Set-Content -Path $comparisonPath -Encoding utf8
$manifest | ConvertTo-Json -Depth 14 | Set-Content -Path $manifestPath -Encoding utf8

$summaryPath = Join-Path $runDir "summary.json"
$summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
Set-JsonProperty -Object $summary.metrics -Name $baselineSlug -Value $baselineAggregate
Set-JsonProperty -Object $summary.metrics -Name $candidateSlug -Value $candidateAggregate
Set-JsonProperty -Object $summary.raw_artifacts -Name "echo_ab_comparison" -Value $comparisonPath
Set-JsonProperty -Object $summary.raw_artifacts -Name "echo_ab_run" -Value $manifestPath
Set-JsonProperty -Object $summary -Name "comparison" -Value ([pscustomobject]$comparison)
Set-JsonProperty -Object $summary -Name "status" -Value "compared"
Set-JsonProperty -Object $summary -Name "decision" -Value "Echo variant A/B recorded; review required"
$summary | ConvertTo-Json -Depth 16 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $runDir "notes.md"
$existingNotes = Get-Content -Raw $notesPath
$existingNotes = $existingNotes -replace "(?m)^- Status: initialized\r?$", "- Status: compared"
$existingNotes = $existingNotes -replace "(?m)^- Measurements recorded: false\r?$", "- Measurements recorded: true"
Set-Content -Path $notesPath -Value $existingNotes.TrimEnd() -Encoding utf8
Add-Content -Path $notesPath -Value "`n## Echo variant A/B`n"
Add-Content -Path $notesPath -Value "- Sequence: $expandedSequence"
Add-Content -Path $notesPath -Value "- Baseline: $BaselineLabel ($baseline)"
Add-Content -Path $notesPath -Value "- Candidate: $CandidateLabel ($candidate)"
Add-Content -Path $notesPath -Value "- Comparison: $comparisonPath"
Add-Content -Path $notesPath -Value "- Manifest: $manifestPath"
Add-Content -Path $notesPath -Value "- Decision: Echo variant A/B recorded; review required"

Write-Output $runDir
