<#
.SYNOPSIS
Create deterministic chat replay fixture artifacts.
new-chat-replay-fixture.py is the Python twin.
#>
[CmdletBinding()]
param(
    [ValidateSet("normal", "high-volume", "burst", "reconnect")]
    [string]$ReplayScenario = "normal",

    [ValidateRange(0, 1000000)]
    [int]$MessageCount = 0,

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 60,

    [ValidateRange(1, 10000)]
    [int]$UserCount = 50,

    [ValidateRange(1, 2147483647)]
    [int]$Seed = 1337,

    [ValidateRange(1, 1000000)]
    [int]$BurstSize = 250,

    [ValidateRange(1, 600000)]
    [int]$BurstWindowMs = 1000,

    [ValidateNotNullOrEmpty()]
    [string]$Channel = "#echo-bench",

    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$BenchmarkScenario = "chat replay fixture"
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function Get-DefaultMessageCount {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Scenario
    )

    switch ($Scenario) {
        "normal" { return 600 }
        "high-volume" { return 6000 }
        "burst" { return 2000 }
        "reconnect" { return 1200 }
        default { return 600 }
    }
}

function Get-MessageTimestampMs {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Scenario,

        [Parameter(Mandatory = $true)]
        [int]$Index,

        [Parameter(Mandatory = $true)]
        [int]$TotalMessages,

        [Parameter(Mandatory = $true)]
        [int]$DurationMs,

        [Parameter(Mandatory = $true)]
        [int]$BurstLimit,

        [Parameter(Mandatory = $true)]
        [int]$BurstWindow
    )

    if ($TotalMessages -le 1) {
        return 0
    }

    if ($Scenario -eq "burst") {
        $activeBurstSize = [Math]::Min($BurstLimit, $TotalMessages)
        if ($Index -lt $activeBurstSize) {
            $burstDivisor = [Math]::Max(1, $activeBurstSize - 1)
            return [int][Math]::Floor(($Index / $burstDivisor) * $BurstWindow)
        }

        $remaining = $TotalMessages - $activeBurstSize
        $remainingDivisor = [Math]::Max(1, $remaining - 1)
        $remainingDuration = [Math]::Max(1, $DurationMs - $BurstWindow)
        return [int]($BurstWindow + [Math]::Floor((($Index - $activeBurstSize) / $remainingDivisor) * $remainingDuration))
    }

    $divisor = [Math]::Max(1, $TotalMessages - 1)
    return [int][Math]::Floor(($Index / $divisor) * $DurationMs)
}

function New-ControlEvent {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Kind,

        [Parameter(Mandatory = $true)]
        [int]$TimestampMs,

        [Parameter(Mandatory = $true)]
        [string]$Channel,

        [Parameter(Mandatory = $true)]
        [string]$Detail
    )

    return [pscustomobject][ordered]@{
        schema_version = 1
        sequence = 0
        kind = $Kind
        timestamp_ms = $TimestampMs
        channel = $Channel
        detail = $Detail
    }
}

function New-MessageEvent {
    param(
        [Parameter(Mandatory = $true)]
        [int]$Index,

        [Parameter(Mandatory = $true)]
        [int]$TimestampMs,

        [Parameter(Mandatory = $true)]
        [string]$Channel,

        [Parameter(Mandatory = $true)]
        [int]$UserCount,

        [Parameter(Mandatory = $true)]
        [System.Random]$Random
    )

    $userIndex = $Random.Next(0, $UserCount)
    $login = "benchuser{0:D4}" -f $userIndex
    $displayName = "BenchUser{0:D4}" -f $userIndex
    $userId = 900000 + $userIndex
    $messageId = "echo-bench-{0:D8}" -f $Index
    $colors = @("#1f77b4", "#2ca02c", "#d62728", "#9467bd", "#8c564b", "#e377c2", "#17becf")
    $color = $colors[$Random.Next(0, $colors.Count)]
    $variant = switch ($Index % 8) {
        0 { "plain" }
        1 { "mention" }
        2 { "twitch-emote" }
        3 { "third-party-emote" }
        4 { "badge-heavy" }
        5 { "link" }
        6 { "long-message" }
        default { "reply-like" }
    }

    $messageText = switch ($variant) {
        "plain" { "deterministic chat message $Index" }
        "mention" { "@BenchUser0001 deterministic mention payload $Index" }
        "twitch-emote" { "Kappa deterministic emote payload $Index" }
        "third-party-emote" { "OMEGALUL 7TVWide FFZCool payload $Index" }
        "badge-heavy" { "subscriber badge-heavy message $Index" }
        "link" { "link payload https://example.invalid/echo-bench/$Index" }
        "long-message" { "long payload $Index " + ("abcdefghijklmnopqrstuvwxyz " * 8).Trim() }
        default { "reply-style payload $Index responding to echo-bench-{0:D8}" -f [Math]::Max(0, $Index - 1) }
    }

    $badges = if ($variant -eq "badge-heavy") { "moderator/1,subscriber/24,founder/0" } else { "subscriber/12" }
    $emotes = if ($variant -eq "twitch-emote") { "25:0-4" } else { "" }
    $channelName = $Channel.TrimStart("#")
    $ircLine = "@badge-info=;badges=$badges;color=$color;display-name=$displayName;emotes=$emotes;flags=;id=$messageId;mod=0;room-id=123456;tmi-sent-ts=$TimestampMs;turbo=0;user-id=$userId;user-type= :$login!$login@$login.tmi.twitch.tv PRIVMSG $Channel :$messageText"

    return [pscustomobject][ordered]@{
        schema_version = 1
        sequence = 0
        kind = "message"
        timestamp_ms = $TimestampMs
        channel = $Channel
        source = "twitch-irc-fixture"
        login = $login
        display_name = $displayName
        message = [ordered]@{
            id = $messageId
            variant = $variant
            text = $messageText
        }
        tags = [ordered]@{
            badges = $badges
            color = $color
            display_name = $displayName
            emotes = $emotes
            room_id = "123456"
            tmi_sent_ts = $TimestampMs
            user_id = [string]$userId
        }
        irc = [ordered]@{
            channel = $channelName
            line = $ircLine
        }
    }
}

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-BenchRunDirectory -RepoRoot $repoRoot -RunDir $RunDir -Metadata @{
    Scenario = $BenchmarkScenario
    OutputRoot = $OutputRoot
    FixtureName = "chat-replay-$ReplayScenario"
    Notes = "Deterministic chat replay fixture for '$ReplayScenario'. This is input data, not performance evidence."
}
$normalizedChannel = if ($Channel.StartsWith("#")) { $Channel } else { "#$Channel" }
$durationMs = $DurationSeconds * 1000
$resolvedMessageCount = if ($MessageCount -gt 0) { $MessageCount } else { Get-DefaultMessageCount -Scenario $ReplayScenario }
$random = [System.Random]::new($Seed)
$events = New-Object System.Collections.Generic.List[object]

for ($index = 0; $index -lt $resolvedMessageCount; $index++) {
    $timestampMs = Get-MessageTimestampMs `
        -Scenario $ReplayScenario `
        -Index $index `
        -TotalMessages $resolvedMessageCount `
        -DurationMs $durationMs `
        -BurstLimit $BurstSize `
        -BurstWindow $BurstWindowMs
    $events.Add((New-MessageEvent -Index $index -TimestampMs $timestampMs -Channel $normalizedChannel -UserCount $UserCount -Random $random)) | Out-Null
}

if ($ReplayScenario -eq "reconnect") {
    $disconnectAt = [int][Math]::Floor($durationMs * 0.40)
    $attemptAt = [int][Math]::Min($durationMs, $disconnectAt + 1000)
    $completeAt = [int][Math]::Min($durationMs, $disconnectAt + 2500)
    $events.Add((New-ControlEvent -Kind "disconnect" -TimestampMs $disconnectAt -Channel $normalizedChannel -Detail "simulated transport disconnect")) | Out-Null
    $events.Add((New-ControlEvent -Kind "reconnect-attempt" -TimestampMs $attemptAt -Channel $normalizedChannel -Detail "simulated reconnect attempt")) | Out-Null
    $events.Add((New-ControlEvent -Kind "reconnect-complete" -TimestampMs $completeAt -Channel $normalizedChannel -Detail "simulated reconnect completion")) | Out-Null
}

$orderedEvents = @($events | Sort-Object timestamp_ms, kind)
for ($sequence = 0; $sequence -lt $orderedEvents.Count; $sequence++) {
    $orderedEvents[$sequence].sequence = $sequence
}

$slug = ConvertTo-Slug -Value "chat-replay-$ReplayScenario"
$replayPath = Join-Path $scenarioDir "$slug.ndjson"
$summaryPath = Join-Path $scenarioDir "$slug.summary.json"
$manifestPath = Join-Path $scenarioDir "$slug.manifest.json"

$orderedEvents | ForEach-Object { $_ | ConvertTo-Json -Depth 10 -Compress } | Set-Content -Path $replayPath -Encoding utf8

$messageEvents = @($orderedEvents | Where-Object { $_.kind -eq "message" })
$controlEvents = @($orderedEvents | Where-Object { $_.kind -ne "message" })
$variantSummary = @(
    $messageEvents |
        Group-Object { $_.message.variant } |
        ForEach-Object {
            [ordered]@{
                variant = $_.Name
                count = $_.Count
            }
        } |
        Sort-Object variant
)

$fixtureSummary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    replay_scenario = $ReplayScenario
    channel = $normalizedChannel
    seed = $Seed
    duration_seconds = $DurationSeconds
    requested_message_count = $MessageCount
    message_count = $messageEvents.Count
    control_event_count = $controlEvents.Count
    total_event_count = $orderedEvents.Count
    user_count = $UserCount
    first_timestamp_ms = if ($orderedEvents.Count -gt 0) { $orderedEvents[0].timestamp_ms } else { $null }
    last_timestamp_ms = if ($orderedEvents.Count -gt 0) { $orderedEvents[$orderedEvents.Count - 1].timestamp_ms } else { $null }
    average_message_rate_per_second = [Math]::Round($messageEvents.Count / [double]$DurationSeconds, 3)
    burst_size = if ($ReplayScenario -eq "burst") { [Math]::Min($BurstSize, $messageEvents.Count) } else { $null }
    burst_window_ms = if ($ReplayScenario -eq "burst") { $BurstWindowMs } else { $null }
    variants = $variantSummary
    decision = "chat replay fixture recorded; not a benchmark comparison"
}
$fixtureSummary | ConvertTo-Json -Depth 10 | Set-Content -Path $summaryPath -Encoding utf8

$manifest = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    replay_path = $replayPath
    summary_path = $summaryPath
    replay_scenario = $ReplayScenario
    channel = $normalizedChannel
    seed = $Seed
    decision = "chat replay fixture recorded; not measured"
}
$manifest | ConvertTo-Json -Depth 5 | Set-Content -Path $manifestPath -Encoding utf8

$runSummaryPath = Join-Path $scenarioDir "summary.json"
if (Test-Path $runSummaryPath) {
    $runSummary = Get-Content -Raw $runSummaryPath | ConvertFrom-Json
} else {
    $runSummary = [pscustomobject]@{
        schema_version = 1
        scenario = $BenchmarkScenario
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $runSummary -Name "raw_artifacts"
Set-JsonProperty -Object $rawArtifacts -Name "chat_replay" -Value $replayPath
Set-JsonProperty -Object $rawArtifacts -Name "chat_replay_manifest" -Value $manifestPath
Set-JsonProperty -Object $rawArtifacts -Name "chat_replay_summary" -Value $summaryPath
Set-JsonProperty -Object $runSummary -Name "chat_replay" -Value ([pscustomobject]$fixtureSummary)
Set-JsonProperty -Object $runSummary -Name "decision" -Value "chat replay fixture recorded; not measured"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $runSummaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Chat replay fixture"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Replay artifact: $replayPath"
    Add-Content -Path $notesPath -Value "- Manifest artifact: $manifestPath"
    Add-Content -Path $notesPath -Value "- Scenario: $ReplayScenario"
    Add-Content -Path $notesPath -Value "- Messages: $($messageEvents.Count)"
    Add-Content -Path $notesPath -Value "- Control events: $($controlEvents.Count)"
    Add-Content -Path $notesPath -Value "- Decision: chat replay fixture recorded; not measured"
}

Write-Output $manifestPath
