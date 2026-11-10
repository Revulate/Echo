<#
.SYNOPSIS
Validate benchmark artifact directories.
validate-benchmark-run.py is the cross-platform counterpart.
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidateNotNullOrEmpty()]
    [string]$RunDir,

    [string]$OutputFile = "",

    [switch]$RequireEcho,
    [switch]$RequireEchoBaseline,
    [switch]$RequireEchoStartup,
    [switch]$RequireChatReplay,
    [switch]$RequireCacheSnapshot,
    [switch]$RequireCacheComparison,
    [switch]$RequireRenderFixture,
    [switch]$RequireFixtureSuite,
    [switch]$RequireFrameTrace,
    [switch]$RequireGpuTrace,
    [switch]$RequireConnectionTrace,
    [switch]$RequireLongSessionSamples,
    [switch]$RequireCleanupSamples,
    [switch]$RequireEchoProfile,

    [ValidateRange(1, 1000000)]
    [int]$MinimumSamples = 1
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"
Import-Module (Join-Path $PSScriptRoot 'BenchLib.psm1') -Force -DisableNameChecking

function Add-Check {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [Parameter(Mandatory = $true)]
        [bool]$Passed,

        [string]$Detail = "",
        [string]$Path = ""
    )

    $Checks.Add([pscustomobject]@{
        name = $Name
        passed = $Passed
        detail = $Detail
        path = $Path
    }) | Out-Null
}

function Test-RawSampleArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [object]$PathValue
    )

    if ($null -eq $PathValue -or [string]::IsNullOrWhiteSpace([string]$PathValue)) {
        Add-Check -Checks $Checks -Name "$Name path present" -Passed $false -Detail "Missing raw artifact path."
        return
    }

    $path = [string]$PathValue
    $exists = Test-Path $path
    Add-Check -Checks $Checks -Name "$Name raw file exists" -Passed $exists -Path $path
    if (-not $exists) {
        return
    }

    try {
        $rows = @(Import-Csv $path)
        $sampleCount = $rows.Count
        Add-Check `
            -Checks $Checks `
            -Name "$Name raw file has minimum samples" `
            -Passed ($sampleCount -ge $MinimumSamples) `
            -Detail "samples=$sampleCount minimum=$MinimumSamples" `
            -Path $path
    } catch {
        Add-Check `
            -Checks $Checks `
            -Name "$Name raw file can be parsed" `
            -Passed $false `
            -Detail $_.Exception.Message `
            -Path $path
    }
}

function Test-JsonArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [object]$PathValue
    )

    if ($null -eq $PathValue -or [string]::IsNullOrWhiteSpace([string]$PathValue)) {
        Add-Check -Checks $Checks -Name "$Name path present" -Passed $false -Detail "Missing JSON artifact path."
        return $null
    }

    $path = [string]$PathValue
    $exists = Test-Path $path
    Add-Check -Checks $Checks -Name "$Name file exists" -Passed $exists -Path $path
    if (-not $exists) {
        return $null
    }

    try {
        $json = Get-Content -Raw $path | ConvertFrom-Json
        Add-Check -Checks $Checks -Name "$Name parses as JSON" -Passed $true -Path $path
        return $json
    } catch {
        Add-Check -Checks $Checks -Name "$Name parses as JSON" -Passed $false -Detail $_.Exception.Message -Path $path
        return $null
    }
}

function Test-NdjsonArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [string]$Name,

        [object]$PathValue
    )

    if ($null -eq $PathValue -or [string]::IsNullOrWhiteSpace([string]$PathValue)) {
        Add-Check -Checks $Checks -Name "$Name path present" -Passed $false -Detail "Missing NDJSON artifact path."
        return $null
    }

    $path = [string]$PathValue
    $exists = Test-Path $path
    Add-Check -Checks $Checks -Name "$Name file exists" -Passed $exists -Path $path
    if (-not $exists) {
        return $null
    }

    $lineCount = 0
    $firstLine = $null
    $lastLine = $null
    $reader = [System.IO.File]::OpenText($path)
    try {
        while ($true) {
            $line = $reader.ReadLine()
            if ($null -eq $line) {
                break
            }

            if ([string]::IsNullOrWhiteSpace($line)) {
                continue
            }

            if ($null -eq $firstLine) {
                $firstLine = $line
            }
            $lastLine = $line
            $lineCount += 1
        }
    } finally {
        $reader.Close()
    }

    Add-Check `
        -Checks $Checks `
        -Name "$Name has minimum records" `
        -Passed ($lineCount -ge $MinimumSamples) `
        -Detail "records=$lineCount minimum=$MinimumSamples" `
        -Path $path

    foreach ($sample in @(
        [pscustomobject]@{ Label = "first"; Line = $firstLine },
        [pscustomobject]@{ Label = "last"; Line = $lastLine }
    )) {
        if ($null -eq $sample.Line) {
            Add-Check -Checks $Checks -Name "$Name $($sample.Label) record present" -Passed $false -Path $path
            continue
        }

        try {
            $sample.Line | ConvertFrom-Json | Out-Null
            Add-Check -Checks $Checks -Name "$Name $($sample.Label) record parses as JSON" -Passed $true -Path $path
        } catch {
            Add-Check `
                -Checks $Checks `
                -Name "$Name $($sample.Label) record parses as JSON" `
                -Passed $false `
                -Detail $_.Exception.Message `
                -Path $path
        }
    }

    return [pscustomobject]@{
        path = $path
        line_count = $lineCount
    }
}

function Test-CacheSnapshotArtifacts {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [object]$CacheSnapshots
    )

    if ($null -eq $CacheSnapshots) {
        Add-Check -Checks $Checks -Name "cache snapshots present" -Passed $false -Detail "Missing raw_artifacts.cache_snapshots."
        return
    }

    $snapshotProperties = @($CacheSnapshots.PSObject.Properties)
    Add-Check `
        -Checks $Checks `
        -Name "cache snapshots registered" `
        -Passed ($snapshotProperties.Count -gt 0) `
        -Detail "count=$($snapshotProperties.Count)"

    foreach ($snapshotProperty in $snapshotProperties) {
        $snapshotName = $snapshotProperty.Name
        $snapshotJson = Test-JsonArtifact `
            -Checks $Checks `
            -Name "cache snapshot $snapshotName" `
            -PathValue $snapshotProperty.Value

        if ($null -eq $snapshotJson) {
            continue
        }

        $schemaVersion = Get-JsonProperty -Object $snapshotJson -Name "schema_version"
        Add-Check `
            -Checks $Checks `
            -Name "cache snapshot $snapshotName has schema" `
            -Passed ($null -ne $schemaVersion -and [int]$schemaVersion -ge 1) `
            -Detail "schema_version=$schemaVersion"

        foreach ($requiredTextField in @("app", "cache_name", "phase", "cache_path")) {
            $value = Get-JsonProperty -Object $snapshotJson -Name $requiredTextField
            Add-Check `
                -Checks $Checks `
                -Name "cache snapshot $snapshotName has $requiredTextField" `
                -Passed (-not [string]::IsNullOrWhiteSpace([string]$value)) `
                -Detail "$requiredTextField=$value"
        }

        $cacheExists = Get-JsonProperty -Object $snapshotJson -Name "cache_exists"
        Add-Check `
            -Checks $Checks `
            -Name "cache snapshot $snapshotName has cache_exists" `
            -Passed ($null -ne $cacheExists) `
            -Detail "cache_exists=$cacheExists"

        foreach ($requiredNumberField in @("file_count", "directory_count", "total_bytes")) {
            $value = Get-JsonProperty -Object $snapshotJson -Name $requiredNumberField
            $isValidNumber = $false
            if ($null -ne $value) {
                try {
                    $isValidNumber = [int64]$value -ge 0
                } catch {
                    $isValidNumber = $false
                }
            }
            Add-Check `
                -Checks $Checks `
                -Name "cache snapshot $snapshotName has nonnegative $requiredNumberField" `
                -Passed $isValidNumber `
                -Detail "$requiredNumberField=$value"
        }

        $decision = Get-JsonProperty -Object $snapshotJson -Name "decision"
        Add-Check `
            -Checks $Checks `
            -Name "cache snapshot $snapshotName records non-comparison decision" `
            -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not a benchmark comparison") `
            -Detail "decision=$decision"
    }
}

function Test-CacheComparisonArtifacts {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [object]$CacheComparisons
    )

    if ($null -eq $CacheComparisons) {
        Add-Check -Checks $Checks -Name "cache comparisons present" -Passed $false -Detail "Missing raw_artifacts.cache_comparisons."
        return
    }

    $comparisonProperties = @($CacheComparisons.PSObject.Properties)
    Add-Check `
        -Checks $Checks `
        -Name "cache comparisons registered" `
        -Passed ($comparisonProperties.Count -gt 0) `
        -Detail "count=$($comparisonProperties.Count)"

    foreach ($comparisonProperty in $comparisonProperties) {
        $comparisonName = $comparisonProperty.Name
        $comparisonJson = Test-JsonArtifact `
            -Checks $Checks `
            -Name "cache comparison $comparisonName" `
            -PathValue $comparisonProperty.Value

        if ($null -eq $comparisonJson) {
            continue
        }

        $schemaVersion = Get-JsonProperty -Object $comparisonJson -Name "schema_version"
        Add-Check `
            -Checks $Checks `
            -Name "cache comparison $comparisonName has schema" `
            -Passed ($null -ne $schemaVersion -and [int]$schemaVersion -ge 1) `
            -Detail "schema_version=$schemaVersion"

        foreach ($requiredTextField in @("app", "cache_name", "before_snapshot", "after_snapshot", "before_phase", "after_phase")) {
            $value = Get-JsonProperty -Object $comparisonJson -Name $requiredTextField
            Add-Check `
                -Checks $Checks `
                -Name "cache comparison $comparisonName has $requiredTextField" `
                -Passed (-not [string]::IsNullOrWhiteSpace([string]$value)) `
                -Detail "$requiredTextField=$value"
        }

        foreach ($snapshotField in @("before_snapshot", "after_snapshot")) {
            $snapshotPath = Get-JsonProperty -Object $comparisonJson -Name $snapshotField
            $snapshotExists = -not [string]::IsNullOrWhiteSpace([string]$snapshotPath) -and (Test-Path ([string]$snapshotPath))
            Add-Check `
                -Checks $Checks `
                -Name "cache comparison $comparisonName $snapshotField exists" `
                -Passed $snapshotExists `
                -Path ([string]$snapshotPath)
        }

        $delta = Get-JsonProperty -Object $comparisonJson -Name "delta"
        Add-Check `
            -Checks $Checks `
            -Name "cache comparison $comparisonName has delta" `
            -Passed ($null -ne $delta)

        if ($null -ne $delta) {
            foreach ($deltaField in @("file_count", "directory_count", "total_bytes")) {
                $value = Get-JsonProperty -Object $delta -Name $deltaField
                $isNumber = $false
                if ($null -ne $value) {
                    try {
                        [int64]$value | Out-Null
                        $isNumber = $true
                    } catch {
                        $isNumber = $false
                    }
                }
                Add-Check `
                    -Checks $Checks `
                    -Name "cache comparison $comparisonName has numeric delta $deltaField" `
                    -Passed $isNumber `
                    -Detail "$deltaField=$value"
            }
        }

        $decision = Get-JsonProperty -Object $comparisonJson -Name "decision"
        Add-Check `
            -Checks $Checks `
            -Name "cache comparison $comparisonName records non-performance decision" `
            -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app performance comparison") `
            -Detail "decision=$decision"
    }
}

function Test-RenderFixtureArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    Test-NdjsonArtifact -Checks $Checks -Name "render fixture" -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "render_fixture") | Out-Null
    $manifestJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "render fixture manifest" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "render_fixture_manifest")
    if ($null -eq $manifestJson) {
        return
    }

    $fixtureKind = Get-JsonProperty -Object $manifestJson -Name "fixture_kind"
    Add-Check `
        -Checks $Checks `
        -Name "render fixture manifest has kind" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$fixtureKind)) `
        -Detail "fixture_kind=$fixtureKind"

    $messagesPath = Get-JsonProperty -Object $manifestJson -Name "messages_path"
    $messagesPathExists = -not [string]::IsNullOrWhiteSpace([string]$messagesPath) -and (Test-Path ([string]$messagesPath))
    Add-Check `
        -Checks $Checks `
        -Name "render fixture manifest messages path exists" `
        -Passed $messagesPathExists `
        -Path ([string]$messagesPath)

    $layout = Get-JsonProperty -Object $manifestJson -Name "layout"
    Add-Check -Checks $Checks -Name "render fixture manifest has layout" -Passed ($null -ne $layout)
    if ($null -ne $layout) {
        $splitCount = Get-JsonProperty -Object $layout -Name "split_count"
        Add-Check `
            -Checks $Checks `
            -Name "render fixture layout has split count" `
            -Passed ($null -ne $splitCount -and [int]$splitCount -gt 0) `
            -Detail "split_count=$splitCount"
    }

    $decision = Get-JsonProperty -Object $manifestJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "render fixture records non-measured decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not measured") `
        -Detail "decision=$decision"
}

function Test-FixtureSuiteArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $manifestJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "fixture suite manifest" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "fixture_suite_manifest")
    if ($null -eq $manifestJson) {
        return
    }

    $chatFixtures = @(Get-JsonProperty -Object $manifestJson -Name "chat_fixtures")
    $renderFixtures = @(Get-JsonProperty -Object $manifestJson -Name "render_fixtures")
    Add-Check `
        -Checks $Checks `
        -Name "fixture suite has chat fixtures" `
        -Passed ($chatFixtures.Count -gt 0) `
        -Detail "count=$($chatFixtures.Count)"
    Add-Check `
        -Checks $Checks `
        -Name "fixture suite has render fixtures" `
        -Passed ($renderFixtures.Count -gt 0) `
        -Detail "count=$($renderFixtures.Count)"

    $allFixtures = @($chatFixtures + $renderFixtures)
    foreach ($fixture in $allFixtures) {
        $scenario = Get-JsonProperty -Object $fixture -Name "scenario"
        $kind = Get-JsonProperty -Object $fixture -Name "kind"
        $label = if (-not [string]::IsNullOrWhiteSpace([string]$scenario)) {
            "chat $scenario"
        } else {
            "render $kind"
        }

        foreach ($pathField in @("run_dir", "manifest_path", "validation_path")) {
            $pathValue = Get-JsonProperty -Object $fixture -Name $pathField
            $exists = -not [string]::IsNullOrWhiteSpace([string]$pathValue) -and (Test-Path ([string]$pathValue))
            Add-Check `
                -Checks $Checks `
                -Name "fixture suite $label $pathField exists" `
                -Passed $exists `
                -Path ([string]$pathValue)
        }

        $validationPassed = Get-JsonProperty -Object $fixture -Name "validation_passed"
        Add-Check `
            -Checks $Checks `
            -Name "fixture suite $label validation passed" `
            -Passed ([bool]$validationPassed) `
            -Detail "validation_passed=$validationPassed"
    }

    $suiteValidationPassed = Get-JsonProperty -Object $manifestJson -Name "validation_passed"
    Add-Check `
        -Checks $Checks `
        -Name "fixture suite records passing child validations" `
        -Passed ([bool]$suiteValidationPassed) `
        -Detail "validation_passed=$suiteValidationPassed"

    $decision = Get-JsonProperty -Object $manifestJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "fixture suite records non-measured decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not measured") `
        -Detail "decision=$decision"
}

function Test-EchoProfileArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $profileJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "echo profile run" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "echo_profile_run")
    if ($null -eq $profileJson) {
        return
    }

    $app = Get-JsonProperty -Object $profileJson -Name "app"
    Add-Check -Checks $Checks -Name "echo profile run has app echo" -Passed ([string]$app -eq "echo") -Detail "app=$app"

    $profileName = Get-JsonProperty -Object $profileJson -Name "profile_name"
    Add-Check `
        -Checks $Checks `
        -Name "echo profile run has profile name" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$profileName)) `
        -Detail "profile_name=$profileName"

    $launchPath = Get-JsonProperty -Object $profileJson -Name "launch_path"
    Add-Check `
        -Checks $Checks `
        -Name "echo profile run has launch path" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$launchPath)) `
        -Detail "launch_path=$launchPath"

    $artifacts = Get-JsonProperty -Object $profileJson -Name "artifacts"
    foreach ($artifactName in @("stdout", "stderr", "log")) {
        $artifactPath = if ($null -ne $artifacts) { Get-JsonProperty -Object $artifacts -Name $artifactName } else { $null }
        Add-Check `
            -Checks $Checks `
            -Name "echo profile $artifactName artifact exists" `
            -Passed ($null -ne $artifactPath -and (Test-Path ([string]$artifactPath))) `
            -Path ([string]$artifactPath)
    }

    foreach ($rawKey in @("echo_profile_stdout", "echo_profile_stderr", "echo_profile_log")) {
        $artifactPath = Get-JsonProperty -Object $RawArtifacts -Name $rawKey
        Add-Check `
            -Checks $Checks `
            -Name "echo profile raw_artifacts has $rawKey" `
            -Passed ($null -ne $artifactPath -and (Test-Path ([string]$artifactPath))) `
            -Path ([string]$artifactPath)
    }

    $environment = Get-JsonProperty -Object $profileJson -Name "environment"
    Add-Check -Checks $Checks -Name "echo profile run has environment" -Passed ($null -ne $environment)
    if ($null -ne $environment) {
        foreach ($key in @(
            "ECHO_FPS_PROFILER",
            "ECHO_PERF_MONITOR",
            "ECHO_LOG_FILE",
            "ECHO_LOG_FILE_PATH",
            "ECHO_LOG_PERF",
            "ECHO_LOG_PERF_RENDER",
            "ECHO_LOG_PERF_EMOTES",
            "ECHO_LOG_IRC",
            "ECHO_LOG_EMOTES",
            "ECHO_LOG_NET",
            "ECHO_LOG_UI"
        )) {
            $value = Get-JsonProperty -Object $environment -Name $key
            Add-Check `
                -Checks $Checks `
                -Name "echo profile env has $key" `
                -Passed (-not [string]::IsNullOrWhiteSpace([string]$value)) `
                -Detail "$key=$value"
        }
    }

    $stop = Get-JsonProperty -Object $profileJson -Name "stop"
    Add-Check -Checks $Checks -Name "echo profile run has stop object" -Passed ($null -ne $stop)
    if ($null -ne $stop) {
        $method = Get-JsonProperty -Object $stop -Name "method"
        Add-Check `
            -Checks $Checks `
            -Name "echo profile stop method is known" `
            -Passed ([string]$method -in @("close-main-window", "kill", "leave-running", "already-exited")) `
            -Detail "method=$method"
    }

    $decision = Get-JsonProperty -Object $profileJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "echo profile run records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not compared") `
        -Detail "decision=$decision"
}

function Test-FrameTraceArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $frameSummaryJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "frame trace summary" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "frame_trace")
    if ($null -eq $frameSummaryJson) {
        return
    }

    $frameCount = Get-JsonProperty -Object $frameSummaryJson -Name "frame_count"
    Add-Check `
        -Checks $Checks `
        -Name "frame trace summary has frames" `
        -Passed ($null -ne $frameCount -and [int]$frameCount -ge $MinimumSamples) `
        -Detail "frame_count=$frameCount minimum=$MinimumSamples"

    foreach ($metric in @("average_ms", "p50_ms", "p95_ms", "p99_ms", "max_ms")) {
        $value = Get-JsonProperty -Object $frameSummaryJson -Name $metric
        $isNumber = $false
        if ($null -ne $value) {
            try {
                [double]$value | Out-Null
                $isNumber = $true
            } catch {
                $isNumber = $false
            }
        }

        Add-Check `
            -Checks $Checks `
            -Name "frame trace summary has numeric $metric" `
            -Passed $isNumber `
            -Detail "$metric=$value"
    }

    $tracePath = Get-JsonProperty -Object $frameSummaryJson -Name "trace_path"
    $traceExists = -not [string]::IsNullOrWhiteSpace([string]$tracePath) -and (Test-Path ([string]$tracePath))
    Add-Check `
        -Checks $Checks `
        -Name "frame trace source exists" `
        -Passed $traceExists `
        -Path ([string]$tracePath)

    $decision = Get-JsonProperty -Object $frameSummaryJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "frame trace summary records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app comparison") `
        -Detail "decision=$decision"
}

function Test-GpuTraceArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $gpuSummaryJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "GPU trace summary" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "gpu_trace")
    if ($null -eq $gpuSummaryJson) {
        return
    }

    $sampleCount = Get-JsonProperty -Object $gpuSummaryJson -Name "sample_count"
    Add-Check `
        -Checks $Checks `
        -Name "GPU trace summary has samples" `
        -Passed ($null -ne $sampleCount -and [int]$sampleCount -ge $MinimumSamples) `
        -Detail "sample_count=$sampleCount minimum=$MinimumSamples"

    $metricCount = Get-JsonProperty -Object $gpuSummaryJson -Name "metric_count"
    Add-Check `
        -Checks $Checks `
        -Name "GPU trace summary has metrics" `
        -Passed ($null -ne $metricCount -and [int]$metricCount -gt 0) `
        -Detail "metric_count=$metricCount"

    $metrics = Get-JsonProperty -Object $gpuSummaryJson -Name "metrics"
    Add-Check `
        -Checks $Checks `
        -Name "GPU trace summary metrics object present" `
        -Passed ($null -ne $metrics)

    if ($null -ne $metrics) {
        $metricProperties = @($metrics.PSObject.Properties)
        Add-Check `
            -Checks $Checks `
            -Name "GPU trace summary has metric entries" `
            -Passed ($metricProperties.Count -gt 0) `
            -Detail "metric_entries=$($metricProperties.Count)"

        foreach ($metricProperty in $metricProperties) {
            $metricName = $metricProperty.Name
            $metric = $metricProperty.Value
            $metricSampleCount = Get-JsonProperty -Object $metric -Name "sample_count"
            Add-Check `
                -Checks $Checks `
                -Name "GPU trace $metricName has minimum samples" `
                -Passed ($null -ne $metricSampleCount -and [int]$metricSampleCount -ge $MinimumSamples) `
                -Detail "sample_count=$metricSampleCount minimum=$MinimumSamples"

            foreach ($field in @("avg", "p95", "max")) {
                $value = Get-JsonProperty -Object $metric -Name $field
                $isNumber = $false
                if ($null -ne $value) {
                    try {
                        [double]$value | Out-Null
                        $isNumber = $true
                    } catch {
                        $isNumber = $false
                    }
                }

                Add-Check `
                    -Checks $Checks `
                    -Name "GPU trace $metricName has numeric $field" `
                    -Passed $isNumber `
                    -Detail "$field=$value"
            }
        }
    }

    $tracePath = Get-JsonProperty -Object $gpuSummaryJson -Name "trace_path"
    $traceExists = -not [string]::IsNullOrWhiteSpace([string]$tracePath) -and (Test-Path ([string]$tracePath))
    Add-Check `
        -Checks $Checks `
        -Name "GPU trace source exists" `
        -Passed $traceExists `
        -Path ([string]$tracePath)

    $decision = Get-JsonProperty -Object $gpuSummaryJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "GPU trace summary records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app comparison") `
        -Detail "decision=$decision"
}

function Test-ConnectionTraceArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $connectionJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "connection trace summary" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "connection_trace")
    if ($null -eq $connectionJson) {
        return
    }

    $eventCount = Get-JsonProperty -Object $connectionJson -Name "event_count"
    Add-Check `
        -Checks $Checks `
        -Name "connection trace summary has events" `
        -Passed ($null -ne $eventCount -and [int]$eventCount -ge $MinimumSamples) `
        -Detail "event_count=$eventCount minimum=$MinimumSamples"

    foreach ($metric in @("disconnect_count", "reconnect_attempt_count", "reconnect_complete_count", "duplicate_message_count", "dropped_message_count")) {
        $value = Get-JsonProperty -Object $connectionJson -Name $metric
        $isNumber = $false
        if ($null -ne $value) {
            try {
                [int64]$value | Out-Null
                $isNumber = $true
            } catch {
                $isNumber = $false
            }
        }

        Add-Check `
            -Checks $Checks `
            -Name "connection trace summary has numeric $metric" `
            -Passed $isNumber `
            -Detail "$metric=$value"
    }

    $tracePath = Get-JsonProperty -Object $connectionJson -Name "trace_path"
    $traceExists = -not [string]::IsNullOrWhiteSpace([string]$tracePath) -and (Test-Path ([string]$tracePath))
    Add-Check `
        -Checks $Checks `
        -Name "connection trace source exists" `
        -Passed $traceExists `
        -Path ([string]$tracePath)

    $decision = Get-JsonProperty -Object $connectionJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "connection trace summary records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app comparison") `
        -Detail "decision=$decision"
}

function Test-LongSessionSamplesArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $longSessionJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "long-session samples summary" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "long_session_samples")
    if ($null -eq $longSessionJson) {
        return
    }

    $sampleCount = Get-JsonProperty -Object $longSessionJson -Name "sample_count"
    Add-Check `
        -Checks $Checks `
        -Name "long-session summary has minimum samples" `
        -Passed ($null -ne $sampleCount -and [int]$sampleCount -ge $MinimumSamples) `
        -Detail "sample_count=$sampleCount minimum=$MinimumSamples"

    $durationMs = Get-JsonProperty -Object $longSessionJson -Name "observed_duration_ms"
    $durationIsValid = $false
    if ($null -ne $durationMs) {
        try {
            $durationIsValid = [double]$durationMs -gt 0
        } catch {
            $durationIsValid = $false
        }
    }
    Add-Check `
        -Checks $Checks `
        -Name "long-session summary has positive duration" `
        -Passed $durationIsValid `
        -Detail "observed_duration_ms=$durationMs"

    $samplePath = Get-JsonProperty -Object $longSessionJson -Name "sample_path"
    $samplePathExists = -not [string]::IsNullOrWhiteSpace([string]$samplePath) -and (Test-Path ([string]$samplePath))
    Add-Check `
        -Checks $Checks `
        -Name "long-session source samples exist" `
        -Passed $samplePathExists `
        -Path ([string]$samplePath)

    $metrics = Get-JsonProperty -Object $longSessionJson -Name "metrics"
    Add-Check `
        -Checks $Checks `
        -Name "long-session summary has metrics" `
        -Passed ($null -ne $metrics)

    if ($null -ne $metrics) {
        foreach ($metricName in @("working_set_bytes", "private_memory_bytes")) {
            $metric = Get-JsonProperty -Object $metrics -Name $metricName
            Add-Check `
                -Checks $Checks `
                -Name "long-session summary has $metricName" `
                -Passed ($null -ne $metric)

            if ($null -eq $metric) {
                continue
            }

            foreach ($field in @("first", "last", "max", "delta", "growth_per_minute")) {
                $value = Get-JsonProperty -Object $metric -Name $field
                $isNumber = $false
                if ($null -ne $value) {
                    try {
                        [double]$value | Out-Null
                        $isNumber = $true
                    } catch {
                        $isNumber = $false
                    }
                }

                Add-Check `
                    -Checks $Checks `
                    -Name "long-session $metricName has numeric $field" `
                    -Passed $isNumber `
                    -Detail "$field=$value"
            }
        }
    }

    $decision = Get-JsonProperty -Object $longSessionJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "long-session summary records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app comparison") `
        -Detail "decision=$decision"
}

function Test-CleanupSamplesArtifact {
    param(
        [Parameter(Mandatory = $true)]
        [AllowEmptyCollection()]
        [System.Collections.Generic.List[object]]$Checks,

        [Parameter(Mandatory = $true)]
        [psobject]$RawArtifacts
    )

    $cleanupJson = Test-JsonArtifact `
        -Checks $Checks `
        -Name "cleanup samples summary" `
        -PathValue (Get-JsonProperty -Object $RawArtifacts -Name "cleanup_samples")
    if ($null -eq $cleanupJson) {
        return
    }

    $sampleCount = Get-JsonProperty -Object $cleanupJson -Name "sample_count"
    Add-Check `
        -Checks $Checks `
        -Name "cleanup summary has minimum samples" `
        -Passed ($null -ne $sampleCount -and [int]$sampleCount -ge $MinimumSamples) `
        -Detail "sample_count=$sampleCount minimum=$MinimumSamples"

    foreach ($countField in @("before_sample_count", "after_sample_count")) {
        $countValue = Get-JsonProperty -Object $cleanupJson -Name $countField
        $hasSamples = $false
        if ($null -ne $countValue) {
            try {
                $hasSamples = [int]$countValue -gt 0
            } catch {
                $hasSamples = $false
            }
        }

        Add-Check `
            -Checks $Checks `
            -Name "cleanup summary has $countField" `
            -Passed $hasSamples `
            -Detail "$countField=$countValue"
    }

    $cleanupStartMs = Get-JsonProperty -Object $cleanupJson -Name "cleanup_start_elapsed_ms"
    $cleanupStartIsValid = $false
    if ($null -ne $cleanupStartMs) {
        try {
            $cleanupStartIsValid = [double]$cleanupStartMs -ge 0
        } catch {
            $cleanupStartIsValid = $false
        }
    }
    Add-Check `
        -Checks $Checks `
        -Name "cleanup summary has cleanup boundary" `
        -Passed $cleanupStartIsValid `
        -Detail "cleanup_start_elapsed_ms=$cleanupStartMs"

    $samplePath = Get-JsonProperty -Object $cleanupJson -Name "sample_path"
    $samplePathExists = -not [string]::IsNullOrWhiteSpace([string]$samplePath) -and (Test-Path ([string]$samplePath))
    Add-Check `
        -Checks $Checks `
        -Name "cleanup source samples exist" `
        -Passed $samplePathExists `
        -Path ([string]$samplePath)

    $metrics = Get-JsonProperty -Object $cleanupJson -Name "metrics"
    Add-Check `
        -Checks $Checks `
        -Name "cleanup summary has metrics" `
        -Passed ($null -ne $metrics)

    if ($null -ne $metrics) {
        foreach ($metricName in @("working_set_bytes", "private_memory_bytes")) {
            $metric = Get-JsonProperty -Object $metrics -Name $metricName
            Add-Check `
                -Checks $Checks `
                -Name "cleanup summary has $metricName" `
                -Passed ($null -ne $metric)

            if ($null -eq $metric) {
                continue
            }

            foreach ($field in @("delta_last", "delta_avg")) {
                $value = Get-JsonProperty -Object $metric -Name $field
                $isNumber = $false
                if ($null -ne $value) {
                    try {
                        [double]$value | Out-Null
                        $isNumber = $true
                    } catch {
                        $isNumber = $false
                    }
                }

                Add-Check `
                    -Checks $Checks `
                    -Name "cleanup $metricName has numeric $field" `
                    -Passed $isNumber `
                    -Detail "$field=$value"
            }
        }
    }

    $decision = Get-JsonProperty -Object $cleanupJson -Name "decision"
    Add-Check `
        -Checks $Checks `
        -Name "cleanup summary records non-comparison decision" `
        -Passed (-not [string]::IsNullOrWhiteSpace([string]$decision) -and [string]$decision -match "not an app comparison") `
        -Detail "decision=$decision"
}

$resolvedRunDir = (Resolve-Path $RunDir).Path
$checks = [System.Collections.Generic.List[object]]::new()

$environmentPath = Join-Path $resolvedRunDir "environment.json"
$summaryPath = Join-Path $resolvedRunDir "summary.json"
$notesPath = Join-Path $resolvedRunDir "notes.md"

$environmentExists = Test-Path $environmentPath
Add-Check -Checks $checks -Name "environment.json exists" -Passed $environmentExists -Path $environmentPath
if ($environmentExists) {
    try {
        Get-Content -Raw $environmentPath | ConvertFrom-Json | Out-Null
        Add-Check -Checks $checks -Name "environment.json parses as JSON" -Passed $true -Path $environmentPath
    } catch {
        Add-Check -Checks $checks -Name "environment.json parses as JSON" -Passed $false -Detail $_.Exception.Message -Path $environmentPath
    }
}

$summaryExists = Test-Path $summaryPath
Add-Check -Checks $checks -Name "summary.json exists" -Passed $summaryExists -Path $summaryPath
$summary = $null
if ($summaryExists) {
    try {
        $summary = Get-Content -Raw $summaryPath | ConvertFrom-Json
        Add-Check -Checks $checks -Name "summary.json parses as JSON" -Passed $true -Path $summaryPath
    } catch {
        Add-Check -Checks $checks -Name "summary.json parses as JSON" -Passed $false -Detail $_.Exception.Message -Path $summaryPath
    }
}

Add-Check -Checks $checks -Name "notes.md exists" -Passed (Test-Path $notesPath) -Path $notesPath

if ($null -ne $summary) {
    $measurementsRecorded = Get-JsonProperty -Object $summary -Name "measurements_recorded"
    $requiresMeasurement = $RequireEcho -or
        $RequireEchoStartup -or
        $RequireGpuTrace -or
        $RequireLongSessionSamples -or
        $RequireCleanupSamples -or
        $RequireEchoProfile
    if ($requiresMeasurement) {
        Add-Check `
            -Checks $checks `
            -Name "summary records measurements" `
            -Passed ([bool]$measurementsRecorded) `
            -Detail "measurements_recorded=$measurementsRecorded" `
            -Path $summaryPath
    } else {
        Add-Check `
            -Checks $checks `
            -Name "summary measurement flag is metadata-compatible" `
            -Passed $true `
            -Detail "measurements_recorded=$measurementsRecorded" `
            -Path $summaryPath
    }

    $rawArtifacts = Get-JsonProperty -Object $summary -Name "raw_artifacts"
    if ($null -eq $rawArtifacts) {
        Add-Check -Checks $checks -Name "summary raw_artifacts present" -Passed $false -Path $summaryPath
    } else {
        Add-Check -Checks $checks -Name "summary raw_artifacts present" -Passed $true -Path $summaryPath

        if ($RequireEcho) {
            Test-RawSampleArtifact -Checks $checks -Name "echo" -PathValue (Get-JsonProperty -Object $rawArtifacts -Name "echo")
        }
        if ($RequireEchoBaseline) {
            $echoBaselineJson = Test-JsonArtifact `
                -Checks $checks `
                -Name "echo baseline" `
                -PathValue (Get-JsonProperty -Object $rawArtifacts -Name "echo_baseline")
            if ($null -ne $echoBaselineJson) {
                $fileObject = Get-JsonProperty -Object $echoBaselineJson -Name "file"
                $sha = if ($null -ne $fileObject) { Get-JsonProperty -Object $fileObject -Name "sha256" } else { $null }
                Add-Check `
                    -Checks $checks `
                    -Name "echo baseline has SHA256" `
                    -Passed (-not [string]::IsNullOrWhiteSpace([string]$sha) -and [string]$sha.Length -eq 64) `
                    -Detail "sha256=$sha"
            }
        }
        if ($RequireEchoStartup) {
            Test-JsonArtifact -Checks $checks -Name "echo startup" -PathValue (Get-JsonProperty -Object $rawArtifacts -Name "echo_startup") | Out-Null
        }
        if ($RequireChatReplay) {
            Test-NdjsonArtifact -Checks $checks -Name "chat replay" -PathValue (Get-JsonProperty -Object $rawArtifacts -Name "chat_replay") | Out-Null
            $chatReplayManifest = Test-JsonArtifact `
                -Checks $checks `
                -Name "chat replay manifest" `
                -PathValue (Get-JsonProperty -Object $rawArtifacts -Name "chat_replay_manifest")
            if ($null -ne $chatReplayManifest) {
                $replayScenario = Get-JsonProperty -Object $chatReplayManifest -Name "replay_scenario"
                Add-Check `
                    -Checks $checks `
                    -Name "chat replay manifest has scenario" `
                    -Passed (-not [string]::IsNullOrWhiteSpace([string]$replayScenario)) `
                    -Detail "replay_scenario=$replayScenario"
            }
        }
        if ($RequireCacheSnapshot) {
            Test-CacheSnapshotArtifacts `
                -Checks $checks `
                -CacheSnapshots (Get-JsonProperty -Object $rawArtifacts -Name "cache_snapshots")
        }
        if ($RequireCacheComparison) {
            Test-CacheComparisonArtifacts `
                -Checks $checks `
                -CacheComparisons (Get-JsonProperty -Object $rawArtifacts -Name "cache_comparisons")
        }
        if ($RequireRenderFixture) {
            Test-RenderFixtureArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireFixtureSuite) {
            Test-FixtureSuiteArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireFrameTrace) {
            Test-FrameTraceArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireGpuTrace) {
            Test-GpuTraceArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireConnectionTrace) {
            Test-ConnectionTraceArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireLongSessionSamples) {
            Test-LongSessionSamplesArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireCleanupSamples) {
            Test-CleanupSamplesArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
        if ($RequireEchoProfile) {
            Test-EchoProfileArtifact -Checks $checks -RawArtifacts $rawArtifacts
        }
    }
}

$failedChecks = @($checks | Where-Object { -not $_.passed })
$validation = [ordered]@{
    schema_version = 1
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    run_dir = $resolvedRunDir
    passed = $failedChecks.Count -eq 0
    check_count = $checks.Count
    failed_check_count = $failedChecks.Count
    minimum_samples = $MinimumSamples
    checks = $checks
}

if ([string]::IsNullOrWhiteSpace($OutputFile)) {
    $OutputFile = Join-Path $resolvedRunDir "validation.json"
}

$validation | ConvertTo-Json -Depth 8 | Set-Content -Path $OutputFile -Encoding utf8

if ($null -ne $summary) {
    $rawArtifacts = Get-JsonProperty -Object $summary -Name "raw_artifacts"
    if ($null -eq $rawArtifacts) {
        $rawArtifacts = [pscustomobject]@{}
        Set-JsonProperty -Object $summary -Name "raw_artifacts" -Value $rawArtifacts
    }
    Set-JsonProperty -Object $rawArtifacts -Name "validation" -Value $OutputFile
    Set-JsonProperty -Object $summary -Name "validation" -Value ([pscustomobject]$validation)
    $summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8
}

if (Test-Path $notesPath) {
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "## Benchmark validation"
    Add-Content -Path $notesPath -Value ""
    Add-Content -Path $notesPath -Value "- Validation artifact: $OutputFile"
    Add-Content -Path $notesPath -Value "- Passed: $($validation.passed)"
    Add-Content -Path $notesPath -Value "- Failed checks: $($validation.failed_check_count)"
}

Write-Output $OutputFile
