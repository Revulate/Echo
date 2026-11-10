param(
    [string]$Fixture = "docs\fixtures\7tv-eventapi\emote-set-add-remove-rename.json",
    [string]$SetId = "example_set_id",
    [string]$Channel = "examplechannel",
    [int]$DelayMs = 3000,
    [switch]$ReplayOnly
)

$ErrorActionPreference = "Stop"

$repoRoot = Split-Path -Parent $PSScriptRoot
$fixturePath = Join-Path $repoRoot $Fixture
if (-not (Test-Path -LiteralPath $fixturePath)) {
    throw "Replay fixture not found: $fixturePath"
}

$logDir = Join-Path $repoRoot "debug-logs"
New-Item -ItemType Directory -Force -Path $logDir | Out-Null
$stamp = Get-Date -Format "yyyyMMdd-HHmmss"
$stdout = Join-Path $logDir "echo-7tv-replay-$stamp.stdout.log"
$stderr = Join-Path $logDir "echo-7tv-replay-$stamp.stderr.log"

$env:ECHO_LOG = "irc,emotes,cache,perf,perf_cache,perf_emotes,perf_render,ui,net"
$env:ECHO_PERF_MONITOR = "1"
$env:ECHO_RENDER_TRACE = "1"
$env:ECHO_7TV_EVENTAPI_REPLAY = $fixturePath
$env:ECHO_7TV_EVENTAPI_REPLAY_SET_ID = $SetId
$env:ECHO_7TV_EVENTAPI_REPLAY_CHANNEL = $Channel
$env:ECHO_7TV_EVENTAPI_REPLAY_DELAY_MS = "$DelayMs"
if ($ReplayOnly) {
    $env:ECHO_7TV_EVENTAPI_REPLAY_ONLY = "1"
} else {
    Remove-Item Env:\ECHO_7TV_EVENTAPI_REPLAY_ONLY -ErrorAction SilentlyContinue
}

Write-Host "Running 7TV EventAPI replay"
Write-Host "Fixture: $fixturePath"
Write-Host "SetId:   $SetId"
Write-Host "Channel: $Channel"
Write-Host "Stdout:  $stdout"
Write-Host "Stderr:  $stderr"

Push-Location $repoRoot
try {
    cargo run --bin echo > $stdout 2> $stderr
} finally {
    Pop-Location
}
