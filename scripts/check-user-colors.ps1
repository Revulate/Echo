[CmdletBinding()]
param(
    [ValidateSet('Baseline', 'Current')]
    [string]$Mode = 'Current',
    [string]$BaselineRef
)

$ErrorActionPreference = 'Stop'
$repoRoot = Split-Path -Parent $PSScriptRoot
$depsPath = Join-Path $repoRoot 'target\debug\deps'
$rustc = (Get-Command rustc -ErrorAction Stop).Source
$outputPath = Join-Path $depsPath ("user-colors-{0}.exe" -f $Mode.ToLowerInvariant())

function Get-Rlib([string]$crateName) {
    $matches = Get-ChildItem -LiteralPath $depsPath -Filter ("lib{0}-*.rlib" -f $crateName) |
        Sort-Object LastWriteTime -Descending
    if ($matches.Count -eq 0) {
        throw "Missing compiled dependency for $crateName under $depsPath. Build the debug target first."
    }
    $matches[0].FullName
}

# Several builds can leave serde rlibs linked to different serde_core builds.
# The harness derives serde traits and hands them to serde_json, so both must
# come from the same build: probe pairs newest-first until one type-checks.
function Get-SerdePair {
    $candidates = {
        param([string]$crateName)
        Get-ChildItem -LiteralPath $depsPath -Filter ("lib{0}-*.rlib" -f $crateName) |
            Sort-Object LastWriteTime -Descending | ForEach-Object FullName
    }
    $probe = [System.IO.Path]::GetTempFileName()
    $probeOutput = "$probe.rmeta"
    try {
        Set-Content -LiteralPath $probe -NoNewline -Value @'
#[derive(serde::Serialize, serde::Deserialize)]
struct Probe { value: u8 }
fn main() { let _ = serde_json::to_string(&Probe { value: 1 }); }
'@
        foreach ($json in (& $candidates 'serde_json')) {
            foreach ($serde in (& $candidates 'serde')) {
                & $rustc --edition=2024 --crate-name serde_probe --crate-type=bin --emit=metadata `
                    -o $probeOutput $probe -L "dependency=$depsPath" `
                    --extern "serde=$serde" --extern "serde_json=$json" 2>$null
                if ($LASTEXITCODE -eq 0) { return @($serde, $json) }
            }
        }
    } finally {
        Remove-Item -LiteralPath $probe, $probeOutput -Force -ErrorAction SilentlyContinue
    }
    throw "No serde/serde_json pair under $depsPath shares a serde_core build. Build the debug target first."
}

function Get-Section([string]$source, [string]$startMarker, [string]$endMarker) {
    $start = $source.IndexOf($startMarker, [StringComparison]::Ordinal)
    if ($start -lt 0) {
        throw "Could not find expected user-color section start: $startMarker"
    }
    $end = $source.IndexOf($endMarker, $start, [StringComparison]::Ordinal)
    if ($end -lt 0) {
        throw "Could not find expected user-color section end: $endMarker"
    }
    $source.Substring($start, $end - $start)
}

function Normalize-Source([string]$source) {
    ($source -split "`r?`n" |
        Where-Object { -not $_.TrimStart().StartsWith('//') } |
        ForEach-Object { $_.TrimEnd() }) -join "`n"
}

function Get-BaselineSource {
    $connection = (& git -C $repoRoot show ("{0}:src/backend/irc/connection.rs" -f $BaselineRef)) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Could not read baseline connection source at $BaselineRef" }
    $constants = Get-Section $connection 'const USER_COLOR_CACHE_SIZE' '// Cached-channel emote refresh schedule.'
    $state = Get-Section $connection '/// User color LRU cache' '/// Recent chatters per channel for @username completion'
    $functions = Get-Section $connection 'const USER_COLOR_CACHE_FILE' '// ============================================================================'
    $testSource = Get-Content -LiteralPath (Join-Path $repoRoot 'src\backend\irc\user_colors.rs') -Raw
    $tests = $testSource.Substring($testSource.IndexOf("#[cfg(test)]", [StringComparison]::Ordinal))
    $tests = $tests -replace 'use super::\*;\s*', 'use crate::*;'

@"
#![allow(unused_variables)]

use lru::LruCache;
use parking_lot::{Mutex, RwLock};
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;
use std::num::NonZeroUsize;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicU64, Ordering};

#[macro_export]
macro_rules! log_cache {
    (`$(`$tokens:tt)*) => { () };
}

mod benchmark_profile {
    pub fn config_root() -> Option<std::path::PathBuf> { None }
}

mod config {
    pub fn atomic_write_json(_file_path: &std::path::Path, _content: &str) -> Result<(), Box<dyn std::error::Error>> { Ok(()) }
}

mod backend {
    pub mod settings {
        pub fn get_colorize_users_without_color() -> bool { true }
    }
}

$constants
$state
$functions
$tests
"@
}

function Assert-EquivalentFunctionBodies([string]$reference) {
    $connection = (& git -C $repoRoot show ("{0}:src/backend/irc/connection.rs" -f $reference)) -join "`n"
    if ($LASTEXITCODE -ne 0) { throw "Could not read baseline connection source at $reference" }
    $current = Get-Content -LiteralPath (Join-Path $repoRoot 'src\backend\irc\user_colors.rs') -Raw
    $baselineFunctions = Get-Section $connection 'fn get_user_color_cache_path' '// ============================================================================'
    $currentFunctions = Get-Section $current 'fn get_user_color_cache_path' '#[cfg(test)]'
    if ((Normalize-Source $baselineFunctions) -ne (Normalize-Source $currentFunctions)) {
        throw 'User-color function bodies diverged from the baseline source during extraction.'
    }
    Write-Host 'User-color function-body equivalence: passed'
}

$sourcePath = Join-Path $repoRoot 'scripts\tests\user_colors.rs'
$temporarySource = $null
try {
    if ($Mode -eq 'Baseline') {
        if ([string]::IsNullOrWhiteSpace($BaselineRef)) {
            throw 'Baseline mode requires -BaselineRef with the immutable pre-extraction commit.'
        }
        Assert-EquivalentFunctionBodies $BaselineRef
        $temporarySource = [System.IO.Path]::GetTempFileName()
        Set-Content -LiteralPath $temporarySource -Value (Get-BaselineSource) -NoNewline
        $sourcePath = $temporarySource
    }

    $serdeRlib, $serdeJsonRlib = Get-SerdePair
    $args = @(
        '--edition=2024', '--crate-name', 'user_color_checks', '--test', $sourcePath, '-o', $outputPath,
        '-C', 'debuginfo=0', '-L', "dependency=$depsPath",
        '--extern', "lru=$(Get-Rlib 'lru')",
        '--extern', "parking_lot=$(Get-Rlib 'parking_lot')",
        '--extern', "serde=$serdeRlib",
        '--extern', "serde_json=$serdeJsonRlib"
    )
    & $rustc @args
    if ($LASTEXITCODE -ne 0) { throw "rustc harness compile failed with exit code $LASTEXITCODE" }
    $testName = if ($Mode -eq 'Baseline') {
        'tests::preserves_user_color_resolution_contract'
    } else {
        'user_colors::tests::preserves_user_color_resolution_contract'
    }
    & $outputPath --exact $testName
    if ($LASTEXITCODE -ne 0) { throw "user-color harness failed with exit code $LASTEXITCODE" }
} finally {
    if ($temporarySource) { Remove-Item -LiteralPath $temporarySource -Force -ErrorAction SilentlyContinue }
    Remove-Item -LiteralPath $outputPath -Force -ErrorAction SilentlyContinue
}
