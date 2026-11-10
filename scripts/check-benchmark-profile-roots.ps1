[CmdletBinding()]
param(
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$sourceRoot = Join-Path $repositoryRoot "src"
$centralModule = "benchmark_profile.rs"
$forbiddenPatterns = @(
    [ordered]@{ pattern = "dirs::(config_dir|data_dir|data_local_dir|cache_dir)\s*\("; description = "direct dirs storage root" },
    [ordered]@{ pattern = 'PathBuf::from\("(settings|user_notes|sounds|echo_trace|dhat-heap)\.json"\)'; description = "legacy cwd storage file" },
    [ordered]@{ pattern = 'std::env::var\("HOME"\)'; description = "manual HOME storage root" }
)

if (-not (Test-Path -LiteralPath (Join-Path $sourceRoot $centralModule))) {
    throw "Missing benchmark profile storage module: $centralModule"
}

if ($SelfTest) {
    foreach ($negativeFixture in @("let path = dirs::config_dir();", "let path = dirs::data_dir();")) {
        if ($negativeFixture -notmatch $forbiddenPatterns[0].pattern) {
            throw "Internal negative fixture did not match a forbidden direct dirs call: $negativeFixture"
        }
    }
    $negativeFixture = 'let path = PathBuf::from("sounds.json");'
    if ($negativeFixture -notmatch $forbiddenPatterns[1].pattern) {
        throw "Internal negative fixture did not match a forbidden legacy cwd storage file."
    }
}

$violations = @()
Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Filter "*.rs" | ForEach-Object {
    $relativePath = [IO.Path]::GetRelativePath($sourceRoot, $_.FullName) -replace "\\", "/"
    if ($relativePath -eq $centralModule) {
        return
    }
    $source = Get-Content -LiteralPath $_.FullName -Raw
    foreach ($rule in $forbiddenPatterns) {
        if ($source -match $rule.pattern) {
            $violations += "$relativePath ($($rule.description))"
        }
    }
}

if ($violations.Count -gt 0) {
    throw "Echo-owned storage must use benchmark_profile; isolated-root violations found in: $($violations -join ', ')"
}

Write-Host "PASS: all Echo-owned storage roots route through $centralModule."
