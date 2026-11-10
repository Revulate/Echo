<#
.SYNOPSIS
Repeat measure-startup.ps1 for a number of observations and summarize the
startup milestones (median and p95).
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$Scenario,
    [Parameter(Mandatory = $true)][string]$RunDir,
    [Parameter(Mandatory = $true)][string]$EchoLaunchPath,
    [string[]]$EchoLaunchArgument = @(),
    [string]$EchoLaunchWorkingDirectory = "",
    [string]$EchoAppLogPath = "",
    [ValidateSet("process-start", "main-window", "input-idle", "app-log")][string]$ReadyMode = "main-window",
    [string]$StartupMilestone = "ready",
    [ValidateRange(10, 1000)][int]$ObservationCount = 10,
    [ValidateRange(1, 300)][int]$ReadyTimeoutSeconds = 30,
    [ValidateRange(1, 86400)][int]$SampleDurationSeconds = 1,
    [ValidateRange(100, 60000)][int]$IntervalMilliseconds = 1000,
    [switch]$StopLaunchedProcesses
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function Invoke-StartupObservation {
    param([string]$App, [string]$Path, [string[]]$Arguments, [string]$WorkingDirectory, [string]$LogPath, [int]$Index)
    $script = Join-Path (Split-Path -Parent $PSCommandPath) "measure-startup.ps1"
    $params = @{
        App = $App; Scenario = $Scenario; RunDir = $RunDir; LaunchPath = $Path
        ReadyMode = $ReadyMode; ReadyTimeoutSeconds = $ReadyTimeoutSeconds
        SampleDurationSeconds = $SampleDurationSeconds; IntervalMilliseconds = $IntervalMilliseconds
        ObservationId = ("{0:D3}" -f $Index)
    }
    if ($Arguments.Count -gt 0) { $params.LaunchArgument = $Arguments }
    if (-not [string]::IsNullOrWhiteSpace($WorkingDirectory)) { $params.LaunchWorkingDirectory = $WorkingDirectory }
    if (-not [string]::IsNullOrWhiteSpace($LogPath)) { $params.AppLogPath = $LogPath }
    if (-not [string]::IsNullOrWhiteSpace($StartupMilestone)) { $params.StartupMilestone = $StartupMilestone }
    if ($StopLaunchedProcesses) { $params.StopLaunchedProcess = $true }
    $raw = (& $script @params | Select-Object -Last 1 | Out-String).Trim()
    if (-not (Test-Path $raw)) { throw "Startup observation failed for $App #$Index." }
    return Get-Content -Raw -LiteralPath $raw | ConvertFrom-Json
}

$resolvedRunDir = (New-Item -ItemType Directory -Force -Path $RunDir).FullName
$observations = [System.Collections.Generic.List[object]]::new()
for ($index = 1; $index -le $ObservationCount; $index++) {
    $observations.Add((Invoke-StartupObservation "echo" $EchoLaunchPath $EchoLaunchArgument $EchoLaunchWorkingDirectory $EchoAppLogPath $index))
}

$summaryRows = [ordered]@{}
foreach ($field in @("process_created_elapsed_ms", "ready_elapsed_ms", "window_elapsed_ms", "input_idle_elapsed_ms")) {
    $values = @($observations | ForEach-Object { $value = $_.$field; if ($null -ne $value) { [double]$value } })
    $summaryRows[$field] = [ordered]@{ count = $values.Count; median_ms = Get-Percentile $values 50; p95_ms = Get-Percentile $values 95 }
}
foreach ($milestone in @(
    "process_created", "prewarm_joined", "prewarm_detached", "gpui_http_client_ready", "user_colors_loaded",
    "settings_loaded", "backend_initialized", "caches_initialized", "theme_initialized",
    "main_window_opening", "main_window_created", "first_root_render", "layout_restored",
    "first_chat_view_usable",
    "first_text_paint", "first_visible_emote_paint", "visible_emotes_complete",
    "startup_network_fallback_complete"
)) {
    $values = @($observations | ForEach-Object {
        $property = $_.app_owned_milestones_ms.PSObject.Properties[$milestone]
        if ($null -ne $property -and $null -ne $property.Value) { [double]$property.Value }
    })
    $summaryRows["app_owned_$milestone"] = [ordered]@{
        count = $values.Count
        median_ms = Get-Percentile $values 50
        p95_ms = Get-Percentile $values 95
        raw_samples_ms = $values
    }
}
$summaries = [ordered]@{ echo = $summaryRows }

$output = Join-Path $resolvedRunDir "startup-campaign.json"
[ordered]@{
    schema_version = 1; created_at_utc = [DateTime]::UtcNow.ToString("o"); scenario = $Scenario
    observation_count_per_app = $ObservationCount
    ready_mode = $ReadyMode; startup_milestone = $StartupMilestone
    observations = @($observations); summaries = $summaries
    note = "Repeated startup observations with raw per-observation artifacts; median and p95 are descriptive."
} | ConvertTo-Json -Depth 12 | Set-Content -LiteralPath $output -Encoding utf8
Write-Output $output
