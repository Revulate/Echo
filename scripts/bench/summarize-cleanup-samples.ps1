[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$SamplePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$RunDir = "",
    [string]$OutputFile = "",

    [double]$CleanupStartElapsedMs = -1,

    [ValidateRange(1, 86400000)]
    [int]$BeforeWindowMilliseconds = 300000,

    [ValidateRange(1, 86400000)]
    [int]$AfterWindowMilliseconds = 300000
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function ConvertTo-Slug {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    $slug = $Value.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-"
    $slug = $slug.Trim("-")
    if ([string]::IsNullOrWhiteSpace($slug)) {
        return "value"
    }

    return $slug
}

function Get-JsonProperty {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Object,

        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $property = $Object.PSObject.Properties.Match($Name) | Select-Object -First 1
    if ($null -eq $property) {
        return $null
    }

    return $property.Value
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

function Ensure-ObjectProperty {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Object,

        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    $value = Get-JsonProperty -Object $Object -Name $Name
    if ($null -eq $value) {
        $value = [pscustomobject]@{}
        Set-JsonProperty -Object $Object -Name $Name -Value $value
    }

    return $value
}

function Get-FirstPropertyValue {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Object,

        [Parameter(Mandatory = $true)]
        [string[]]$Names
    )

    foreach ($name in $Names) {
        $value = Get-JsonProperty -Object $Object -Name $name
        if ($null -ne $value -and -not [string]::IsNullOrWhiteSpace([string]$value)) {
            return $value
        }
    }

    return $null
}

function ConvertTo-NullableDouble {
    param(
        [object]$Value
    )

    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) {
        return $null
    }

    $parsed = 0.0
    if ([double]::TryParse(
        [string]$Value,
        [System.Globalization.NumberStyles]::Float,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [ref]$parsed
    )) {
        return $parsed
    }

    return $null
}

function Get-ElapsedMs {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Sample
    )

    return ConvertTo-NullableDouble -Value (Get-FirstPropertyValue -Object $Sample -Names @("elapsed_ms", "time_ms", "timestamp_ms", "t_ms"))
}

function Get-Percentile {
    param(
        [Parameter(Mandatory = $true)]
        [double[]]$SortedValues,

        [Parameter(Mandatory = $true)]
        [ValidateRange(0, 100)]
        [double]$Percentile
    )

    if ($SortedValues.Count -eq 0) {
        return $null
    }

    if ($SortedValues.Count -eq 1) {
        return [Math]::Round($SortedValues[0], 6)
    }

    $rank = ($Percentile / 100.0) * ($SortedValues.Count - 1)
    $lower = [Math]::Floor($rank)
    $upper = [Math]::Ceiling($rank)
    if ($lower -eq $upper) {
        return [Math]::Round($SortedValues[$lower], 6)
    }

    $weight = $rank - $lower
    $value = ($SortedValues[$lower] * (1.0 - $weight)) + ($SortedValues[$upper] * $weight)
    return [Math]::Round($value, 6)
}

function Get-SeriesStats {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Samples,

        [Parameter(Mandatory = $true)]
        [string[]]$Aliases
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
            sample_count = 0
            first = $null
            last = $null
            min = $null
            avg = $null
            p50 = $null
            p95 = $null
            max = $null
        }
    }

    $sorted = [double[]]@($values | Sort-Object)
    $total = 0.0
    foreach ($value in $values) {
        $total += $value
    }

    return [ordered]@{
        sample_count = $values.Count
        first = [Math]::Round($values[0], 6)
        last = [Math]::Round($values[$values.Count - 1], 6)
        min = Get-Percentile -SortedValues $sorted -Percentile 0
        avg = [Math]::Round($total / $values.Count, 6)
        p50 = Get-Percentile -SortedValues $sorted -Percentile 50
        p95 = Get-Percentile -SortedValues $sorted -Percentile 95
        max = Get-Percentile -SortedValues $sorted -Percentile 100
    }
}

function New-WindowComparison {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$BeforeSamples,

        [Parameter(Mandatory = $true)]
        [object[]]$AfterSamples,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [string[]]$Aliases
    )

    $before = Get-SeriesStats -Samples $BeforeSamples -Aliases $Aliases
    $after = Get-SeriesStats -Samples $AfterSamples -Aliases $Aliases
    $deltaLast = $null
    $deltaAvg = $null
    $deltaLastPercent = $null
    $afterLowerThanBeforeLast = $null
    if ($null -ne $before.last -and $null -ne $after.last) {
        $deltaLast = [Math]::Round([double]$after.last - [double]$before.last, 6)
        $afterLowerThanBeforeLast = [double]$after.last -lt [double]$before.last
        if ([double]$before.last -ne 0.0) {
            $deltaLastPercent = [Math]::Round(($deltaLast / [double]$before.last) * 100.0, 6)
        }
    }
    if ($null -ne $before.avg -and $null -ne $after.avg) {
        $deltaAvg = [Math]::Round([double]$after.avg - [double]$before.avg, 6)
    }

    return [ordered]@{
        name = $Name
        before = [pscustomobject]$before
        after = [pscustomobject]$after
        delta_last = $deltaLast
        delta_last_percent = $deltaLastPercent
        delta_avg = $deltaAvg
        after_lower_than_before_last = $afterLowerThanBeforeLast
    }
}

function Find-CleanupStartElapsedMs {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Samples
    )

    foreach ($sample in $Samples) {
        $explicitStart = ConvertTo-NullableDouble -Value (Get-FirstPropertyValue -Object $sample.row -Names @("cleanup_start_elapsed_ms", "leave_elapsed_ms"))
        if ($null -ne $explicitStart) {
            return [double]$explicitStart
        }

        $marker = Get-FirstPropertyValue -Object $sample.row -Names @("phase", "state", "event", "marker")
        if ($null -eq $marker) {
            continue
        }

        $normalized = ([string]$marker).ToLowerInvariant()
        if ($normalized -match "leave|left|cleanup|after[-_ ]?leave|channels[-_ ]?left") {
            return [double]$sample.elapsed_ms
        }
    }

    throw "Cleanup boundary not found. Provide -CleanupStartElapsedMs or a sample row with phase/state/event/marker containing leave, left, cleanup, after-leave, or channels-left."
}

$resolvedSamplePath = (Resolve-Path $SamplePath).Path
$rows = @(Import-Csv -Path $resolvedSamplePath)
if ($rows.Count -eq 0) {
    throw "Cleanup sample file has no rows: $resolvedSamplePath"
}

$timedRowList = [System.Collections.Generic.List[object]]::new()
for ($index = 0; $index -lt $rows.Count; $index++) {
    $row = $rows[$index]
    $elapsed = Get-ElapsedMs -Sample $row
    if ($null -eq $elapsed) {
        continue
    }

    $timedRowList.Add([pscustomobject][ordered]@{
            sequence = $index
            elapsed_ms = [double]$elapsed
            row = $row
        }) | Out-Null
}

$timedRows = @($timedRowList | Sort-Object elapsed_ms, sequence)

if ($timedRows.Count -eq 0) {
    throw "Cleanup sample file needs an elapsed_ms, time_ms, timestamp_ms, or t_ms column."
}

$cleanupStart = if ($CleanupStartElapsedMs -ge 0) {
    [double]$CleanupStartElapsedMs
} else {
    Find-CleanupStartElapsedMs -Samples $timedRows
}

$beforeStart = [Math]::Max(0.0, $cleanupStart - [double]$BeforeWindowMilliseconds)
$afterEnd = $cleanupStart + [double]$AfterWindowMilliseconds
$beforeRows = @($timedRows | Where-Object { $_.elapsed_ms -lt $cleanupStart -and $_.elapsed_ms -ge $beforeStart })
$afterRows = @($timedRows | Where-Object { $_.elapsed_ms -ge $cleanupStart -and $_.elapsed_ms -le $afterEnd })

if ($beforeRows.Count -eq 0) {
    throw "No samples found before cleanup boundary in the configured before window."
}
if ($afterRows.Count -eq 0) {
    throw "No samples found after cleanup boundary in the configured after window."
}

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

$beforeSampleObjects = @($beforeRows | ForEach-Object { $_.row })
$afterSampleObjects = @($afterRows | ForEach-Object { $_.row })
$metrics = [pscustomobject]@{}
foreach ($spec in $seriesSpecs) {
    Set-JsonProperty `
        -Object $metrics `
        -Name $spec.name `
        -Value ([pscustomobject](New-WindowComparison `
                -BeforeSamples $beforeSampleObjects `
                -AfterSamples $afterSampleObjects `
                -Name $spec.name `
                -Aliases $spec.aliases))
}

$exitedSamples = @($timedRows | Where-Object {
        $hasExited = Get-FirstPropertyValue -Object $_.row -Names @("has_exited", "process_exited", "exited")
        $null -ne $hasExited -and [string]$hasExited -match "^(true|1|yes)$"
    })

$firstRow = ($timedRows | Select-Object -First 1).row
$lastRow = ($timedRows | Select-Object -Last 1).row
$summary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    sample_path = $resolvedSamplePath
    sample_count = $timedRows.Count
    first_timestamp_utc = Get-FirstPropertyValue -Object $firstRow -Names @("timestamp_utc", "timestamp", "time_utc")
    last_timestamp_utc = Get-FirstPropertyValue -Object $lastRow -Names @("timestamp_utc", "timestamp", "time_utc")
    first_elapsed_ms = [Math]::Round([double]($timedRows | Select-Object -First 1).elapsed_ms, 6)
    last_elapsed_ms = [Math]::Round([double]($timedRows | Select-Object -Last 1).elapsed_ms, 6)
    cleanup_start_elapsed_ms = [Math]::Round($cleanupStart, 6)
    before_window_ms = $BeforeWindowMilliseconds
    after_window_ms = $AfterWindowMilliseconds
    before_sample_count = $beforeRows.Count
    after_sample_count = $afterRows.Count
    process_exited_sample_count = $exitedSamples.Count
    metrics = $metrics
    decision = "cleanup samples summarized; not an app comparison"
}

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $resolvedSamplePath
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir ("{0}.cleanup-summary.json" -f (ConvertTo-Slug $App))
}

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $runSummary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = "cleanup summary"
        status = "initialized"
        measurements_recorded = $true
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
$cleanupRawSamples = Ensure-ObjectProperty -Object $rawArtifacts -Name "cleanup_raw_samples"
$cleanupSummaries = Ensure-ObjectProperty -Object $rawArtifacts -Name "cleanup_summaries"
$appKey = ConvertTo-Slug -Value $App
Set-JsonProperty -Object $cleanupRawSamples -Name $appKey -Value $resolvedSamplePath
Set-JsonProperty -Object $cleanupSummaries -Name $appKey -Value $OutputFile
Set-JsonProperty -Object $rawArtifacts -Name "cleanup_samples" -Value $OutputFile
$cleanupSummary = Ensure-ObjectProperty -Object $runSummary -Name "cleanup_summary"
Set-JsonProperty -Object $cleanupSummary -Name $appKey -Value ([pscustomobject]$summary)
Set-JsonProperty -Object $runSummary -Name "measurements_recorded" -Value $true
Set-JsonProperty -Object $runSummary -Name "decision" -Value "cleanup samples summarized; not compared"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Cleanup sample summary"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- App: $App"
    Add-Content -Path $notesPath -Value "- Raw sample artifact: $resolvedSamplePath"
    Add-Content -Path $notesPath -Value "- Summary artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Cleanup start elapsed ms: $($summary.cleanup_start_elapsed_ms)"
    Add-Content -Path $notesPath -Value "- Before samples: $($summary.before_sample_count)"
    Add-Content -Path $notesPath -Value "- After samples: $($summary.after_sample_count)"
    Add-Content -Path $notesPath -Value "- Decision: cleanup samples summarized; not compared"
}

Write-Output $OutputFile
