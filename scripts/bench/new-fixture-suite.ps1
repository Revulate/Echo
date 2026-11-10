<#
.SYNOPSIS
Create and validate a deterministic chat/render fixture suite.
new-fixture-suite.py is the Python twin.
#>
[CmdletBinding()]
param(
    [ValidateSet("normal", "high-volume", "burst", "reconnect")]
    [string[]]$ChatScenarios = @("normal", "high-volume", "burst", "reconnect"),

    [ValidateSet("static-emotes", "animated-emotes", "mixed-emotes", "large-scrollback", "rapid-scrolling", "multi-split")]
    [string[]]$RenderKinds = @("static-emotes", "animated-emotes", "mixed-emotes", "large-scrollback", "rapid-scrolling", "multi-split"),

    [ValidateRange(0, 1000000)]
    [int]$ChatMessageCount = 0,

    [ValidateRange(0, 1000000)]
    [int]$RenderMessageCount = 0,

    [ValidateRange(1, 2147483647)]
    [int]$Seed = 9001,

    [string]$OutputRoot = "",
    [string]$BenchmarkScenario = "benchmark fixture suite"
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function Invoke-CheckedScript {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ScriptPath,

        [Parameter(Mandatory = $true)]
        [hashtable]$Arguments
    )

    $output = & $ScriptPath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "Script failed: $ScriptPath"
    }

    $path = (($output | Select-Object -Last 1) | Out-String).Trim()
    if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path $path)) {
        throw "Script did not return an existing artifact path: $ScriptPath"
    }

    return (Resolve-Path $path).Path
}

function Read-ValidationResult {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ValidationPath
    )

    $json = Get-Content -Raw $ValidationPath | ConvertFrom-Json
    $passed = Get-JsonProperty -Object $json -Name "passed"
    return [bool]$passed
}

$repoRoot = Resolve-RepoRoot
$scriptRoot = Join-Path $repoRoot "scripts\bench"
$runInitializer = Join-Path $scriptRoot "new-benchmark-run.ps1"
$chatFixtureScript = Join-Path $scriptRoot "new-chat-replay-fixture.ps1"
$renderFixtureScript = Join-Path $scriptRoot "new-render-fixture.ps1"
$validatorScript = Join-Path $scriptRoot "validate-benchmark-run.ps1"

foreach ($requiredScript in @($runInitializer, $chatFixtureScript, $renderFixtureScript, $validatorScript)) {
    if (-not (Test-Path $requiredScript)) {
        throw "Required benchmark script not found: $requiredScript"
    }
}

$suiteArgs = @{
    Scenario = $BenchmarkScenario
    FixtureName = "fixture-suite"
    Notes = "Deterministic fixture suite for chat replay and render benchmark inputs. This is not performance evidence."
}
if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) {
    $suiteArgs.OutputRoot = $OutputRoot
}

$suiteDir = Invoke-CheckedScript -ScriptPath $runInitializer -Arguments $suiteArgs
$fixtureOutputRoot = Join-Path $suiteDir "fixtures"
New-Item -ItemType Directory -Force -Path $fixtureOutputRoot | Out-Null

$chatFixtures = @()
$renderFixtures = @()
$suitePassed = $true
$fixtureIndex = 0

foreach ($scenario in $ChatScenarios) {
    $fixtureSeed = $Seed + $fixtureIndex
    $fixtureIndex += 1
    $fixtureArgs = @{
        ReplayScenario = $scenario
        OutputRoot = $fixtureOutputRoot
        BenchmarkScenario = "chat replay fixture suite $scenario"
        Seed = $fixtureSeed
    }
    if ($ChatMessageCount -gt 0) {
        $fixtureArgs.MessageCount = $ChatMessageCount
    }

    $manifestPath = Invoke-CheckedScript -ScriptPath $chatFixtureScript -Arguments $fixtureArgs
    $fixtureRunDir = Split-Path -Parent $manifestPath
    $validationArgs = @{
        RunDir = $fixtureRunDir
        RequireChatReplay = $true
        MinimumSamples = if ($ChatMessageCount -gt 0) { $ChatMessageCount } else { 1 }
    }
    $validationPath = Invoke-CheckedScript -ScriptPath $validatorScript -Arguments $validationArgs
    $validationPassed = Read-ValidationResult -ValidationPath $validationPath
    $suitePassed = $suitePassed -and $validationPassed

    $chatFixtures += [ordered]@{
        scenario = $scenario
        seed = $fixtureSeed
        run_dir = $fixtureRunDir
        manifest_path = $manifestPath
        validation_path = $validationPath
        validation_passed = $validationPassed
    }
}

foreach ($kind in $RenderKinds) {
    $fixtureSeed = $Seed + $fixtureIndex
    $fixtureIndex += 1
    $fixtureArgs = @{
        FixtureKind = $kind
        OutputRoot = $fixtureOutputRoot
        BenchmarkScenario = "render fixture suite $kind"
        Seed = $fixtureSeed
    }
    if ($RenderMessageCount -gt 0) {
        $fixtureArgs.MessageCount = $RenderMessageCount
    }

    $manifestPath = Invoke-CheckedScript -ScriptPath $renderFixtureScript -Arguments $fixtureArgs
    $fixtureRunDir = Split-Path -Parent $manifestPath
    $validationArgs = @{
        RunDir = $fixtureRunDir
        RequireRenderFixture = $true
        MinimumSamples = if ($RenderMessageCount -gt 0) { $RenderMessageCount } else { 1 }
    }
    $validationPath = Invoke-CheckedScript -ScriptPath $validatorScript -Arguments $validationArgs
    $validationPassed = Read-ValidationResult -ValidationPath $validationPath
    $suitePassed = $suitePassed -and $validationPassed

    $renderFixtures += [ordered]@{
        kind = $kind
        seed = $fixtureSeed
        run_dir = $fixtureRunDir
        manifest_path = $manifestPath
        validation_path = $validationPath
        validation_passed = $validationPassed
    }
}

$suiteManifestPath = Join-Path $suiteDir "fixture-suite.manifest.json"
$suiteManifest = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    suite_dir = $suiteDir
    fixture_output_root = $fixtureOutputRoot
    seed = $Seed
    chat_message_count_override = $ChatMessageCount
    render_message_count_override = $RenderMessageCount
    chat_fixtures = $chatFixtures
    render_fixtures = $renderFixtures
    validation_passed = $suitePassed
    decision = "fixture suite recorded; not measured"
}
$suiteManifest | ConvertTo-Json -Depth 10 | Set-Content -Path $suiteManifestPath -Encoding utf8

$summaryPath = Join-Path $suiteDir "summary.json"
$summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
$rawArtifacts = Ensure-ObjectProperty -Object $summary -Name "raw_artifacts"
Set-JsonProperty -Object $rawArtifacts -Name "fixture_suite_manifest" -Value $suiteManifestPath
Set-JsonProperty -Object $summary -Name "fixture_suite" -Value ([pscustomobject][ordered]@{
    chat_fixture_count = $chatFixtures.Count
    render_fixture_count = $renderFixtures.Count
    validation_passed = $suitePassed
    manifest_path = $suiteManifestPath
})
Set-JsonProperty -Object $summary -Name "decision" -Value "fixture suite recorded; not measured"
$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $suiteDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Fixture suite"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Suite manifest: $suiteManifestPath"
    Add-Content -Path $notesPath -Value "- Chat fixtures: $($chatFixtures.Count)"
    Add-Content -Path $notesPath -Value "- Render fixtures: $($renderFixtures.Count)"
    Add-Content -Path $notesPath -Value "- Child validations passed: $suitePassed"
    Add-Content -Path $notesPath -Value "- Decision: fixture suite recorded; not measured"
}

Write-Output $suiteManifestPath
