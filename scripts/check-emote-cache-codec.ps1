[CmdletBinding()]
param(
    [string] $BaselineRef
)

$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$deps = Join-Path $root 'target/debug/deps'
$testTemplate = Join-Path $PSScriptRoot 'tests/emote_cache_codec.rs'
$current = Join-Path $root 'src/backend/emotes/cache_codec.rs'

function Find-Rlib([string] $pattern) {
    $matches = @(Get-ChildItem -LiteralPath $deps -Filter $pattern)
    if ($pattern -eq 'libwindows-*.rlib') {
        $matches = @($matches | Where-Object {
            $depInfo = Join-Path $deps ($_.BaseName.Substring(3) + '.d')
            (Get-Content -LiteralPath $depInfo -TotalCount 1) -match 'windows-0\.62\.'
        })
    }
    if ($pattern -eq 'libserde-*.rlib' -and $matches.Count -gt 1) {
        # The pinned Cargo can emit host and target Serde variants. Match the
        # dependency used by the actual emote model instead of picking by age.
        $model = Get-Item -LiteralPath (Find-Rlib 'libecho_emote_core-*.rlib')
        $modelHash = $model.BaseName.Substring('libecho_emote_core-'.Length)
        $fingerprints = Join-Path $root 'target/debug/.fingerprint'
        $modelInfo = Get-Content -LiteralPath (Join-Path $fingerprints "echo-emote-core-$modelHash/lib-echo_emote_core.json") -Raw | ConvertFrom-Json
        $serdeDependency = @($modelInfo.deps | Where-Object { $_[1] -eq 'serde' })
        if ($serdeDependency.Count -ne 1) { throw 'Unable to identify the model Serde dependency.' }
        $expected = [Convert]::ToHexString([BitConverter]::GetBytes([UInt64]$serdeDependency[0][3])).ToLowerInvariant()
        $matches = @($matches | Where-Object {
            $fingerprint = Join-Path $fingerprints ($_.BaseName.Substring(3) + '/lib-serde')
            (Get-Content -LiteralPath $fingerprint -Raw).Trim() -eq $expected
        })
    }
    if ($matches.Count -ne 1) {
        throw "Expected one compiled dependency matching $pattern; found $($matches.Count). Run cargo debug-build in a clean target first."
    }
    $matches[0].FullName
}

function Get-Section([string] $source, [string] $startMarker, [string] $endMarker) {
    $start = $source.IndexOf($startMarker, [StringComparison]::Ordinal)
    if ($start -lt 0) { throw "Missing baseline section: $startMarker" }
    $end = $source.IndexOf($endMarker, $start, [StringComparison]::Ordinal)
    if ($end -lt 0) { throw "Missing baseline boundary: $endMarker" }
    $source.Substring($start, $end - $start)
}

function Get-BaselineCodec {
    $original = (& git -C $root show "${BaselineRef}:src/backend/emotes/mod.rs") -join "`n"
    if ($LASTEXITCODE) { throw "Unable to read baseline $BaselineRef" }
    $imports = Get-Section (Get-Content -LiteralPath $current -Raw) 'use super::' '/// Legacy Bincode v9 configuration'
    $parts = @(
        $imports,
        (Get-Section $original '/// Legacy Bincode v9 configuration' '// Shared HTTP client'),
        (Get-Section $original '/// Current Postcard cache schema version' '/// TTL for cached emotes'),
        (Get-Section $original '/// Persistent emote cache file structure.' 'fn channel_disk_cache_fetched_at'),
        (Get-Section $original "#[cfg(test)]`nmod emote_cache_codec_tests" "#[cfg(test)]`nmod provider_index_tests")
    )
    $baseline = $parts -join "`n"
    # rustfmt canonicalization preserves literal contents, unlike whitespace stripping.
    $baseline
}

function Test-Codec([string] $label, [string] $baselineSource = '') {
    $temporaryRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
    $tempRoot = Join-Path $temporaryRoot ("echo-emote-codec-$label-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $tempRoot | Out-Null
    try {
        $codecPath = $current
        if ($baselineSource) {
            $codecPath = Join-Path $tempRoot 'baseline.rs'
            [IO.File]::WriteAllText($codecPath, $baselineSource)
            $comparison = Join-Path $tempRoot 'current.rs'
            [IO.File]::WriteAllText($comparison, (Get-Content -LiteralPath $current -Raw).Replace('pub(super) ', ''))
            rustfmt --edition 2024 $codecPath $comparison
            if ($LASTEXITCODE) { throw 'Baseline comparison formatting failed.' }
            if ((Get-Content -LiteralPath $codecPath -Raw) -cne (Get-Content -LiteralPath $comparison -Raw)) {
                throw 'Codec source differs beyond module imports, formatting, and parent visibility.'
            }
            Write-Host 'PASS: baseline/current codec source equivalence (including literals and tests).'
        }
        $source = (Get-Content -Raw $testTemplate).Replace('../../src/backend/emotes/cache_codec.rs', ($codecPath -replace '\\', '/'))
        $source = $source.Replace('../../src/utils/atomic_file.rs', ((Join-Path $root 'src/utils/atomic_file.rs') -replace '\\', '/'))
        $harness = Join-Path $tempRoot 'harness.rs'
        $binary = Join-Path $tempRoot 'harness.exe'
        [IO.File]::WriteAllText($harness, $source)
        rustc --edition 2024 --test --crate-name echo_emote_cache_codec $harness "-Ldependency=$deps" "--extern=bincode=$(Find-Rlib 'libcu_bincode-*.rlib')" "--extern=echo_core=$(Find-Rlib 'libecho_core-*.rlib')" "--extern=echo_emote_core=$(Find-Rlib 'libecho_emote_core-*.rlib')" "--extern=serde_json=$(Find-Rlib 'libserde_json-*.rlib')" "--extern=postcard=$(Find-Rlib 'libpostcard-*.rlib')" "--extern=serde=$(Find-Rlib 'libserde-*.rlib')" "--extern=zstd=$(Find-Rlib 'libzstd-*.rlib')" "--extern=windows=$(Find-Rlib 'libwindows-*.rlib')" -o $binary
        if ($LASTEXITCODE) { throw "rustc failed for $label" }
        & $binary --nocapture
        if ($LASTEXITCODE) { throw "codec tests failed for $label" }
    } finally {
        $resolved = [IO.Path]::GetFullPath($tempRoot)
        if (-not $resolved.StartsWith($temporaryRoot, [StringComparison]::OrdinalIgnoreCase) -or
            ((Get-Item -LiteralPath $resolved).Attributes -band [IO.FileAttributes]::ReparsePoint)) {
            throw "Refusing cleanup outside the ordinary temporary fixture: $resolved"
        }
        Remove-Item -LiteralPath $resolved -Recurse -Force
    }
}

if ($BaselineRef) {
    Test-Codec 'baseline' (Get-BaselineCodec)
}
Test-Codec 'current'
