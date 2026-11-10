<#
.SYNOPSIS
Summarize saved long-session process samples.
summarize-long-session-samples.py is the Python twin.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SamplePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$RunDir = "",
    [string]$OutputFile = "",

    [ValidateRange(1, 86400000)]
    [int]$MinimumDurationMilliseconds = 1
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function New-SeriesSummary {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Samples,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [string[]]$Aliases,

        [Parameter(Mandatory = $true)]
        [double]$ObservedDurationMinutes
    )

    $values = [System.Collections.Generic.List[double]]::new()
    foreach ($sample in $Samples) {
        $rawValue = Get-FirstPropertyValue -Object $sample -Names $Aliases
        $parsed = ConvertTo-NullableDouble -Value $rawValue
        if ($null -ne $parsed) {
            $values.Add([double]$parsed) | Out-Null
        }
    }

    if ($values.Count -eq 0) {
        return [ordered]@{
            name = $Name
            sample_count = 0
            first = $null
            last = $null
            min = $null
            avg = $null
            p50 = $null
            p95 = $null
            max = $null
            delta = $null
            delta_percent = $null
            max_minus_min = $null
            growth_per_minute = $null
        }
    }

    $sorted = [double[]]@($values | Sort-Object)
    $total = 0.0
    foreach ($value in $values) {
        $total += $value
    }

    $first = $values[0]
    $last = $values[$values.Count - 1]
    $delta = $last - $first
    $deltaPercent = $null
    if ($first -ne 0.0) {
        $deltaPercent = [Math]::Round(($delta / $first) * 100.0, 6)
    }

    return [ordered]@{
        name = $Name
        sample_count = $values.Count
        first = [Math]::Round($first, 6)
        last = [Math]::Round($last, 6)
        min = Get-Percentile -Values $sorted -Percentile 0
        avg = [Math]::Round($total / $values.Count, 6)
        p50 = Get-Percentile -Values $sorted -Percentile 50
        p95 = Get-Percentile -Values $sorted -Percentile 95
        max = Get-Percentile -Values $sorted -Percentile 100
        delta = [Math]::Round($delta, 6)
        delta_percent = $deltaPercent
        max_minus_min = [Math]::Round($sorted[$sorted.Count - 1] - $sorted[0], 6)
        growth_per_minute = if ($ObservedDurationMinutes -gt 0) { [Math]::Round($delta / $ObservedDurationMinutes, 6) } else { $null }
    }
}

$resolvedSamplePath = (Resolve-Path $SamplePath).Path
$rows = @(Import-Csv -Path $resolvedSamplePath)
if ($rows.Count -eq 0) {
    throw "Long-session sample file has no rows: $resolvedSamplePath"
}

$orderedRows = @(
    $rows |
        Sort-Object @{
            Expression = {
                $elapsed = ConvertTo-NullableDouble -Value (Get-FirstPropertyValue -Object $_ -Names @("elapsed_ms", "time_ms", "timestamp_ms", "t_ms"))
                if ($null -eq $elapsed) { [double]::PositiveInfinity } else { [double]$elapsed }
            }
        }, sample_index
)

$firstRow = $orderedRows | Select-Object -First 1
$lastRow = $orderedRows | Select-Object -Last 1
$firstElapsed = ConvertTo-NullableDouble -Value (Get-FirstPropertyValue -Object $firstRow -Names @("elapsed_ms", "time_ms", "timestamp_ms", "t_ms"))
$lastElapsed = ConvertTo-NullableDouble -Value (Get-FirstPropertyValue -Object $lastRow -Names @("elapsed_ms", "time_ms", "timestamp_ms", "t_ms"))
$observedDurationMs = if ($null -ne $firstElapsed -and $null -ne $lastElapsed) {
    [Math]::Max(0.0, [double]$lastElapsed - [double]$firstElapsed)
} else {
    $null
}

if ($null -eq $observedDurationMs) {
    throw "Long-session sample file needs an elapsed_ms, time_ms, timestamp_ms, or t_ms column."
}

if ($observedDurationMs -lt $MinimumDurationMilliseconds) {
    throw "Long-session sample duration is shorter than required: observed=${observedDurationMs}ms minimum=${MinimumDurationMilliseconds}ms."
}

$observedDurationMinutes = [double]$observedDurationMs / 60000.0
$exitedSamples = @($orderedRows | Where-Object {
        $hasExited = Get-FirstPropertyValue -Object $_ -Names @("has_exited", "process_exited", "exited")
        $null -ne $hasExited -and [string]$hasExited -match "^(true|1|yes)$"
    })

$seriesSpecs = @(
    [ordered]@{ name = "working_set_bytes"; aliases = @("working_set_bytes", "rss_bytes", "rss", "working_set") },
    [ordered]@{ name = "private_memory_bytes"; aliases = @("private_memory_bytes", "private_bytes", "private_memory") },
    [ordered]@{ name = "paged_memory_bytes"; aliases = @("paged_memory_bytes", "paged_bytes") },
    [ordered]@{ name = "virtual_memory_bytes"; aliases = @("virtual_memory_bytes", "virtual_bytes", "vms_bytes") },
    [ordered]@{ name = "handle_count"; aliases = @("handle_count", "handles") },
    [ordered]@{ name = "thread_count"; aliases = @("thread_count", "threads") },
    [ordered]@{ name = "cpu_percent_all_processors"; aliases = @("cpu_percent_all_processors", "cpu_percent", "cpu_all_processors_percent") },
    [ordered]@{ name = "cpu_percent_single_core"; aliases = @("cpu_percent_single_core", "cpu_single_core_percent") }
)

$metrics = [pscustomobject]@{}
foreach ($spec in $seriesSpecs) {
    Set-JsonProperty `
        -Object $metrics `
        -Name $spec.name `
        -Value ([pscustomobject](New-SeriesSummary `
                -Samples $orderedRows `
                -Name $spec.name `
                -Aliases $spec.aliases `
                -ObservedDurationMinutes $observedDurationMinutes))
}

$summary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    sample_path = $resolvedSamplePath
    sample_count = $orderedRows.Count
    first_timestamp_utc = Get-FirstPropertyValue -Object $firstRow -Names @("timestamp_utc", "timestamp", "time_utc")
    last_timestamp_utc = Get-FirstPropertyValue -Object $lastRow -Names @("timestamp_utc", "timestamp", "time_utc")
    first_elapsed_ms = [Math]::Round([double]$firstElapsed, 6)
    last_elapsed_ms = [Math]::Round([double]$lastElapsed, 6)
    observed_duration_ms = [Math]::Round([double]$observedDurationMs, 6)
    observed_duration_seconds = [Math]::Round([double]$observedDurationMs / 1000.0, 6)
    minimum_duration_ms = $MinimumDurationMilliseconds
    process_exited_sample_count = $exitedSamples.Count
    metrics = $metrics
    decision = "long-session samples summarized; not an app comparison"
}

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $resolvedSamplePath
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir ("{0}.long-session-summary.json" -f (ConvertTo-Slug $App))
}

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $runSummary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = "long session summary"
        status = "initialized"
        measurements_recorded = $true
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
$longSessionRawSamples = Ensure-ObjectProperty -Object $rawArtifacts -Name "long_session_raw_samples"
$longSessionSummaries = Ensure-ObjectProperty -Object $rawArtifacts -Name "long_session_summaries"
$appKey = ConvertTo-Slug -Value $App
Set-JsonProperty -Object $longSessionRawSamples -Name $appKey -Value $resolvedSamplePath
Set-JsonProperty -Object $longSessionSummaries -Name $appKey -Value $OutputFile
Set-JsonProperty -Object $rawArtifacts -Name "long_session_samples" -Value $OutputFile
$longSessionSummary = Ensure-ObjectProperty -Object $runSummary -Name "long_session_summary"
Set-JsonProperty -Object $longSessionSummary -Name $appKey -Value ([pscustomobject]$summary)
Set-JsonProperty -Object $runSummary -Name "measurements_recorded" -Value $true
Set-JsonProperty -Object $runSummary -Name "decision" -Value "long-session samples summarized; not compared"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Long-session sample summary"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- App: $App"
    Add-Content -Path $notesPath -Value "- Raw sample artifact: $resolvedSamplePath"
    Add-Content -Path $notesPath -Value "- Summary artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Samples: $($summary.sample_count)"
    Add-Content -Path $notesPath -Value "- Observed duration seconds: $($summary.observed_duration_seconds)"
    Add-Content -Path $notesPath -Value "- Decision: long-session samples summarized; not compared"
}

Write-Output $OutputFile
