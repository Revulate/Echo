[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TracePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$RunDir = "",
    [string]$OutputFile = "",
    [int]$TargetProcessId = 0,
    [string]$TargetProcessName = "",

    [string]$GpuTimeColumn = "",
    [string]$GpuUtilizationColumn = "",
    [string]$GpuMemoryColumn = ""
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

function Normalize-ColumnName {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    return ($Value.ToLowerInvariant() -replace "[^a-z0-9]+", "")
}

function Resolve-TraceColumn {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Columns,

        [string]$ExplicitColumn = "",

        [Parameter(Mandatory = $true)]
        [string[]]$Aliases
    )

    if (-not [string]::IsNullOrWhiteSpace($ExplicitColumn)) {
        if ($Columns -contains $ExplicitColumn) {
            return $ExplicitColumn
        }

        throw "Column '$ExplicitColumn' was not found. Available columns: $($Columns -join ', ')"
    }

    $normalizedAliases = @($Aliases | ForEach-Object { Normalize-ColumnName -Value $_ })
    foreach ($column in $Columns) {
        $normalizedColumn = Normalize-ColumnName -Value $column
        if ($normalizedAliases -contains $normalizedColumn) {
            return $column
        }
    }

    return $null
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

function Get-MetricMultiplier {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Column,

        [Parameter(Mandatory = $true)]
        [string]$MetricName
    )

    $normalizedColumn = Normalize-ColumnName -Value $Column

    if ($MetricName -eq "gpu_time_ms") {
        if ($normalizedColumn -match "micro|usec|us$") {
            return 0.001
        }
        if ($normalizedColumn -match "nanosecond|nsec|ns$") {
            return 0.000001
        }
        if ($normalizedColumn -match "second|sec$|seconds$" -and $normalizedColumn -notmatch "ms|millisecond") {
            return 1000.0
        }
    }

    if ($MetricName -eq "gpu_memory_bytes") {
        if ($normalizedColumn -match "gib") {
            return 1073741824.0
        }
        if ($normalizedColumn -match "gb") {
            return 1000000000.0
        }
        if ($normalizedColumn -match "mib") {
            return 1048576.0
        }
        if ($normalizedColumn -match "mb") {
            return 1000000.0
        }
        if ($normalizedColumn -match "kib") {
            return 1024.0
        }
        if ($normalizedColumn -match "kb") {
            return 1000.0
        }
    }

    return 1.0
}

function New-SeriesSummary {
    param(
        [Parameter(Mandatory = $true)]
        [object[]]$Rows,

        [Parameter(Mandatory = $true)]
        [string]$Column,

        [Parameter(Mandatory = $true)]
        [string]$MetricName,

        [Parameter(Mandatory = $true)]
        [string]$Unit,

        [Parameter(Mandatory = $true)]
        [bool]$LowerIsBetter
    )

    $multiplier = Get-MetricMultiplier -Column $Column -MetricName $MetricName
    $values = [System.Collections.Generic.List[double]]::new()
    foreach ($row in $Rows) {
        $rawValue = Get-JsonProperty -Object $row -Name $Column
        $parsed = ConvertTo-NullableDouble -Value $rawValue
        if ($null -ne $parsed) {
            $values.Add([double]$parsed * [double]$multiplier) | Out-Null
        }
    }

    if ($values.Count -eq 0) {
        return [ordered]@{
            name = $MetricName
            column = $Column
            unit = $Unit
            sample_count = 0
            min = $null
            avg = $null
            p50 = $null
            p95 = $null
            p99 = $null
            max = $null
            lower_is_better = $LowerIsBetter
        }
    }

    $sorted = [double[]]@($values | Sort-Object)
    $total = 0.0
    foreach ($value in $values) {
        $total += $value
    }

    return [ordered]@{
        name = $MetricName
        column = $Column
        unit = $Unit
        sample_count = $values.Count
        min = Get-Percentile -SortedValues $sorted -Percentile 0
        avg = [Math]::Round($total / $values.Count, 6)
        p50 = Get-Percentile -SortedValues $sorted -Percentile 50
        p95 = Get-Percentile -SortedValues $sorted -Percentile 95
        p99 = Get-Percentile -SortedValues $sorted -Percentile 99
        max = Get-Percentile -SortedValues $sorted -Percentile 100
        lower_is_better = $LowerIsBetter
    }
}

$resolvedTracePath = (Resolve-Path $TracePath).Path
$rows = @(Import-Csv -Path $resolvedTracePath)
if ($rows.Count -eq 0) {
    throw "GPU trace has no rows: $resolvedTracePath"
}

if ($TargetProcessId -gt 0) {
    $rows = @($rows | Where-Object {
        $value = $_.PSObject.Properties["ProcessID"]
        $null -ne $value -and [int]$value.Value -eq $TargetProcessId
    })
    if ($rows.Count -eq 0) {
        throw "GPU trace has no rows for target process ID $TargetProcessId"
    }
} elseif (-not [string]::IsNullOrWhiteSpace($TargetProcessName)) {
    $rows = @($rows | Where-Object {
        $value = $_.PSObject.Properties["Application"]
        $null -ne $value -and [string]$value.Value -ieq $TargetProcessName
    })
    if ($rows.Count -eq 0) {
        throw "GPU trace has no rows for target process '$TargetProcessName'"
    }
}

$columns = @($rows[0].PSObject.Properties.Name)
$gpuTimeAliases = @(
    "MsGPUTime",
    "MsGPUBusy",
    "gpu_time_ms",
    "GpuTimeMs",
    "GPUTimeMs",
    "gpu_duration_ms",
    "GpuDurationMs",
    "GPUDurationMs",
    "gpu_busy_ms",
    "GpuBusyMs",
    "GPUBusyMs",
    "gpu_elapsed_ms",
    "GpuElapsedMs",
    "ms_on_gpu",
    "MsOnGpu",
    "GPU Time (ms)",
    "GPU Duration (ms)",
    "GPU Busy (ms)"
)
$gpuUtilizationAliases = @(
    "gpu_utilization_percent",
    "GpuUtilizationPercent",
    "GPUUtilizationPercent",
    "gpu_usage_percent",
    "GpuUsagePercent",
    "GPUUsagePercent",
    "gpu_busy_percent",
    "GpuBusyPercent",
    "GPUBusyPercent",
    "gpu_percent",
    "GPU Percent",
    "GPU Busy (%)",
    "GPU Utilization (%)"
)
$gpuMemoryAliases = @(
    "gpu_memory_bytes",
    "GpuMemoryBytes",
    "GPUMemoryBytes",
    "dedicated_gpu_memory_bytes",
    "DedicatedGpuMemoryBytes",
    "vram_bytes",
    "VRAMBytes",
    "gpu_memory_mb",
    "GpuMemoryMB",
    "GPUMemoryMB",
    "vram_mb",
    "VRAMMB",
    "GPU Memory (MB)",
    "Dedicated GPU Memory (MB)"
)

$resolvedGpuTimeColumn = Resolve-TraceColumn -Columns $columns -ExplicitColumn $GpuTimeColumn -Aliases $gpuTimeAliases
$resolvedGpuUtilizationColumn = Resolve-TraceColumn -Columns $columns -ExplicitColumn $GpuUtilizationColumn -Aliases $gpuUtilizationAliases
$resolvedGpuMemoryColumn = Resolve-TraceColumn -Columns $columns -ExplicitColumn $GpuMemoryColumn -Aliases $gpuMemoryAliases

if ([string]::IsNullOrWhiteSpace($resolvedGpuTimeColumn) -and
    [string]::IsNullOrWhiteSpace($resolvedGpuUtilizationColumn) -and
    [string]::IsNullOrWhiteSpace($resolvedGpuMemoryColumn)) {
    throw "Could not identify GPU time, utilization, or memory columns. Provide -GpuTimeColumn, -GpuUtilizationColumn, or -GpuMemoryColumn. Available columns: $($columns -join ', ')"
}

$metrics = [pscustomobject]@{}
if (-not [string]::IsNullOrWhiteSpace($resolvedGpuTimeColumn)) {
    Set-JsonProperty `
        -Object $metrics `
        -Name "gpu_time_ms" `
        -Value ([pscustomobject](New-SeriesSummary `
                -Rows $rows `
                -Column $resolvedGpuTimeColumn `
                -MetricName "gpu_time_ms" `
                -Unit "ms" `
                -LowerIsBetter $true))
}
if (-not [string]::IsNullOrWhiteSpace($resolvedGpuUtilizationColumn)) {
    Set-JsonProperty `
        -Object $metrics `
        -Name "gpu_utilization_percent" `
        -Value ([pscustomobject](New-SeriesSummary `
                -Rows $rows `
                -Column $resolvedGpuUtilizationColumn `
                -MetricName "gpu_utilization_percent" `
                -Unit "percent" `
                -LowerIsBetter $true))
}
if (-not [string]::IsNullOrWhiteSpace($resolvedGpuMemoryColumn)) {
    Set-JsonProperty `
        -Object $metrics `
        -Name "gpu_memory_bytes" `
        -Value ([pscustomobject](New-SeriesSummary `
                -Rows $rows `
                -Column $resolvedGpuMemoryColumn `
                -MetricName "gpu_memory_bytes" `
                -Unit "bytes" `
                -LowerIsBetter $true))
}

$metricProperties = @($metrics.PSObject.Properties)
$sampleCount = 0
foreach ($metricProperty in $metricProperties) {
    $metricSampleCount = Get-JsonProperty -Object $metricProperty.Value -Name "sample_count"
    if ($null -ne $metricSampleCount) {
        $sampleCount = [Math]::Max($sampleCount, [int]$metricSampleCount)
    }
}

if ($sampleCount -eq 0) {
    throw "GPU trace columns were found, but no numeric GPU metric samples could be parsed."
}

$summary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    trace_path = $resolvedTracePath
    row_count = $rows.Count
    sample_count = $sampleCount
    metric_count = $metricProperties.Count
    columns = [ordered]@{
        gpu_time_ms = $resolvedGpuTimeColumn
        gpu_utilization_percent = $resolvedGpuUtilizationColumn
        gpu_memory_bytes = $resolvedGpuMemoryColumn
    }
    metrics = $metrics
    decision = "GPU trace summarized; not an app comparison"
}

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $resolvedTracePath
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir ("{0}.gpu-summary.json" -f (ConvertTo-Slug $App))
}

$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $runSummary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = "GPU trace summary"
        status = "initialized"
        measurements_recorded = $true
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
$gpuTraces = Ensure-ObjectProperty -Object $rawArtifacts -Name "gpu_traces"
$gpuSummaries = Ensure-ObjectProperty -Object $rawArtifacts -Name "gpu_summaries"
$appKey = ConvertTo-Slug -Value $App
Set-JsonProperty -Object $gpuTraces -Name $appKey -Value $resolvedTracePath
Set-JsonProperty -Object $gpuSummaries -Name $appKey -Value $OutputFile
Set-JsonProperty -Object $rawArtifacts -Name "gpu_trace" -Value $OutputFile
$gpuSummary = Ensure-ObjectProperty -Object $runSummary -Name "gpu_summary"
Set-JsonProperty -Object $gpuSummary -Name $appKey -Value ([pscustomobject]$summary)
Set-JsonProperty -Object $runSummary -Name "measurements_recorded" -Value $true
Set-JsonProperty -Object $runSummary -Name "decision" -Value "GPU trace summarized; not compared"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## GPU trace summary"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- App: $App"
    Add-Content -Path $notesPath -Value "- Trace artifact: $resolvedTracePath"
    Add-Content -Path $notesPath -Value "- Summary artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Metric count: $($summary.metric_count)"
    Add-Content -Path $notesPath -Value "- Sample count: $($summary.sample_count)"
    Add-Content -Path $notesPath -Value "- Decision: GPU trace summarized; not compared"
}

Write-Output $OutputFile
