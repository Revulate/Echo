[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TracePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$RunDir = "",
    [string]$OutputFile = "",

    [ValidateRange(1, 1000)]
    [double]$TargetFrameTimeMs = 16.667,

    [ValidateRange(1, 1000)]
    [double]$DroppedFrameThresholdMs = 33.333
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

function Find-FrameTimeColumn {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Columns
    )

    $preferred = @(
        "MsUntilDisplayed",
        "msUntilDisplayed",
        "MsBetweenPresents",
        "msBetweenPresents",
        "MsInPresentAPI",
        "frame_time_ms",
        "FrameTimeMs",
        "frameTimeMs",
        "frametime",
        "FrameTime",
        "duration_ms",
        "DurationMs"
    )

    foreach ($column in $preferred) {
        if ($Columns -contains $column) {
            return $column
        }
    }

    return $null
}

$resolvedTracePath = (Resolve-Path $TracePath).Path
$rows = @(Import-Csv -Path $resolvedTracePath)
if ($rows.Count -eq 0) {
    throw "Frame trace has no rows: $resolvedTracePath"
}

$columns = @($rows[0].PSObject.Properties.Name)
$frameTimeColumn = Find-FrameTimeColumn -Columns $columns
if ([string]::IsNullOrWhiteSpace($frameTimeColumn)) {
    throw "Could not identify a frame-time column in trace. Columns: $($columns -join ', ')"
}

$frameTimes = [System.Collections.Generic.List[double]]::new()
foreach ($row in $rows) {
    $rawValue = Get-JsonProperty -Object $row -Name $frameTimeColumn
    if ($null -eq $rawValue -or [string]::IsNullOrWhiteSpace([string]$rawValue)) {
        continue
    }

    $parsed = 0.0
    if ([double]::TryParse(
        [string]$rawValue,
        [System.Globalization.NumberStyles]::Float,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [ref]$parsed
    )) {
        if ($parsed -ge 0) {
            $frameTimes.Add($parsed) | Out-Null
        }
    }
}

if ($frameTimes.Count -eq 0) {
    throw "Frame trace did not contain any parseable nonnegative frame times in column '$frameTimeColumn'."
}

$sortedFrameTimes = [double[]]@($frameTimes | Sort-Object)
$total = 0.0
$overTarget = 0
$dropped = 0
foreach ($frameTime in $frameTimes) {
    $total += $frameTime
    if ($frameTime -gt $TargetFrameTimeMs) {
        $overTarget += 1
    }
    if ($frameTime -gt $DroppedFrameThresholdMs) {
        $dropped += 1
    }
}

$summary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    trace_path = $resolvedTracePath
    frame_time_column = $frameTimeColumn
    frame_count = $frameTimes.Count
    target_frame_time_ms = $TargetFrameTimeMs
    dropped_frame_threshold_ms = $DroppedFrameThresholdMs
    min_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 0
    p50_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 50
    p90_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 90
    p95_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 95
    p99_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 99
    max_ms = Get-Percentile -SortedValues $sortedFrameTimes -Percentile 100
    average_ms = [Math]::Round($total / $frameTimes.Count, 6)
    frames_over_target = $overTarget
    dropped_frame_count = $dropped
    frames_over_target_percent = [Math]::Round(($overTarget / [double]$frameTimes.Count) * 100.0, 6)
    dropped_frame_percent = [Math]::Round(($dropped / [double]$frameTimes.Count) * 100.0, 6)
    decision = "frame trace summarized; not an app comparison"
}

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $resolvedTracePath
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir ("{0}.frame-summary.json" -f (ConvertTo-Slug $App))
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $runSummary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = "frame trace summary"
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
$frameTraces = Ensure-ObjectProperty -Object $rawArtifacts -Name "frame_traces"
$frameSummaries = Ensure-ObjectProperty -Object $rawArtifacts -Name "frame_summaries"
$appKey = ConvertTo-Slug -Value $App
Set-JsonProperty -Object $frameTraces -Name $appKey -Value $resolvedTracePath
Set-JsonProperty -Object $frameSummaries -Name $appKey -Value $OutputFile
Set-JsonProperty -Object $rawArtifacts -Name "frame_trace" -Value $OutputFile
$frameSummary = Ensure-ObjectProperty -Object $runSummary -Name "frame_summary"
Set-JsonProperty -Object $frameSummary -Name $appKey -Value ([pscustomobject]$summary)
Set-JsonProperty -Object $runSummary -Name "decision" -Value "frame trace summarized; not compared"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Frame trace summary"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- App: $App"
    Add-Content -Path $notesPath -Value "- Trace artifact: $resolvedTracePath"
    Add-Content -Path $notesPath -Value "- Summary artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Frame count: $($summary.frame_count)"
    Add-Content -Path $notesPath -Value "- P95 ms: $($summary.p95_ms)"
    Add-Content -Path $notesPath -Value "- Decision: frame trace summarized; not compared"
}

Write-Output $OutputFile
