<#
.SYNOPSIS
Summarize a saved reconnect/connection event trace.
summarize-connection-trace.py is the Python twin.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$TracePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$RunDir = "",
    [string]$OutputFile = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function ConvertTo-EventClass {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    $normalized = $Value.ToLowerInvariant()
    if ($normalized -match "duplicate") {
        return "duplicate-message"
    }
    if ($normalized -match "dropped|drop") {
        return "dropped-message"
    }
    if ($normalized -match "reconnect[-_ ]?complete|reconnected|connected") {
        return "reconnect-complete"
    }
    if ($normalized -match "reconnect[-_ ]?attempt|reconnecting|retry") {
        return "reconnect-attempt"
    }
    if ($normalized -match "disconnect|connection[-_ ]?lost|closed") {
        return "disconnect"
    }
    if ($normalized -match "join") {
        return "join"
    }
    if ($normalized -match "error|fail|timeout") {
        return "error"
    }

    return "other"
}

$resolvedTracePath = (Resolve-Path $TracePath).Path
$extension = [System.IO.Path]::GetExtension($resolvedTracePath).ToLowerInvariant()
$rawEvents = if ($extension -in @(".ndjson", ".jsonl")) {
    @(Get-Content -Path $resolvedTracePath | Where-Object { -not [string]::IsNullOrWhiteSpace($_) } | ForEach-Object { $_ | ConvertFrom-Json })
} elseif ($extension -eq ".json") {
    $json = Get-Content -Raw $resolvedTracePath | ConvertFrom-Json
    if ($json -is [array]) { @($json) } else { @($json.events) }
} else {
    @(Import-Csv -Path $resolvedTracePath)
}

if ($rawEvents.Count -eq 0) {
    throw "Connection trace has no events: $resolvedTracePath"
}

$events = @(
    for ($index = 0; $index -lt $rawEvents.Count; $index++) {
        $traceEvent = $rawEvents[$index]
        $timestampValue = Get-FirstPropertyValue -Object $traceEvent -Names @("timestamp_ms", "time_ms", "elapsed_ms", "at_ms", "t_ms")
        $timestampMs = $null
        if ($null -ne $timestampValue) {
            $parsedTimestamp = 0.0
            if ([double]::TryParse(
                [string]$timestampValue,
                [System.Globalization.NumberStyles]::Float,
                [System.Globalization.CultureInfo]::InvariantCulture,
                [ref]$parsedTimestamp
            )) {
                $timestampMs = $parsedTimestamp
            }
        }

        $kindValue = Get-FirstPropertyValue -Object $traceEvent -Names @("kind", "event", "state", "connection_state", "message")
        $kindText = if ($null -ne $kindValue) { [string]$kindValue } else { "event" }
        $channel = Get-FirstPropertyValue -Object $traceEvent -Names @("channel", "room", "target")
        [pscustomobject][ordered]@{
            sequence = $index
            timestamp_ms = $timestampMs
            kind = $kindText
            class = ConvertTo-EventClass -Value $kindText
            channel = if ($null -ne $channel) { [string]$channel } else { "" }
        }
    }
)

$orderedEvents = @($events | Sort-Object @{ Expression = { if ($null -eq $_.timestamp_ms) { [double]::PositiveInfinity } else { [double]$_.timestamp_ms } } }, sequence)
$classes = @($orderedEvents | Select-Object -ExpandProperty class)
$channels = @($orderedEvents | Where-Object { -not [string]::IsNullOrWhiteSpace($_.channel) } | Select-Object -ExpandProperty channel -Unique)
$disconnects = @($orderedEvents | Where-Object { $_.class -eq "disconnect" -and $null -ne $_.timestamp_ms })
$reconnectCompletes = @($orderedEvents | Where-Object { $_.class -eq "reconnect-complete" -and $null -ne $_.timestamp_ms })
$durations = [System.Collections.Generic.List[double]]::new()

foreach ($disconnect in $disconnects) {
    $complete = $reconnectCompletes |
        Where-Object { [double]$_.timestamp_ms -ge [double]$disconnect.timestamp_ms } |
        Select-Object -First 1
    if ($null -ne $complete) {
        $durations.Add([double]$complete.timestamp_ms - [double]$disconnect.timestamp_ms) | Out-Null
    }
}

$sortedDurations = [double[]]@($durations | Sort-Object)
$durationTotal = 0.0
foreach ($duration in $durations) {
    $durationTotal += $duration
}

$firstTimestamp = @($orderedEvents | Where-Object { $null -ne $_.timestamp_ms } | Select-Object -First 1).timestamp_ms
$lastTimestamp = @($orderedEvents | Where-Object { $null -ne $_.timestamp_ms } | Select-Object -Last 1).timestamp_ms
$summary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    trace_path = $resolvedTracePath
    event_count = $orderedEvents.Count
    channel_count = $channels.Count
    first_timestamp_ms = $firstTimestamp
    last_timestamp_ms = $lastTimestamp
    disconnect_count = @($classes | Where-Object { $_ -eq "disconnect" }).Count
    reconnect_attempt_count = @($classes | Where-Object { $_ -eq "reconnect-attempt" }).Count
    reconnect_complete_count = @($classes | Where-Object { $_ -eq "reconnect-complete" }).Count
    join_count = @($classes | Where-Object { $_ -eq "join" }).Count
    error_count = @($classes | Where-Object { $_ -eq "error" }).Count
    duplicate_message_count = @($classes | Where-Object { $_ -eq "duplicate-message" }).Count
    dropped_message_count = @($classes | Where-Object { $_ -eq "dropped-message" }).Count
    reconnect_duration_count = $durations.Count
    reconnect_duration_average_ms = if ($durations.Count -gt 0) { [Math]::Round($durationTotal / $durations.Count, 6) } else { $null }
    reconnect_duration_p50_ms = Get-Percentile -Values $sortedDurations -Percentile 50
    reconnect_duration_p95_ms = Get-Percentile -Values $sortedDurations -Percentile 95
    reconnect_duration_max_ms = Get-Percentile -Values $sortedDurations -Percentile 100
    events_by_class = @(
        $classes |
            Group-Object |
            ForEach-Object {
                [ordered]@{
                    class = $_.Name
                    count = $_.Count
                }
            } |
            Sort-Object class
    )
    decision = "connection trace summarized; not an app comparison"
}

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $resolvedTracePath
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir ("{0}.connection-summary.json" -f (ConvertTo-Slug $App))
}

$summary | ConvertTo-Json -Depth 10 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $runSummary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = "connection trace summary"
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
$connectionTraces = Ensure-ObjectProperty -Object $rawArtifacts -Name "connection_traces"
$connectionSummaries = Ensure-ObjectProperty -Object $rawArtifacts -Name "connection_summaries"
$appKey = ConvertTo-Slug -Value $App
Set-JsonProperty -Object $connectionTraces -Name $appKey -Value $resolvedTracePath
Set-JsonProperty -Object $connectionSummaries -Name $appKey -Value $OutputFile
Set-JsonProperty -Object $rawArtifacts -Name "connection_trace" -Value $OutputFile
$connectionSummary = Ensure-ObjectProperty -Object $runSummary -Name "connection_summary"
Set-JsonProperty -Object $connectionSummary -Name $appKey -Value ([pscustomobject]$summary)
Set-JsonProperty -Object $runSummary -Name "decision" -Value "connection trace summarized; not compared"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Connection trace summary"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- App: $App"
    Add-Content -Path $notesPath -Value "- Trace artifact: $resolvedTracePath"
    Add-Content -Path $notesPath -Value "- Summary artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Event count: $($summary.event_count)"
    Add-Content -Path $notesPath -Value "- Reconnect duration count: $($summary.reconnect_duration_count)"
    Add-Content -Path $notesPath -Value "- Decision: connection trace summarized; not compared"
}

Write-Output $OutputFile
