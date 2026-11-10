[CmdletBinding()]
param(
    [ValidateSet("dist", "dist-max")]
    [string]$Profile = "dist",
    [string]$OutputDirectory = "",
    [ValidateSet("preserved", "profile-invalidated", "cold")]
    [string]$CacheState = "preserved",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

$scriptRoot = Split-Path -Parent $PSCommandPath
$repoRoot = (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
$runStamp = Get-Date -Format "yyyyMMdd-HHmmss"

if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
    $OutputDirectory = Join-Path $repoRoot "benchmarks\results\$runStamp\dist-build-speed-$Profile"
}

$runDirectory = (New-Item -ItemType Directory -Force -Path $OutputDirectory).FullName
$rawPath = Join-Path $runDirectory "build.raw.txt"
$summaryPath = Join-Path $runDirectory "summary.json"
$targetDirectory = if ($Profile -eq "dist-max") { "dist-max" } else { "dist" }
$executablePath = Join-Path $repoRoot "target\$targetDirectory\echo.exe"

$commit = ((& git -C $repoRoot rev-parse HEAD) | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "Unable to resolve the current Git commit."
}
$dirty = @(& git -C $repoRoot status --porcelain).Count -gt 0
$rustcVersion = ((& rustc --version --verbose) | Out-String).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "Unable to resolve rustc version information."
}

$startedUtc = [DateTimeOffset]::UtcNow
$watch = [System.Diagnostics.Stopwatch]::StartNew()
$cargoExitCode = 1

Push-Location $repoRoot
try {
    & cargo build --profile $Profile --locked --bin echo 2>&1 |
        Tee-Object -FilePath $rawPath
    $cargoExitCode = $LASTEXITCODE
}
finally {
    Pop-Location
    $watch.Stop()
}

$artifact = $null
if (Test-Path -LiteralPath $executablePath) {
    $item = Get-Item -LiteralPath $executablePath
    $version = [System.Diagnostics.FileVersionInfo]::GetVersionInfo($item.FullName)
    $artifact = [ordered]@{
        path = $item.FullName
        bytes = $item.Length
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $item.FullName).Hash.ToLowerInvariant()
        file_version = $version.FileVersion
        product_version = $version.ProductVersion
        last_write_utc = $item.LastWriteTimeUtc.ToString("o")
    }
}

$summary = [ordered]@{
    scenario = "optimized Windows dist build speed"
    started_utc = $startedUtc.ToString("o")
    finished_utc = [DateTimeOffset]::UtcNow.ToString("o")
    elapsed_seconds = [Math]::Round($watch.Elapsed.TotalSeconds, 3)
    target_seconds = 180
    target_met = $cargoExitCode -eq 0 -and $watch.Elapsed.TotalSeconds -lt 180
    cargo_exit_code = $cargoExitCode
    command = "cargo build --profile $Profile --locked --bin echo"
    profile = $Profile
    cache_state = $CacheState
    logical_processor_count = [Environment]::ProcessorCount
    commit = $commit
    dirty = $dirty
    rustc = $rustcVersion
    artifact = $artifact
    raw_output = $rawPath
    notes = $Notes
}

$summary | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $summaryPath -Encoding utf8
Write-Host "Dist build elapsed: $($summary.elapsed_seconds) seconds"
Write-Host "Target under 180 seconds: $($summary.target_met)"
Write-Host "Summary: $summaryPath"

if ($cargoExitCode -ne 0) {
    exit $cargoExitCode
}
