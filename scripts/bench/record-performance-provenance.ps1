[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunDir,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$EchoExecutable,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string[]]$CacheTree,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CpuEvidencePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$CpuLaunchPath,

    [string[]]$CpuLaunchArgument = @(),

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$GpuEvidencePath,

    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$GpuLaunchPath,

    [string[]]$GpuLaunchArgument = @(),

    [string]$OutputFile = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Get-Sha256Identity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    $item = Get-Item -LiteralPath $Path -Force -ErrorAction Stop
    if ($item.PSIsContainer) {
        throw "Expected a file but received a directory: $Path"
    }

    return [ordered]@{
        path = $item.FullName
        sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $item.FullName).Hash.ToLowerInvariant()
        bytes = [int64]$item.Length
        last_write_time_utc = $item.LastWriteTimeUtc.ToString("o")
    }
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
        $baseFullPath += [System.IO.Path]::DirectorySeparatorChar
    }

    $relativeUri = ([System.Uri]::new($baseFullPath)).MakeRelativeUri([System.Uri]::new([System.IO.Path]::GetFullPath($TargetPath)))
    return [System.Uri]::UnescapeDataString($relativeUri.ToString()).Replace("/", [System.IO.Path]::DirectorySeparatorChar)
}

function Get-CacheTreeIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Specification
    )

    $separatorIndex = $Specification.IndexOf("=")
    if ($separatorIndex -le 0 -or $separatorIndex -eq ($Specification.Length - 1)) {
        throw "CacheTree entries must be written as '<label>=<existing directory>'; received '$Specification'."
    }

    $label = $Specification.Substring(0, $separatorIndex).Trim()
    $cachePath = $Specification.Substring($separatorIndex + 1).Trim()
    if ([string]::IsNullOrWhiteSpace($label) -or [string]::IsNullOrWhiteSpace($cachePath)) {
        throw "CacheTree entries must have a non-empty label and path; received '$Specification'."
    }
    if (-not (Test-Path -LiteralPath $cachePath -PathType Container)) {
        throw "Cache tree '$label' does not exist or is not a directory: $cachePath"
    }

    $resolvedPath = (Resolve-Path -LiteralPath $cachePath).Path
    $files = @(Get-ChildItem -LiteralPath $resolvedPath -File -Force -Recurse -ErrorAction Stop | Sort-Object FullName)
    $directories = @(Get-ChildItem -LiteralPath $resolvedPath -Directory -Force -Recurse -ErrorAction Stop)
    $digest = [System.Security.Cryptography.SHA256]::Create()
    $totalBytes = [int64]0
    try {
        foreach ($file in $files) {
            $relativePath = ConvertTo-RelativePath -BasePath $resolvedPath -TargetPath $file.FullName
            $fileHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash.ToLowerInvariant()
            $record = "$relativePath`0$($file.Length)`0$fileHash`n"
            $bytes = [System.Text.Encoding]::UTF8.GetBytes($record)
            $null = $digest.TransformBlock($bytes, 0, $bytes.Length, $bytes, 0)
            $totalBytes += [int64]$file.Length
        }
        $null = $digest.TransformFinalBlock(@(), 0, 0)
        $treeHash = ([System.BitConverter]::ToString($digest.Hash)).Replace("-", "").ToLowerInvariant()
    } finally {
        $digest.Dispose()
    }

    return [ordered]@{
        label = $label
        path = $resolvedPath
        sha256 = $treeHash
        file_count = $files.Count
        directory_count = $directories.Count
        total_bytes = $totalBytes
        identity_method = "SHA-256 over UTF-8 sorted relative-path, byte-count, and per-file SHA-256 records"
    }
}

function Resolve-FilePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        [Parameter(Mandatory = $true)]
        [string]$Description
    )

    if (-not (Test-Path -LiteralPath $Path -PathType Leaf)) {
        throw "$Description does not exist or is not a file: $Path"
    }
    return (Resolve-Path -LiteralPath $Path).Path
}

$resolvedRunDir = (New-Item -ItemType Directory -Force -Path $RunDir).FullName
$resolvedExecutable = Resolve-FilePath -Path $EchoExecutable -Description "Echo executable"
$resolvedCpuEvidence = Resolve-FilePath -Path $CpuEvidencePath -Description "CPU evidence artifact"
$resolvedGpuEvidence = Resolve-FilePath -Path $GpuEvidencePath -Description "GPU evidence artifact"
$resolvedCpuLaunch = Resolve-FilePath -Path $CpuLaunchPath -Description "CPU launch executable"
$resolvedGpuLaunch = Resolve-FilePath -Path $GpuLaunchPath -Description "GPU launch executable"

$executableIdentity = Get-Sha256Identity -Path $resolvedExecutable
$cacheIdentities = @($CacheTree | ForEach-Object { Get-CacheTreeIdentity -Specification $_ })
$cpuEvidence = Get-Sha256Identity -Path $resolvedCpuEvidence
$gpuEvidence = Get-Sha256Identity -Path $resolvedGpuEvidence
$cpuLaunchIdentity = Get-Sha256Identity -Path $resolvedCpuLaunch
$gpuLaunchIdentity = Get-Sha256Identity -Path $resolvedGpuLaunch

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir "performance-provenance.json"
}

$provenance = [ordered]@{
    schema_version = 1
    artifact_class = "integrated_cpu_gpu_launch_evidence"
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    passed = $true
    executable = $executableIdentity
    cache_trees = $cacheIdentities
    cpu = [ordered]@{
        evidence = $cpuEvidence
        launch = [ordered]@{
            path = $cpuLaunchIdentity.path
            sha256 = $cpuLaunchIdentity.sha256
            arguments = $CpuLaunchArgument
        }
    }
    gpu = [ordered]@{
        evidence = $gpuEvidence
        launch = [ordered]@{
            path = $gpuLaunchIdentity.path
            sha256 = $gpuLaunchIdentity.sha256
            arguments = $GpuLaunchArgument
        }
    }
    launch_identity_matches_executable = [ordered]@{
        cpu = $cpuLaunchIdentity.sha256 -eq $executableIdentity.sha256
        gpu = $gpuLaunchIdentity.sha256 -eq $executableIdentity.sha256
    }
    decision = "Executable, cache-tree, CPU-launch, and GPU-launch identities recorded; not a performance comparison."
}

$provenance | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $OutputFile -Encoding utf8
Write-Output $OutputFile
