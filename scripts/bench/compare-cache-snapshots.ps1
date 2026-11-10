[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$BeforeSnapshot,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$AfterSnapshot,

    [string]$RunDir = "",
    [string]$OutputFile = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

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

function Read-Snapshot {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $resolved = (Resolve-Path $Path).Path
    $json = Get-Content -Raw $resolved | ConvertFrom-Json
    $schemaVersion = Get-JsonProperty -Object $json -Name "schema_version"
    if ($null -eq $schemaVersion) {
        throw "Cache snapshot is missing schema_version: $resolved"
    }

    foreach ($requiredField in @("app", "cache_name", "phase", "file_count", "directory_count", "total_bytes")) {
        if ($null -eq (Get-JsonProperty -Object $json -Name $requiredField)) {
            throw "Cache snapshot is missing $requiredField`: $resolved"
        }
    }

    return [pscustomobject]@{
        path = $resolved
        json = $json
    }
}

function ConvertTo-ExtensionMap {
    param(
        [object]$ExtensionSummary
    )

    $map = @{}
    foreach ($entry in @($ExtensionSummary)) {
        if ($null -eq $entry) {
            continue
        }

        $extension = [string](Get-JsonProperty -Object $entry -Name "extension")
        if ([string]::IsNullOrWhiteSpace($extension)) {
            $extension = "[unknown]"
        }
        $map[$extension] = [pscustomobject]@{
            extension = $extension
            file_count = [int64](Get-JsonProperty -Object $entry -Name "file_count")
            bytes = [int64](Get-JsonProperty -Object $entry -Name "bytes")
        }
    }

    return $map
}

$before = Read-Snapshot -Path $BeforeSnapshot
$after = Read-Snapshot -Path $AfterSnapshot
$beforeJson = $before.json
$afterJson = $after.json

$resolvedRunDir = if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
    (New-Item -ItemType Directory -Force -Path $RunDir).FullName
} else {
    Split-Path -Parent $after.path
}

$app = [string](Get-JsonProperty -Object $afterJson -Name "app")
$cacheName = [string](Get-JsonProperty -Object $afterJson -Name "cache_name")
$beforePhase = [string](Get-JsonProperty -Object $beforeJson -Name "phase")
$afterPhase = [string](Get-JsonProperty -Object $afterJson -Name "phase")
$beforeExtensionMap = ConvertTo-ExtensionMap -ExtensionSummary (Get-JsonProperty -Object $beforeJson -Name "extension_summary")
$afterExtensionMap = ConvertTo-ExtensionMap -ExtensionSummary (Get-JsonProperty -Object $afterJson -Name "extension_summary")
$extensions = @($beforeExtensionMap.Keys + $afterExtensionMap.Keys | Sort-Object -Unique)

$extensionDeltas = @(
    foreach ($extension in $extensions) {
        $beforeEntry = if ($beforeExtensionMap.ContainsKey($extension)) { $beforeExtensionMap[$extension] } else { $null }
        $afterEntry = if ($afterExtensionMap.ContainsKey($extension)) { $afterExtensionMap[$extension] } else { $null }
        $beforeFileCount = if ($null -ne $beforeEntry) { [int64]$beforeEntry.file_count } else { [int64]0 }
        $afterFileCount = if ($null -ne $afterEntry) { [int64]$afterEntry.file_count } else { [int64]0 }
        $beforeBytes = if ($null -ne $beforeEntry) { [int64]$beforeEntry.bytes } else { [int64]0 }
        $afterBytes = if ($null -ne $afterEntry) { [int64]$afterEntry.bytes } else { [int64]0 }

        [ordered]@{
            extension = $extension
            before_file_count = $beforeFileCount
            after_file_count = $afterFileCount
            file_count_delta = $afterFileCount - $beforeFileCount
            before_bytes = $beforeBytes
            after_bytes = $afterBytes
            bytes_delta = $afterBytes - $beforeBytes
        }
    }
)

$beforeFileCount = [int64](Get-JsonProperty -Object $beforeJson -Name "file_count")
$afterFileCount = [int64](Get-JsonProperty -Object $afterJson -Name "file_count")
$beforeDirectoryCount = [int64](Get-JsonProperty -Object $beforeJson -Name "directory_count")
$afterDirectoryCount = [int64](Get-JsonProperty -Object $afterJson -Name "directory_count")
$beforeBytes = [int64](Get-JsonProperty -Object $beforeJson -Name "total_bytes")
$afterBytes = [int64](Get-JsonProperty -Object $afterJson -Name "total_bytes")
$sameApp = [string](Get-JsonProperty -Object $beforeJson -Name "app") -eq [string](Get-JsonProperty -Object $afterJson -Name "app")
$sameCache = [string](Get-JsonProperty -Object $beforeJson -Name "cache_name") -eq [string](Get-JsonProperty -Object $afterJson -Name "cache_name")

$comparison = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $app
    cache_name = $cacheName
    before_snapshot = $before.path
    after_snapshot = $after.path
    before_phase = $beforePhase
    after_phase = $afterPhase
    same_app = $sameApp
    same_cache = $sameCache
    before = [ordered]@{
        cache_exists = [bool](Get-JsonProperty -Object $beforeJson -Name "cache_exists")
        file_count = $beforeFileCount
        directory_count = $beforeDirectoryCount
        total_bytes = $beforeBytes
    }
    after = [ordered]@{
        cache_exists = [bool](Get-JsonProperty -Object $afterJson -Name "cache_exists")
        file_count = $afterFileCount
        directory_count = $afterDirectoryCount
        total_bytes = $afterBytes
    }
    delta = [ordered]@{
        file_count = $afterFileCount - $beforeFileCount
        directory_count = $afterDirectoryCount - $beforeDirectoryCount
        total_bytes = $afterBytes - $beforeBytes
    }
    extension_deltas = $extensionDeltas
    decision = "cache snapshots compared; not an app performance comparison"
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $artifactName = "{0}-{1}-{2}-to-{3}.cache-comparison.json" -f `
        (ConvertTo-Slug $app),
        (ConvertTo-Slug $cacheName),
        (ConvertTo-Slug $beforePhase),
        (ConvertTo-Slug $afterPhase)
    $OutputFile = Join-Path $resolvedRunDir $artifactName
}

$comparison | ConvertTo-Json -Depth 10 | Set-Content -Path $OutputFile -Encoding utf8

$summaryPath = Join-Path $resolvedRunDir "summary.json"
if (Test-Path $summaryPath) {
    $summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
} else {
    $summary = [pscustomobject]@{
        schema_version = 1
        scenario = "cache snapshot comparison"
        status = "initialized"
        measurements_recorded = $false
        raw_artifacts = [pscustomobject]@{}
        decision = "not measured"
    }
}

$rawArtifacts = Ensure-ObjectProperty -Object $summary -Name "raw_artifacts"
$cacheComparisons = Ensure-ObjectProperty -Object $rawArtifacts -Name "cache_comparisons"
$comparisonKey = "{0}_{1}_{2}_to_{3}" -f `
    (ConvertTo-Slug $app),
    (ConvertTo-Slug $cacheName),
    (ConvertTo-Slug $beforePhase),
    (ConvertTo-Slug $afterPhase)
Set-JsonProperty -Object $cacheComparisons -Name $comparisonKey -Value $OutputFile
$cacheComparisonSummary = Ensure-ObjectProperty -Object $summary -Name "cache_comparisons"
Set-JsonProperty -Object $cacheComparisonSummary -Name $comparisonKey -Value ([pscustomobject]$comparison)
Set-JsonProperty -Object $summary -Name "decision" -Value "cache snapshots compared; not measured"
$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $resolvedRunDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Cache snapshot comparison"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Comparison artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Before snapshot: $($before.path)"
    Add-Content -Path $notesPath -Value "- After snapshot: $($after.path)"
    Add-Content -Path $notesPath -Value "- File delta: $($afterFileCount - $beforeFileCount)"
    Add-Content -Path $notesPath -Value "- Byte delta: $($afterBytes - $beforeBytes)"
    Add-Content -Path $notesPath -Value "- Decision: cache snapshots compared; not measured"
}

Write-Output $OutputFile
