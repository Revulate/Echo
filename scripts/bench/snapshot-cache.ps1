[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CachePath,

    [ValidateSet("echo", "other")]
    [string]$App = "other",

    [string]$CacheName = "cache",
    [string]$Phase = "snapshot",
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$Scenario = "cache snapshot",
    [int]$LargestFileCount = 20
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
        Scenario = $Scenario
        CacheState = $Phase
        Notes = "Cache snapshot for $App/$CacheName at phase '$Phase'."
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

function ConvertTo-RelativePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$BasePath,

        [Parameter(Mandatory = $true)]
        [string]$TargetPath
    )

    $baseFullPath = [System.IO.Path]::GetFullPath($BasePath)
    if (-not $baseFullPath.EndsWith([System.IO.Path]::DirectorySeparatorChar)) {
        $baseFullPath = $baseFullPath + [System.IO.Path]::DirectorySeparatorChar
    }

    $targetFullPath = [System.IO.Path]::GetFullPath($TargetPath)
    $baseUri = [System.Uri]::new($baseFullPath)
    $targetUri = [System.Uri]::new($targetFullPath)
    $relativeUri = $baseUri.MakeRelativeUri($targetUri)

    return [System.Uri]::UnescapeDataString($relativeUri.ToString()).Replace("/", [System.IO.Path]::DirectorySeparatorChar)
}

$repoRoot = Resolve-RepoRoot
$scenarioDir = Initialize-RunDirectory -RepoRoot $repoRoot
$cacheExists = Test-Path $CachePath
$resolvedCachePath = if ($cacheExists) { (Resolve-Path $CachePath).Path } else { $CachePath }
$files = @()
$directories = @()

if ($cacheExists) {
    $files = @(Get-ChildItem -LiteralPath $resolvedCachePath -File -Recurse -Force -ErrorAction Stop)
    $directories = @(Get-ChildItem -LiteralPath $resolvedCachePath -Directory -Recurse -Force -ErrorAction Stop)
}

$totalBytes = [int64]0
foreach ($file in $files) {
    $totalBytes += [int64]$file.Length
}

$extensionGroups = @(
    $files |
        Group-Object { if ([string]::IsNullOrWhiteSpace($_.Extension)) { "[none]" } else { $_.Extension.ToLowerInvariant() } } |
        ForEach-Object {
            $bytes = [int64]0
            foreach ($file in $_.Group) {
                $bytes += [int64]$file.Length
            }
            [ordered]@{
                extension = $_.Name
                file_count = $_.Count
                bytes = $bytes
            }
        } |
        Sort-Object -Property @{ Expression = "bytes"; Descending = $true }, @{ Expression = "file_count"; Descending = $true }
)

$largestFiles = @(
    $files |
        Sort-Object Length -Descending |
        Select-Object -First $LargestFileCount |
        ForEach-Object {
            [ordered]@{
                relative_path = if ($cacheExists) {
                    ConvertTo-RelativePath -BasePath $resolvedCachePath -TargetPath $_.FullName
                } else {
                    $_.FullName
                }
                bytes = [int64]$_.Length
                extension = if ([string]::IsNullOrWhiteSpace($_.Extension)) { "[none]" } else { $_.Extension.ToLowerInvariant() }
                last_write_time_utc = $_.LastWriteTimeUtc.ToString("o")
            }
        }
)

$oldestWrite = $null
$newestWrite = $null
if ($files.Count -gt 0) {
    $oldestWrite = ($files | Sort-Object LastWriteTimeUtc | Select-Object -First 1).LastWriteTimeUtc.ToString("o")
    $newestWrite = ($files | Sort-Object LastWriteTimeUtc -Descending | Select-Object -First 1).LastWriteTimeUtc.ToString("o")
}

$snapshot = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    app = $App
    cache_name = $CacheName
    phase = $Phase
    cache_path = $resolvedCachePath
    cache_exists = $cacheExists
    file_count = $files.Count
    directory_count = $directories.Count
    total_bytes = $totalBytes
    oldest_file_write_utc = $oldestWrite
    newest_file_write_utc = $newestWrite
    extension_summary = $extensionGroups
    largest_files = $largestFiles
    decision = "cache snapshot recorded; not a benchmark comparison"
}

$artifactName = "{0}-{1}-{2}.cache.json" -f (ConvertTo-Slug $App), (ConvertTo-Slug $CacheName), (ConvertTo-Slug $Phase)
$snapshotPath = Join-Path $scenarioDir $artifactName
$snapshot | ConvertTo-Json -Depth 10 | Set-Content -Path $snapshotPath -Encoding utf8

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
$cacheArtifacts = Ensure-ObjectProperty -Object $rawArtifacts -Name "cache_snapshots"
$cacheKey = "{0}_{1}_{2}" -f (ConvertTo-Slug $App), (ConvertTo-Slug $CacheName), (ConvertTo-Slug $Phase)
Set-JsonProperty -Object $cacheArtifacts -Name $cacheKey -Value $snapshotPath
$cacheSummary = Ensure-ObjectProperty -Object $summary -Name "cache"
Set-JsonProperty -Object $cacheSummary -Name $cacheKey -Value ([pscustomobject]$snapshot)
Set-JsonProperty -Object $summary -Name "decision" -Value "cache snapshot recorded; not compared"
$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Cache snapshot: $App/$CacheName/$Phase"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Snapshot artifact: $snapshotPath"
    Add-Content -Path $notesPath -Value "- Cache path: $resolvedCachePath"
    Add-Content -Path $notesPath -Value "- File count: $($files.Count)"
    Add-Content -Path $notesPath -Value "- Total bytes: $totalBytes"
    Add-Content -Path $notesPath -Value "- Decision: cache snapshot recorded; not compared"
}

Write-Output $snapshotPath
