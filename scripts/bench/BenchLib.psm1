<#
.SYNOPSIS
Shared helpers for the PowerShell benchmark artifact scripts in scripts/bench.

.DESCRIPTION
The PowerShell counterpart of benchlib.py: JSON property access on
ConvertFrom-Json objects, slugs, the repository root, percentiles, executable
and relative-path resolution, temporary process environment variables, and
run-directory creation through new-benchmark-run.ps1.

Import it from a bench script with:
    Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

The module runs under its own strict mode and error preference because module
functions do not inherit the importing script's settings.
#>

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Get-JsonProperty {
    param(
        [Parameter(Mandatory = $true)]
        [object]$Object,

        [Parameter(Mandatory = $true)]
        [string]$Name
    )

    if ($Object -is [System.Collections.IDictionary]) {
        if ($Object.Contains($Name)) {
            return $Object[$Name]
        }
        return $null
    }

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

function Get-FirstPropertyValue {
    param(
        [Parameter(Mandatory = $true)]
        [psobject]$Object,

        [Parameter(Mandatory = $true)]
        [string[]]$Names
    )

    foreach ($name in $Names) {
        $value = Get-JsonProperty -Object $Object -Name $name
        if ($null -ne $value -and -not [string]::IsNullOrWhiteSpace([string]$value)) {
            return $value
        }
    }

    return $null
}

function ConvertTo-NullableDouble {
    param(
        [object]$Value
    )

    if ($null -eq $Value -or [string]::IsNullOrWhiteSpace([string]$Value)) {
        return $null
    }

    $parsed = 0.0
    if ([double]::TryParse(
        [string]$Value,
        [System.Globalization.NumberStyles]::Float,
        [System.Globalization.CultureInfo]::InvariantCulture,
        [ref]$parsed
    )) {
        return $parsed
    }

    return $null
}

function Resolve-RepoRoot {
    return (Resolve-Path (Join-Path $PSScriptRoot "..\..")).Path
}

function ConvertTo-Slug {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Value,

        [string]$Fallback = "value"
    )

    $slug = $Value.ToLowerInvariant() -replace "[^a-z0-9._-]+", "-"
    $slug = $slug.Trim("-")
    if ([string]::IsNullOrWhiteSpace($slug)) {
        return $Fallback
    }

    return $slug
}

# Percentile by linear interpolation between the closest ranks: with the values
# sorted ascending, rank = p / 100 * (n - 1) and the result interpolates between
# the values at floor(rank) and ceil(rank). This is numpy's default "linear"
# method and Excel's PERCENTILE.INC, and matches the Python twins. Input need
# not be sorted. Results are rounded to 6 decimal places; empty input returns
# $null.
function Get-Percentile {
    param(
        [Parameter(Mandatory = $true, Position = 0)]
        [AllowNull()]
        [AllowEmptyCollection()]
        [double[]]$Values,

        [Parameter(Mandatory = $true, Position = 1)]
        [ValidateRange(0, 100)]
        [double]$Percentile
    )

    if ($null -eq $Values -or $Values.Count -eq 0) {
        return $null
    }

    $sorted = [double[]]@($Values | Sort-Object)
    if ($sorted.Count -eq 1) {
        return [Math]::Round($sorted[0], 6)
    }

    $rank = ($Percentile / 100.0) * ($sorted.Count - 1)
    $lower = [int][Math]::Floor($rank)
    $upper = [int][Math]::Ceiling($rank)
    if ($lower -eq $upper) {
        return [Math]::Round($sorted[$lower], 6)
    }

    $weight = $rank - $lower
    $value = ($sorted[$lower] * (1.0 - $weight)) + ($sorted[$upper] * $weight)
    return [Math]::Round($value, 6)
}

function Resolve-ExecutablePath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path,

        # Names the parameter the path came from in the error message.
        [string]$Label = "Executable"
    )

    if (Test-Path $Path) {
        return (Resolve-Path $Path).Path
    }

    $command = Get-Command $Path -ErrorAction SilentlyContinue
    if ($null -ne $command -and -not [string]::IsNullOrWhiteSpace($command.Source)) {
        return $command.Source
    }

    throw "$Label '$Path' was not found as a file or executable command."
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

    $baseUri = [System.Uri]::new($baseFullPath)
    $targetUri = [System.Uri]::new([System.IO.Path]::GetFullPath($TargetPath))
    $relativeUri = $baseUri.MakeRelativeUri($targetUri)
    return [System.Uri]::UnescapeDataString($relativeUri.ToString()).Replace("/", [System.IO.Path]::DirectorySeparatorChar)
}

# Runs git and returns its trimmed output, or $null when git fails. With
# -RepoRoot the command runs against that checkout instead of the current
# directory.
function Invoke-GitValue {
    param(
        [string]$RepoRoot = "",

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    if ([string]::IsNullOrWhiteSpace($RepoRoot)) {
        $output = & git @Arguments 2>$null
    } else {
        $output = & git -C $RepoRoot @Arguments 2>$null
    }
    if ($LASTEXITCODE -ne 0) {
        return $null
    }

    return (($output | Out-String).Trim())
}

# Sets a process environment variable and records its previous value in
# $Previous (once per name) so Restore-Env can put it back.
function Set-TemporaryEnv {
    param(
        [Parameter(Mandatory = $true)]
        [hashtable]$Previous,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [AllowEmptyString()]
        [string]$Value
    )

    if (-not $Previous.ContainsKey($Name)) {
        $Previous[$Name] = [Environment]::GetEnvironmentVariable($Name, "Process")
    }

    [Environment]::SetEnvironmentVariable($Name, $Value, "Process")
}

function Restore-Env {
    param(
        [Parameter(Mandatory = $true)]
        [hashtable]$Previous
    )

    # [NullString]::Value removes the variable; a plain $null would be converted
    # to "" and leave it defined but empty.
    foreach ($entry in $Previous.GetEnumerator()) {
        if ($null -eq $entry.Value) {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, [NullString]::Value, "Process")
        } else {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, [string]$entry.Value, "Process")
        }
    }
}

# Returns the run directory for a bench script: -RunDir when given (created if
# missing), otherwise a new directory from new-benchmark-run.ps1. $Metadata
# holds new-benchmark-run.ps1 parameters (Scenario, OutputRoot, EchoPath,
# EchoBuildType, CacheState, NetworkState, FixtureName, Notes). Scenario is
# required; other null or blank entries are dropped, which is equivalent to
# passing them because every optional initializer parameter defaults to "".
function Initialize-BenchRunDirectory {
    param(
        [Parameter(Mandatory = $true)]
        [string]$RepoRoot,

        [AllowEmptyString()]
        [string]$RunDir = "",

        [Parameter(Mandatory = $true)]
        [hashtable]$Metadata
    )

    if (-not [string]::IsNullOrWhiteSpace($RunDir)) {
        return (New-Item -ItemType Directory -Force -Path $RunDir).FullName
    }

    if (-not $Metadata.ContainsKey("Scenario") -or [string]::IsNullOrWhiteSpace([string]$Metadata["Scenario"])) {
        throw "Scenario is required when RunDir is not provided."
    }

    $initializer = Join-Path $RepoRoot "scripts\bench\new-benchmark-run.ps1"
    if (-not (Test-Path $initializer)) {
        throw "Benchmark run initializer not found: $initializer"
    }

    $initializerParameters = @{}
    foreach ($entry in $Metadata.GetEnumerator()) {
        if ($null -ne $entry.Value -and -not [string]::IsNullOrWhiteSpace([string]$entry.Value)) {
            $initializerParameters[[string]$entry.Key] = [string]$entry.Value
        }
    }

    $output = & $initializer @initializerParameters
    $path = (($output | Select-Object -Last 1) | Out-String).Trim()
    if ([string]::IsNullOrWhiteSpace($path) -or -not (Test-Path $path)) {
        throw "Benchmark run initializer did not return an existing path."
    }

    return (Resolve-Path $path).Path
}

Export-ModuleMember -Function @(
    "Get-JsonProperty",
    "Set-JsonProperty",
    "Ensure-ObjectProperty",
    "Get-FirstPropertyValue",
    "ConvertTo-NullableDouble",
    "Resolve-RepoRoot",
    "ConvertTo-Slug",
    "Get-Percentile",
    "Resolve-ExecutablePath",
    "ConvertTo-RelativePath",
    "Invoke-GitValue",
    "Set-TemporaryEnv",
    "Restore-Env",
    "Initialize-BenchRunDirectory"
)
