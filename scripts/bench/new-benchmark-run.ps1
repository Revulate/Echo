[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$Scenario,

    [string]$OutputRoot = "",
    [string]$EchoPath = "",
    [string]$EchoBuildType = "",
    [string]$CacheState = "",
    [string]$NetworkState = "",
    [string]$FixtureName = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Invoke-GitValue {
    param(
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    $output = & git @Arguments 2>$null
    if ($LASTEXITCODE -ne 0) {
        return $null
    }

    return (($output | Out-String).Trim())
}

function Get-OptionalCimInstance {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ClassName
    )

    try {
        return Get-CimInstance -ClassName $ClassName
    } catch {
        return $null
    }
}

function ConvertTo-Slug {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value
    )

    $slug = $Value.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-"
    $slug = $slug.Trim("-")
    if ([string]::IsNullOrWhiteSpace($slug)) {
        return "scenario"
    }

    return $slug
}

$scriptRoot = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..\..")).Path

if ([string]::IsNullOrWhiteSpace($OutputRoot)) {
    $OutputRoot = Join-Path $repoRoot "benchmarks\results"
}

$resolvedOutputRoot = if (Test-Path $OutputRoot) {
    (Resolve-Path $OutputRoot).Path
} else {
    (New-Item -ItemType Directory -Force -Path $OutputRoot).FullName
}

$runId = [DateTime]::UtcNow.ToString("yyyyMMdd-HHmmss")
$scenarioSlug = ConvertTo-Slug -Value $Scenario
$runRoot = Join-Path $resolvedOutputRoot $runId
$scenarioDir = Join-Path $runRoot $scenarioSlug
$suffix = 1

while (Test-Path $scenarioDir) {
    $scenarioDir = Join-Path $runRoot ("{0}-{1:D2}" -f $scenarioSlug, $suffix)
    $suffix += 1
}

New-Item -ItemType Directory -Force -Path $scenarioDir | Out-Null

$commit = Invoke-GitValue -Arguments @("-C", $repoRoot, "rev-parse", "HEAD")
$shortCommit = Invoke-GitValue -Arguments @("-C", $repoRoot, "rev-parse", "--short", "HEAD")
$branch = Invoke-GitValue -Arguments @("-C", $repoRoot, "branch", "--show-current")
$dirtyOutput = Invoke-GitValue -Arguments @("-C", $repoRoot, "status", "--porcelain")
$dirtyPaths = @()
if (-not [string]::IsNullOrWhiteSpace($dirtyOutput)) {
    $dirtyPaths = $dirtyOutput -split "`r?`n"
}

$os = Get-OptionalCimInstance -ClassName "Win32_OperatingSystem" | Select-Object -First 1
$cpu = Get-OptionalCimInstance -ClassName "Win32_Processor" | Select-Object -First 1
$computer = Get-OptionalCimInstance -ClassName "Win32_ComputerSystem" | Select-Object -First 1
$gpus = @(Get-OptionalCimInstance -ClassName "Win32_VideoController" | Where-Object { $null -ne $_ } | ForEach-Object {
    [ordered]@{
        name = $_.Name
        adapter_ram_bytes = $_.AdapterRAM
        driver_version = $_.DriverVersion
        current_refresh_rate_hz = $_.CurrentRefreshRate
        video_mode = $_.VideoModeDescription
    }
})

$powerScheme = $null
try {
    $powerScheme = ((& powercfg /getactivescheme 2>$null) | Out-String).Trim()
} catch {
    $powerScheme = $null
}

$environment = [ordered]@{
    schema_version = 1
    run_id = $runId
    scenario = $Scenario
    scenario_slug = Split-Path -Leaf $scenarioDir
    captured_at_utc = [DateTime]::UtcNow.ToString("o")
    artifacts = [ordered]@{
        root = $scenarioDir
        environment = Join-Path $scenarioDir "environment.json"
        summary = Join-Path $scenarioDir "summary.json"
        notes = Join-Path $scenarioDir "notes.md"
        expected_echo_raw = Join-Path $scenarioDir "echo.raw.txt"
    }
    repo = [ordered]@{
        root = $repoRoot
        branch = $branch
        echo_commit = $commit
        echo_commit_short = $shortCommit
        dirty = $dirtyPaths.Count -gt 0
        dirty_paths = $dirtyPaths
    }
    apps = [ordered]@{
        echo = [ordered]@{
            path = $EchoPath
            build_type = $EchoBuildType
        }
    }
    run_context = [ordered]@{
        cache_state = $CacheState
        network_state = $NetworkState
        fixture_name = $FixtureName
    }
    machine = [ordered]@{
        computer_name = $env:COMPUTERNAME
        os = if ($null -ne $os) {
            [ordered]@{
                caption = $os.Caption
                version = $os.Version
                build_number = $os.BuildNumber
                architecture = $os.OSArchitecture
            }
        } else {
            $null
        }
        cpu = if ($null -ne $cpu) {
            [ordered]@{
                name = $cpu.Name
                cores = $cpu.NumberOfCores
                logical_processors = $cpu.NumberOfLogicalProcessors
                max_clock_mhz = $cpu.MaxClockSpeed
            }
        } else {
            $null
        }
        gpu = $gpus
        total_physical_memory_bytes = if ($null -ne $computer) { $computer.TotalPhysicalMemory } else { $null }
        power_scheme = $powerScheme
        powershell_version = $PSVersionTable.PSVersion.ToString()
    }
}

$summary = [ordered]@{
    schema_version = 1
    run_id = $runId
    scenario = $Scenario
    status = "initialized"
    measurements_recorded = $false
    raw_artifacts = [ordered]@{
        echo = $null
    }
    decision = "not measured"
}

$noteLines = @(
    "# Benchmark Run Notes",
    "",
    "- Run ID: $runId",
    "- Scenario: $Scenario",
    "- Status: initialized",
    "- Measurements recorded: false",
    ""
)

if (-not [string]::IsNullOrWhiteSpace($Notes)) {
    $noteLines += "## Notes"
    $noteLines += ""
    $noteLines += $Notes
    $noteLines += ""
}

$noteLines += "This helper only creates the artifact directory and captures environment metadata."
$noteLines += "Do not use this initialized run as performance evidence until raw measurements are added."

$environment | ConvertTo-Json -Depth 10 | Set-Content -Path (Join-Path $scenarioDir "environment.json") -Encoding utf8
$summary | ConvertTo-Json -Depth 5 | Set-Content -Path (Join-Path $scenarioDir "summary.json") -Encoding utf8
$noteLines | Set-Content -Path (Join-Path $scenarioDir "notes.md") -Encoding utf8

Write-Output $scenarioDir
