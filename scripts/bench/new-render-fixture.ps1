[CmdletBinding()]
param(
    [ValidateSet("static-emotes", "animated-emotes", "mixed-emotes", "large-scrollback", "rapid-scrolling", "multi-split")]
    [string]$FixtureKind = "static-emotes",

    [ValidateRange(0, 1000000)]
    [int]$MessageCount = 0,

    [ValidateRange(1, 32)]
    [int]$SplitCount = 4,

    [ValidateRange(320, 16384)]
    [int]$ViewportWidth = 1280,

    [ValidateRange(240, 16384)]
    [int]$ViewportHeight = 720,

    [ValidateRange(1, 2147483647)]
    [int]$Seed = 4242,

    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$BenchmarkScenario = "render fixture"
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
    $scriptRoot = Split-Path -Parent $PSCommandPath
    return (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
}

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

function Initialize-RunDirectory {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepoRoot
    )

    if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
        return (New-Item -ItemType Directory -Force -Path $RunDir).FullName
    }

    $initializer = Join-Path $RepoRoot "scripts\bench\new-benchmark-run.ps1"
    if (-not (Test-Path $initializer)) {
        throw "Benchmark run initializer not found: $initializer"
    }

    $args = @{
        Scenario = $BenchmarkScenario
        FixtureName = "render-$FixtureKind"
        Notes = "Deterministic render fixture for '$FixtureKind'. This is input data, not frame-time evidence."
    }
    if (-not [string]::IsNullOrWhiteSpace($OutputRoot)) {
        $args.OutputRoot = $OutputRoot
    }

    $output = & $initializer @args
    $path = (($output | Select-Object -Last 1) | Out-String).Trim()
    if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path $path)) {
        throw "Benchmark run initializer did not return an existing path."
    }

    return (Resolve-Path $path).Path
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

function Get-DefaultMessageCount {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Kind
    )

    switch ($Kind) {
        "static-emotes" { return 500 }
        "animated-emotes" { return 500 }
        "mixed-emotes" { return 750 }
        "large-scrollback" { return 20000 }
        "rapid-scrolling" { return 8000 }
        "multi-split" { return 2000 }
        default { return 500 }
    }
}

function New-EmoteToken {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Code,

        [Parameter(Mandatory = $true)]
        [string]$Provider,

        [Parameter(Mandatory = $true)]
        [bool]$Animated,

        [bool]$Wide = $false,
        [int]$Scale = 2
    )

    return [ordered]@{
        kind = "emote"
        code = $Code
        provider = $Provider
        animated = $Animated
        wide = $Wide
        preferred_scale = $Scale
    }
}

function Get-EmotePool {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Kind
    )

    $static = @(
        (New-EmoteToken -Code "Kappa" -Provider "twitch" -Animated $false -Scale 4),
        (New-EmoteToken -Code "PogChamp" -Provider "twitch" -Animated $false -Scale 4),
        (New-EmoteToken -Code "LUL" -Provider "twitch" -Animated $false -Scale 4),
        (New-EmoteToken -Code "BibleThump" -Provider "twitch" -Animated $false -Scale 4)
    )
    $animated = @(
        (New-EmoteToken -Code "peepoDance" -Provider "7tv" -Animated $true),
        (New-EmoteToken -Code "catJAM" -Provider "bttv" -Animated $true),
        (New-EmoteToken -Code "HYPERCLAP" -Provider "ffz" -Animated $true),
        (New-EmoteToken -Code "PETTHEMODS" -Provider "7tv" -Animated $true)
    )
    $mixed = @(
        (New-EmoteToken -Code "Kappa" -Provider "twitch" -Animated $false -Scale 4),
        (New-EmoteToken -Code "OMEGALUL" -Provider "bttv" -Animated $false),
        (New-EmoteToken -Code "FeelsGoodMan" -Provider "ffz" -Animated $false),
        (New-EmoteToken -Code "WidePeepoHappy" -Provider "7tv" -Animated $false -Wide $true),
        (New-EmoteToken -Code "peepoDance" -Provider "7tv" -Animated $true),
        (New-EmoteToken -Code "catJAM" -Provider "bttv" -Animated $true)
    )

    switch ($Kind) {
        "static-emotes" { return $static }
        "animated-emotes" { return $animated }
        default { return $mixed }
    }
}

function New-RenderMessage {
    param(
        [Parameter(Mandatory = $true)]
        [int]$Index,

        [Parameter(Mandatory = $true)]
        [string]$Kind,

        [Parameter(Mandatory = $true)]
        [int]$SplitCount,

        [Parameter(Mandatory = $true)]
        [object[]]$EmotePool,

        [Parameter(Mandatory = $true)]
        [System.Random]$Random
    )

    $channelIndex = if ($Kind -eq "multi-split") { $Index % $SplitCount } else { 0 }
    $channel = "#echo-render-{0:D2}" -f $channelIndex
    $messageId = "render-fixture-{0:D8}" -f $Index
    $login = "renderuser{0:D4}" -f $Random.Next(0, 250)
    $displayName = "RenderUser{0:D4}" -f $Random.Next(0, 250)
    $timestampMs = $Index * 100
    $emoteA = $EmotePool[$Index % $EmotePool.Count]
    $emoteB = $EmotePool[($Index + 3) % $EmotePool.Count]
    $longTail = if ($Kind -in @("large-scrollback", "rapid-scrolling")) {
        " " + ("scrollback payload " * (($Index % 5) + 1)).Trim()
    } else {
        ""
    }

    $tokens = @(
        [ordered]@{
            kind = "text"
            value = "message $Index from $displayName"
        },
        $emoteA,
        [ordered]@{
            kind = "text"
            value = "render fixture$longTail"
        }
    )

    if ($Kind -in @("animated-emotes", "mixed-emotes", "static-emotes")) {
        $tokens += $emoteB
        $tokens += [ordered]@{
            kind = "mention"
            value = "@RenderUser0001"
        }
    }

    if ($Index % 17 -eq 0) {
        $tokens += [ordered]@{
            kind = "link"
            value = "https://example.invalid/render-fixture/$Index"
        }
    }

    return [ordered]@{
        schema_version = 1
        sequence = $Index
        kind = "render-message"
        fixture_kind = $Kind
        timestamp_ms = $timestampMs
        channel = $channel
        split_index = $channelIndex
        id = $messageId
        login = $login
        display_name = $displayName
        badges = if ($Index % 11 -eq 0) { @("moderator/1", "subscriber/24") } else { @("subscriber/12") }
        color = @("#1f77b4", "#2ca02c", "#d62728", "#9467bd", "#17becf")[$Index % 5]
        tokens = $tokens
        expected = [ordered]@{
            contains_animated_emote = @($tokens | Where-Object { $_.kind -eq "emote" -and $_.animated }).Count -gt 0
            contains_wide_emote = @($tokens | Where-Object { $_.kind -eq "emote" -and $_.wide }).Count -gt 0
            contains_link = @($tokens | Where-Object { $_.kind -eq "link" }).Count -gt 0
        }
    }
}

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-RunDirectory -RepoRoot $repoRoot
$resolvedMessageCount = if ($MessageCount -gt 0) { $MessageCount } else { Get-DefaultMessageCount -Kind $FixtureKind }
$random = [System.Random]::new($Seed)
$emotePool = @(Get-EmotePool -Kind $FixtureKind)
$slug = ConvertTo-Slug -Value "render-$FixtureKind"
$messagesPath = Join-Path $scenarioDir "$slug.messages.ndjson"
$manifestPath = Join-Path $scenarioDir "$slug.manifest.json"
$summaryPath = Join-Path $scenarioDir "$slug.summary.json"

$messages = for ($index = 0; $index -lt $resolvedMessageCount; $index++) {
    New-RenderMessage -Index $index -Kind $FixtureKind -SplitCount $SplitCount -EmotePool $emotePool -Random $random
}
$messages | ForEach-Object { $_ | ConvertTo-Json -Depth 10 -Compress } | Set-Content -Path $messagesPath -Encoding utf8

$channelCount = if ($FixtureKind -eq "multi-split") { $SplitCount } else { 1 }
$layout = [ordered]@{
    split_count = $channelCount
    viewport = [ordered]@{
        width = $ViewportWidth
        height = $ViewportHeight
    }
    channels = @(
        for ($split = 0; $split -lt $channelCount; $split++) {
            [ordered]@{
                split_index = $split
                channel = "#echo-render-{0:D2}" -f $split
            }
        }
    )
}
$actions = @()
if ($FixtureKind -eq "rapid-scrolling") {
    $actions = @(
        [ordered]@{ at_ms = 0; kind = "scroll-to-bottom"; target = "all" },
        [ordered]@{ at_ms = 250; kind = "wheel"; delta_y = -2400; repeat = 20 },
        [ordered]@{ at_ms = 1250; kind = "wheel"; delta_y = 2400; repeat = 20 },
        [ordered]@{ at_ms = 2250; kind = "scroll-to-bottom"; target = "all" }
    )
}

$providerSummary = @(
    $messages |
        ForEach-Object { $_.tokens } |
        Where-Object { $_.kind -eq "emote" } |
        Group-Object provider |
        ForEach-Object {
            [ordered]@{
                provider = $_.Name
                emote_token_count = $_.Count
                animated_count = @($_.Group | Where-Object { $_.animated }).Count
                wide_count = @($_.Group | Where-Object { $_.wide }).Count
            }
        } |
        Sort-Object provider
)

$fixtureSummary = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    fixture_kind = $FixtureKind
    message_count = $resolvedMessageCount
    split_count = $channelCount
    viewport_width = $ViewportWidth
    viewport_height = $ViewportHeight
    seed = $Seed
    provider_summary = $providerSummary
    action_count = $actions.Count
    decision = "render fixture recorded; not frame-time evidence"
}
$fixtureSummary | ConvertTo-Json -Depth 10 | Set-Content -Path $summaryPath -Encoding utf8

$manifest = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    fixture_kind = $FixtureKind
    messages_path = $messagesPath
    summary_path = $summaryPath
    layout = $layout
    actions = $actions
    decision = "render fixture recorded; not measured"
}
$manifest | ConvertTo-Json -Depth 10 | Set-Content -Path $manifestPath -Encoding utf8

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
Set-JsonProperty -Object $rawArtifacts -Name "render_fixture" -Value $messagesPath
Set-JsonProperty -Object $rawArtifacts -Name "render_fixture_manifest" -Value $manifestPath
Set-JsonProperty -Object $rawArtifacts -Name "render_fixture_summary" -Value $summaryPath
Set-JsonProperty -Object $runSummary -Name "render_fixture" -Value ([pscustomobject]$fixtureSummary)
Set-JsonProperty -Object $runSummary -Name "decision" -Value "render fixture recorded; not measured"
$runSummary | ConvertTo-Json -Depth 12 | Set-Content -Path $runSummaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Render fixture"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Messages artifact: $messagesPath"
    Add-Content -Path $notesPath -Value "- Manifest artifact: $manifestPath"
    Add-Content -Path $notesPath -Value "- Fixture kind: $FixtureKind"
    Add-Content -Path $notesPath -Value "- Messages: $resolvedMessageCount"
    Add-Content -Path $notesPath -Value "- Splits: $channelCount"
    Add-Content -Path $notesPath -Value "- Decision: render fixture recorded; not measured"
}

Write-Output $manifestPath
