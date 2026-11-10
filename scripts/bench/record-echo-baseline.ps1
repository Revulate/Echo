<#
.SYNOPSIS
Record Echo executable identity metadata.

Records a baseline identity only; it is not a benchmark measurement.
record-echo-baseline.py is the Python twin.
#>
[CmdletBinding()]
param(
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$Scenario = "echo baseline",

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$ExecutablePath,

    [string[]]$LaunchArgument = @(),
    [string]$VersionLabel = "",
    [string]$Commit = "",
    [string]$BuildType = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-BenchRunDirectory -RepoRoot $repoRoot -RunDir $RunDir -Metadata @{
    Scenario = $Scenario
    OutputRoot = $OutputRoot
    EchoPath = $ExecutablePath
    EchoBuildType = $BuildType
    Notes = $Notes
}
$resolvedExecutable = Resolve-ExecutablePath -Path $ExecutablePath -Label "ExecutablePath"
$fileItem = Get-Item -LiteralPath $resolvedExecutable
$fileHash = Get-FileHash -Algorithm SHA256 -LiteralPath $resolvedExecutable
$versionInfo = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($resolvedExecutable)

$repoCommit = if ([string]::IsNullOrWhiteSpace($Commit)) {
    Invoke-GitValue -RepoRoot $repoRoot -Arguments @("rev-parse", "HEAD")
} else {
    $Commit
}
$repoShortCommit = Invoke-GitValue -RepoRoot $repoRoot -Arguments @("rev-parse", "--short", "HEAD")
$repoBranch = Invoke-GitValue -RepoRoot $repoRoot -Arguments @("branch", "--show-current")
$dirtyOutput = Invoke-GitValue -RepoRoot $repoRoot -Arguments @("status", "--porcelain")
$dirtyPaths = @()
if (-not [string]::IsNullOrWhiteSpace($dirtyOutput)) {
    $dirtyPaths = $dirtyOutput -split "`r?`n"
}

$baseline = [ordered]@{
    schema_version = 1
    recorded_at_utc = [DateTime]::UtcNow.ToString("o")
    app = "echo"
    executable_path = $resolvedExecutable
    executable_name = $fileItem.Name
    launch_argument = $LaunchArgument
    version_label = $VersionLabel
    build_type = $BuildType
    repo = [ordered]@{
        root = $repoRoot
        branch = $repoBranch
        commit = $repoCommit
        short_commit = $repoShortCommit
        dirty = $dirtyPaths.Count -gt 0
        dirty_paths = $dirtyPaths
    }
    file = [ordered]@{
        length_bytes = $fileItem.Length
        last_write_time_utc = $fileItem.LastWriteTimeUtc.ToString("o")
        sha256 = $fileHash.Hash
    }
    version_info = [ordered]@{
        file_description = $versionInfo.FileDescription
        file_version = $versionInfo.FileVersion
        product_name = $versionInfo.ProductName
        product_version = $versionInfo.ProductVersion
        company_name = $versionInfo.CompanyName
        original_filename = $versionInfo.OriginalFilename
    }
    decision = "baseline recorded; not a benchmark measurement"
}

$baselinePath = Join-Path $scenarioDir "echo-baseline.json"
$baseline | ConvertTo-Json -Depth 8 | Set-Content -Path $baselinePath -Encoding utf8

$summaryPath = Join-Path $scenarioDir "summary.json"
if (Test-Path $summaryPath) {
    $summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $summary = [pscustomobject]@{
        schema_version = 1
        scenario = $Scenario
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $summary -Name "raw_artifacts"
Set-JsonProperty -Object $rawArtifacts -Name "echo_baseline" -Value $baselinePath
Set-JsonProperty -Object $summary -Name "echo_baseline" -Value ([pscustomobject]$baseline)
if (-not [bool](Get-JsonProperty -Object $summary -Name "measurements_recorded")) {
    Set-JsonProperty -Object $summary -Name "measurements_recorded" -Value $false
}
Set-JsonProperty -Object $summary -Name "decision" -Value "baseline recorded; not measured"
$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$environmentPath = Join-Path $scenarioDir "environment.json"
if (Test-Path $environmentPath) {
    $environment = Get-Content -Raw $environmentPath | ConvertFrom-Json
    $apps = Ensure-ObjectProperty -Object $environment -Name "apps"
    $echo = Ensure-ObjectProperty -Object $apps -Name "echo"
    Set-JsonProperty -Object $echo -Name "path" -Value $resolvedExecutable
    Set-JsonProperty -Object $echo -Name "build_type" -Value $BuildType
    Set-JsonProperty -Object $echo -Name "version" -Value $VersionLabel
    Set-JsonProperty -Object $echo -Name "sha256" -Value $fileHash.Hash
    $environment | ConvertTo-Json -Depth 12 | Set-Content -Path $environmentPath -Encoding utf8
}

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Echo baseline"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Baseline artifact: $baselinePath"
    Add-Content -Path $notesPath -Value "- Executable: $resolvedExecutable"
    Add-Content -Path $notesPath -Value "- Build type: $BuildType"
    Add-Content -Path $notesPath -Value "- Commit: $repoCommit"
    Add-Content -Path $notesPath -Value "- Dirty: $($dirtyPaths.Count -gt 0)"
    Add-Content -Path $notesPath -Value "- SHA256: $($fileHash.Hash)"
    Add-Content -Path $notesPath -Value "- Decision: baseline recorded; not a benchmark measurement"
}

Write-Output $baselinePath
