<#
.SYNOPSIS
Ratchet: literal colors, spacing, radii and text sizes in src/components may
only decrease against scripts/ui-literals-baseline.txt.
#>
[CmdletBinding()]
param(
    [switch]$SelfTest,
    [switch]$UpdateBaseline
)

# Ratchet for literal colors, spacing, radii, and text sizes in src/components.
# UI code should use theme::colors and theme::tokens; each category's count may
# only go down. After removing literals, rerun with -UpdateBaseline to lock the
# lower count in.

$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$componentsRoot = Join-Path $repositoryRoot "src/components"
$baselinePath = Join-Path $PSScriptRoot "ui-literals-baseline.txt"

$categories = [ordered]@{
    color     = '\bhsla\(\s*-?[0-9.]|\brgba?\(\s*0x|(?<!\w)(black|white)\(\)'
    spacing   = '\.(p|px|py|pt|pb|pl|pr|m|mx|my|mt|mb|ml|mr|gap|gap_x|gap_y)\(\s*px\(\s*-?[0-9]'
    radius    = '\.rounded(_[a-z]+)?\(\s*px\(\s*[0-9]'
    text_size = '\.text_size\(\s*px\(\s*[0-9]'
}

function Measure-Literals([string]$source) {
    $counts = [ordered]@{}
    foreach ($name in $categories.Keys) {
        $counts[$name] = [regex]::Matches($source, $categories[$name]).Count
    }
    $counts
}

function Read-Baseline([string]$path) {
    $baseline = @{}
    foreach ($line in Get-Content -LiteralPath $path) {
        if ($line -match '^\s*([a-z_]+)\s*=\s*(\d+)\s*$') {
            $baseline[$Matches[1]] = [int]$Matches[2]
        }
    }
    $baseline
}

function Compare-Counts($current, $baseline) {
    $regressions = @()
    foreach ($name in $categories.Keys) {
        if (-not $baseline.ContainsKey($name)) {
            $regressions += "$name (missing from baseline)"
        } elseif ($current[$name] -gt $baseline[$name]) {
            $regressions += "$name $($baseline[$name]) -> $($current[$name])"
        }
    }
    , $regressions
}

if ($SelfTest) {
    $literalFixture = @'
div().bg(hsla(0.0, 0.0, 0.0, 0.5)).text_color(rgb(0xffffff)).border_color(gpui::black())
    .p(px(8.0)).gap_x(px(4.0)).mt(px(-2.0))
    .rounded(px(4.0)).rounded_t(px(6.0))
    .text_size(px(13.0))
'@
    $tokenFixture = @'
div().bg(colors::backdrop()).text_color(colors::white_alpha(0.5)).border_color(transparent_black())
    .p(px(Spacing::SM)).gap_x(px(Spacing::XS)).mt(px(-Spacing::XXS))
    .rounded(px(Radii::SM)).rounded_t(Radii::md())
    .text_size(px(FontSize::BASE))
'@
    $expected = @{ color = 3; spacing = 3; radius = 2; text_size = 1 }
    $literalCounts = Measure-Literals $literalFixture
    foreach ($name in $categories.Keys) {
        if ($literalCounts[$name] -ne $expected[$name]) {
            throw "Self-test: $name matched $($literalCounts[$name]) literal fixture(s), expected $($expected[$name])."
        }
    }
    $tokenCounts = Measure-Literals $tokenFixture
    foreach ($name in $categories.Keys) {
        if ($tokenCounts[$name] -ne 0) {
            throw "Self-test: $name flagged $($tokenCounts[$name]) token usage(s) as literals."
        }
    }
    $fakeBaseline = @{ color = 3; spacing = 2; radius = 2 }
    $regressions = Compare-Counts $literalCounts $fakeBaseline
    if ($regressions.Count -ne 2) {
        throw "Self-test: expected an increased and a missing category, got: $($regressions -join ', ')"
    }
}

$totals = [ordered]@{}
foreach ($name in $categories.Keys) { $totals[$name] = 0 }
Get-ChildItem -LiteralPath $componentsRoot -Recurse -File -Filter "*.rs" | ForEach-Object {
    $counts = Measure-Literals ([IO.File]::ReadAllText($_.FullName))
    foreach ($name in $categories.Keys) { $totals[$name] += $counts[$name] }
}

$summary = ($categories.Keys | ForEach-Object { "$_=$($totals[$_])" }) -join ", "

if ($UpdateBaseline) {
    $lines = @("# Literal UI styling counts in src/components; see scripts/check-ui-literals.ps1.")
    $lines += $categories.Keys | ForEach-Object { "$_=$($totals[$_])" }
    Set-Content -LiteralPath $baselinePath -Value $lines -Encoding utf8NoBOM
    Write-Host "Baseline updated: $summary"
    return
}

if (-not (Test-Path -LiteralPath $baselinePath)) {
    throw "Missing baseline $baselinePath; run with -UpdateBaseline to create it."
}
$baseline = Read-Baseline $baselinePath
$regressions = Compare-Counts $totals $baseline
if ($regressions.Count -gt 0) {
    throw "Literal UI styling increased in src/components (use theme::colors / theme::tokens): $($regressions -join ', ')"
}

$lowered = @($categories.Keys | Where-Object { $totals[$_] -lt $baseline[$_] })
if ($lowered.Count -gt 0) {
    Write-Host "Counts dropped for $($lowered -join ', '); rerun with -UpdateBaseline to lock them in."
}
Write-Host "PASS: UI literal counts within baseline ($summary)."
