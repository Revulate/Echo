[CmdletBinding()]
param(
    [string]$RunDir = "",
    [string]$OutputRoot = "",
    [string]$Scenario = "echo pgo build",

    [ValidateRange(1, 86400)]
    [int]$DurationSeconds = 30,

    [ValidateRange(0.0, 10000.0)]
    [double]$EchoChatReplaySpeedup = 5.0,

    [ValidateRange(1, 300)]
    [int]$ExitWaitSeconds = 10,

    [ValidateSet("close-main-window", "kill", "leave-running")]
    [string]$StopMode = "close-main-window",

    [string]$EchoChatReplay = "",
    [string[]]$LaunchArgument = @(),
    [string]$LaunchWorkingDirectory = "",
    [string]$CargoPath = "cargo",
    [ValidatePattern("^[A-Za-z0-9._-]+$")]
    [string]$CargoProfile = "dist",
    [string]$LlvmProfdataPath = "",
    [string]$ProfileDataDir = "",
    [string]$MergedProfilePath = "",
    [switch]$SkipBaselineBuild,
    [switch]$SkipOptimizedBuild,
    [switch]$DryRun,
    [switch]$SkipInstrumentedBuild,
    [switch]$ForceCleanProfileData,
    [string]$EchoBuildType = "release-pgo",
    [string]$CacheState = "preserved",
    [string]$NetworkState = "local-or-user-configured",
    [string]$FixtureName = "",
    [string]$Notes = ""
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Resolve-RepoRoot {
    $scriptRoot = Split-Path -Parent $PSCommandPath
    return (Resolve-Path (Join-Path $scriptRoot "..\..")).Path
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

    $scenarioNotes = "PGO build workflow. Instrumented and optimized builds are build artifacts, not performance evidence."
    if (-not [string]::IsNullOrWhiteSpace($Notes)) {
        $scenarioNotes = "$scenarioNotes`n$Notes"
    }

    $args = @{
        Scenario = $Scenario
        EchoBuildType = $EchoBuildType
        CacheState = $CacheState
        NetworkState = $NetworkState
        FixtureName = $FixtureName
        Notes = $scenarioNotes
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

function Resolve-CommandPath {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Command
    )

    if (Test-Path $Command) {
        return (Resolve-Path $Command).Path
    }

    $resolved = Get-Command $Command -ErrorAction SilentlyContinue
    if ($null -ne $resolved -and -not [string]::IsNullOrWhiteSpace($resolved.Source)) {
        return $resolved.Source
    }

    return $null
}

function Resolve-LlvmProfdata {
    param(
        [string]$RequestedPath
    )

    if (-not [string]::IsNullOrWhiteSpace($RequestedPath)) {
        $resolvedRequested = Resolve-CommandPath -Command $RequestedPath
        if ($null -ne $resolvedRequested) {
            return $resolvedRequested
        }
        throw "LlvmProfdataPath '$RequestedPath' was not found."
    }

    foreach ($name in @("llvm-profdata.exe", "llvm-profdata")) {
        $candidate = Resolve-CommandPath -Command $name
        if ($null -ne $candidate) {
            return $candidate
        }
    }

    $rustc = Resolve-CommandPath -Command "rustc"
    if ($null -ne $rustc) {
        try {
            $sysroot = ((& $rustc --print sysroot) | Out-String).Trim()
            if (-not [string]::IsNullOrWhiteSpace($sysroot)) {
                $rustHost = ((& rustc -vV) | Where-Object { $_ -like "host:*" } | ForEach-Object { $_.Substring(5).Trim() } | Select-Object -First 1)
                if (-not [string]::IsNullOrWhiteSpace($rustHost)) {
                    foreach ($exe in @("llvm-profdata.exe", "llvm-profdata")) {
                        $candidate = Join-Path $sysroot "lib\rustlib\$rustHost\bin\$exe"
                        if (Test-Path $candidate) {
                            return (Resolve-Path $candidate).Path
                        }
                    }
                }
            }
        } catch {
            # Keep the error message focused on the missing PGO tool below.
        }
    }

    return $null
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

    foreach ($entry in $Previous.GetEnumerator()) {
        if ($null -eq $entry.Value) {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, $null, "Process")
        } else {
            [Environment]::SetEnvironmentVariable([string]$entry.Key, [string]$entry.Value, "Process")
        }
    }
}

function Get-FileIdentity {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Path
    )

    if (-not (Test-Path $Path -PathType Leaf)) {
        return $null
    }

    $item = Get-Item $Path
    return [ordered]@{
        path = $item.FullName
        size_bytes = $item.Length
        sha256 = (Get-FileHash -Algorithm SHA256 $item.FullName).Hash
    }
}

function Invoke-LoggedCommand {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Executable,

        [Parameter(Mandatory = $true)]
        [string[]]$Arguments,

        [Parameter(Mandatory = $true)]
        [string]$WorkingDirectory,

        [Parameter(Mandatory = $true)]
        [string]$LogPath,

        [hashtable]$Environment = @{},

        [switch]$DryRunOnly
    )

    $commandLine = "$Executable $($Arguments -join ' ')"
    $record = [ordered]@{
        command = $commandLine
        executable = $Executable
        arguments = $Arguments
        working_directory = $WorkingDirectory
        log_path = $LogPath
        environment_keys = @($Environment.Keys | Sort-Object)
        started_at_utc = $null
        finished_at_utc = $null
        elapsed_seconds = $null
        exit_code = $null
        dry_run = [bool]$DryRunOnly
    }

    if ($DryRunOnly) {
        Set-Content -Path $LogPath -Value "DRY RUN: $commandLine" -Encoding utf8
        return $record
    }

    $previousEnv = @{}
    try {
        foreach ($item in $Environment.GetEnumerator()) {
            Set-TemporaryEnv -Previous $previousEnv -Name ([string]$item.Key) -Value ([string]$item.Value)
        }
        $record.started_at_utc = [DateTime]::UtcNow.ToString("o")
        $timer = [System.Diagnostics.Stopwatch]::StartNew()
        $previousErrorActionPreference = $ErrorActionPreference
        try {
            $ErrorActionPreference = "Continue"
            & $Executable @Arguments *> $LogPath
            $exitCode = $LASTEXITCODE
        } finally {
            $ErrorActionPreference = $previousErrorActionPreference
        }
        $timer.Stop()
        $record.finished_at_utc = [DateTime]::UtcNow.ToString("o")
        $record.elapsed_seconds = [Math]::Round($timer.Elapsed.TotalSeconds, 3)
        $record.exit_code = $exitCode

        if ($exitCode -ne 0) {
            throw "Command failed with exit code ${exitCode}: $commandLine. See $LogPath"
        }
    } finally {
        Restore-Env -Previous $previousEnv
    }

    return $record
}

function Stop-ProfileProcess {
    param(
        [Parameter(Mandatory = $true)]
        [System.Diagnostics.Process]$Process
    )

    if ($StopMode -eq "leave-running") {
        return [ordered]@{
            requested = $false
            method = "leave-running"
            graceful_close_requested = $false
            killed = $false
            exited_after_stop = $false
        }
    }

    $gracefulCloseRequested = $false
    $killed = $false

    try {
        $Process.Refresh()
        if ($Process.HasExited) {
            return [ordered]@{
                requested = $false
                method = "already-exited"
                graceful_close_requested = $false
                killed = $false
                exited_after_stop = $true
            }
        }

        if ($StopMode -eq "close-main-window" -and $Process.MainWindowHandle -ne [IntPtr]::Zero) {
            $gracefulCloseRequested = [bool]$Process.CloseMainWindow()
        }

        if ($gracefulCloseRequested) {
            try {
                Wait-Process -Id $Process.Id -Timeout $ExitWaitSeconds -ErrorAction Stop | Out-Null
            } catch {
                # Fall through and kill below if the window did not close.
            }
        }

        $Process.Refresh()
        if (-not $Process.HasExited) {
            Stop-Process -Id $Process.Id -Force
            $killed = $true
            try {
                Wait-Process -Id $Process.Id -Timeout $ExitWaitSeconds -ErrorAction Stop | Out-Null
            } catch {
                # Record final observed status below.
            }
        }

        $Process.Refresh()
        return [ordered]@{
            requested = $true
            method = $StopMode
            graceful_close_requested = $gracefulCloseRequested
            killed = $killed
            exited_after_stop = [bool]$Process.HasExited
        }
    } catch {
        return [ordered]@{
            requested = $true
            method = $StopMode
            graceful_close_requested = $gracefulCloseRequested
            killed = $killed
            exited_after_stop = $false
            error = $_.Exception.Message
        }
    }
}

$repoRoot = Resolve-RepoRoot
if ($StopMode -eq "leave-running" -and -not $DryRun) {
    throw "StopMode 'leave-running' is not valid for PGO because profile data must be finalized before merging."
}
if ($SkipInstrumentedBuild -and -not $SkipBaselineBuild) {
    throw "SkipInstrumentedBuild requires SkipBaselineBuild so a baseline rebuild cannot overwrite the reusable instrumented binary."
}
$scenarioDir = Initialize-RunDirectory -RepoRoot $repoRoot
$cargo = Resolve-CommandPath -Command $CargoPath
if ($null -eq $cargo) {
    throw "CargoPath '$CargoPath' was not found."
}

$profileDataRoot = if (-not [string]::IsNullOrWhiteSpace($ProfileDataDir)) {
    (New-Item -ItemType Directory -Force -Path $ProfileDataDir).FullName
} else {
    (New-Item -ItemType Directory -Force -Path (Join-Path $scenarioDir "pgo-profraw")).FullName
}

$mergedProfile = if (-not [string]::IsNullOrWhiteSpace($MergedProfilePath)) {
    $parent = Split-Path -Parent $MergedProfilePath
    if (-not [string]::IsNullOrWhiteSpace($parent)) {
        New-Item -ItemType Directory -Force -Path $parent | Out-Null
    }
    $MergedProfilePath
} else {
    Join-Path $scenarioDir "echo-pgo.profdata"
}

$artifactPaths = [ordered]@{
    baseline_build_log = Join-Path $scenarioDir "baseline-build.raw.txt"
    instrumented_build_log = Join-Path $scenarioDir "instrumented-build.raw.txt"
    profile_run_stdout = Join-Path $scenarioDir "profile-run.stdout.raw.txt"
    profile_run_stderr = Join-Path $scenarioDir "profile-run.stderr.raw.txt"
    profile_data_dir = $profileDataRoot
    merged_profile = $mergedProfile
    profdata_input_list = Join-Path $scenarioDir "profdata-input-files.txt"
    profdata_merge_log = Join-Path $scenarioDir "profdata-merge.raw.txt"
    optimized_build_log = Join-Path $scenarioDir "optimized-build.raw.txt"
    baseline_binary = Join-Path $scenarioDir "echo-baseline.exe"
    optimized_binary = Join-Path $scenarioDir "echo-pgo.exe"
    replay_report = Join-Path $scenarioDir "pgo-replay-report.json"
    run_manifest = Join-Path $scenarioDir "pgo-build-run.json"
}

$llvmProfdata = Resolve-LlvmProfdata -RequestedPath $LlvmProfdataPath
if ($null -eq $llvmProfdata -and -not $DryRun) {
    throw "llvm-profdata was not found. Install it with: rustup component add llvm-tools-preview --toolchain $(rustc --version | ForEach-Object { ($_ -split ' ')[1] })"
}

if ($ForceCleanProfileData -and (Test-Path $profileDataRoot)) {
    Remove-Item -Recurse -Force -Path $profileDataRoot
    New-Item -ItemType Directory -Force -Path $profileDataRoot | Out-Null
}

$preexistingProfileFiles = @()
if (Test-Path $profileDataRoot) {
    $preexistingProfileFiles = @(Get-ChildItem -Path $profileDataRoot -Filter "*.profraw" -File)
}
if ($preexistingProfileFiles.Count -gt 0) {
    throw "ProfileDataDir already contains $($preexistingProfileFiles.Count) .profraw files. Use ForceCleanProfileData or a fresh directory to prevent mixed training data."
}

$steps = New-Object System.Collections.Generic.List[object]
$profileOutputDirectory = if ($CargoProfile -eq "release") { "release" } else { $CargoProfile }
$buildOutputExe = Join-Path $repoRoot "target\$profileOutputDirectory\echo.exe"
$buildArguments = @("build", "--profile", $CargoProfile, "--locked", "--bin", "echo")

if (-not $SkipBaselineBuild) {
    $baselineEnv = @{ RUSTFLAGS = ""; LLVM_PROFILE_FILE = "" }
    $steps.Add((Invoke-LoggedCommand -Executable $cargo -Arguments $buildArguments -WorkingDirectory $repoRoot -LogPath $artifactPaths.baseline_build_log -Environment $baselineEnv -DryRunOnly:$DryRun))
    if (-not $DryRun) {
        if (-not (Test-Path $buildOutputExe -PathType Leaf)) { throw "Baseline Echo binary not found after build: $buildOutputExe" }
        Copy-Item -LiteralPath $buildOutputExe -Destination $artifactPaths.baseline_binary -Force
    }
}

$instrumentedEnv = @{
    RUSTFLAGS = "-Cprofile-generate=$profileDataRoot"
    LLVM_PROFILE_FILE = (Join-Path $profileDataRoot "echo-%m-%p.profraw")
}
if (-not $SkipInstrumentedBuild) {
    $steps.Add((Invoke-LoggedCommand -Executable $cargo -Arguments $buildArguments -WorkingDirectory $repoRoot -LogPath $artifactPaths.instrumented_build_log -Environment $instrumentedEnv -DryRunOnly:$DryRun))
}

$resolvedWorkingDirectory = if (-not [string]::IsNullOrWhiteSpace($LaunchWorkingDirectory)) {
    (New-Item -ItemType Directory -Force -Path $LaunchWorkingDirectory).FullName
} else {
    $repoRoot
}

$profileRun = [ordered]@{
    launched = $false
    dry_run = [bool]$DryRun
    launch_path = $buildOutputExe
    launch_arguments = $LaunchArgument
    working_directory = $resolvedWorkingDirectory
    duration_seconds = $DurationSeconds
    stop = $null
    exit_code = $null
}

if ($DryRun) {
    Set-Content -Path $artifactPaths.profile_run_stdout -Value "DRY RUN: $buildOutputExe $($LaunchArgument -join ' ')" -Encoding utf8
    Set-Content -Path $artifactPaths.profile_run_stderr -Value "" -Encoding utf8
} else {
    if (-not (Test-Path $buildOutputExe)) {
        throw "Instrumented Echo binary not found after build: $buildOutputExe"
    }

    $profileEnv = @{
        LLVM_PROFILE_FILE = (Join-Path $profileDataRoot "echo-%m-%p.profraw")
    }
    if (-not [string]::IsNullOrWhiteSpace($EchoChatReplay)) {
        $profileEnv["ECHO_CHAT_REPLAY"] = (Resolve-Path $EchoChatReplay).Path
        $profileEnv["ECHO_CHAT_REPLAY_REPORT"] = $artifactPaths.replay_report
        $profileEnv["ECHO_CHAT_REPLAY_CLEAR_CHANNEL"] = "1"
        $profileEnv["ECHO_CHAT_REPLAY_INSERT_CONTROL_MESSAGES"] = "1"
        if ($EchoChatReplaySpeedup -gt 0.0) {
            $profileEnv["ECHO_CHAT_REPLAY_SPEEDUP"] = [string]$EchoChatReplaySpeedup
        }
    }

    $previousEnv = @{}
    $process = $null
    try {
        foreach ($item in $profileEnv.GetEnumerator()) {
            Set-TemporaryEnv -Previous $previousEnv -Name ([string]$item.Key) -Value ([string]$item.Value)
        }

        $startArgs = @{
            FilePath = $buildOutputExe
            WorkingDirectory = $resolvedWorkingDirectory
            RedirectStandardOutput = $artifactPaths.profile_run_stdout
            RedirectStandardError = $artifactPaths.profile_run_stderr
            PassThru = $true
        }
        if ($LaunchArgument.Count -gt 0) {
            $startArgs.ArgumentList = $LaunchArgument
        }

        $process = Start-Process @startArgs
        $profileRun.launched = $true
        $profileRun.process_id = $process.Id
        Start-Sleep -Seconds $DurationSeconds
        $profileRun.stop = Stop-ProfileProcess -Process $process
    } finally {
        Restore-Env -Previous $previousEnv
    }

    if ($null -ne $process) {
        try {
            $process.Refresh()
            if ($process.HasExited) {
                $profileRun.exit_code = $process.ExitCode
            }
            $profileRun.process_exited = [bool]$process.HasExited
        } catch {
            $profileRun.process_exited = $true
        }
    }
    if (-not [bool]$profileRun.process_exited) {
        throw "The instrumented Echo process did not exit; refusing to merge potentially incomplete profile data."
    }
}

$replayResult = $null
if (-not $DryRun -and -not [string]::IsNullOrWhiteSpace($EchoChatReplay)) {
    if (-not (Test-Path $artifactPaths.replay_report -PathType Leaf)) {
        throw "The PGO training run did not complete its requested replay: $($artifactPaths.replay_report)"
    }
    $replayResult = Get-Content -Raw $artifactPaths.replay_report | ConvertFrom-Json
    if (-not [bool]$replayResult.replay_injected -or [int]$replayResult.inserted_messages -le 0 -or [int]$replayResult.parse_errors -ne 0 -or [int]$replayResult.skipped_events -ne 0) {
        throw "The PGO replay report is not acceptable: replay_injected=$($replayResult.replay_injected), inserted_messages=$($replayResult.inserted_messages), parse_errors=$($replayResult.parse_errors), skipped_events=$($replayResult.skipped_events)."
    }
}

$profrawFiles = @()
if (-not $DryRun -and (Test-Path $profileDataRoot)) {
    $profrawFiles = @(Get-ChildItem -Path $profileDataRoot -Filter "*.profraw" -File | ForEach-Object { $_.FullName })
    if ($profrawFiles.Count -eq 0) {
        throw "No .profraw files were written to $profileDataRoot. The profile run did not exercise the instrumented binary."
    }
}

if ($DryRun) {
    Set-Content -Path $artifactPaths.profdata_merge_log -Value "DRY RUN: llvm-profdata merge --output=$mergedProfile --input-files=$($artifactPaths.profdata_input_list)" -Encoding utf8
} else {
    $utf8NoBom = New-Object System.Text.UTF8Encoding($false)
    [System.IO.File]::WriteAllLines($artifactPaths.profdata_input_list, [string[]]$profrawFiles, $utf8NoBom)
    $mergeArgs = @("merge", "--output=$mergedProfile", "--input-files=$($artifactPaths.profdata_input_list)")
    $steps.Add((Invoke-LoggedCommand -Executable $llvmProfdata -Arguments $mergeArgs -WorkingDirectory $repoRoot -LogPath $artifactPaths.profdata_merge_log))
}

if (-not $SkipOptimizedBuild) {
    $optimizedEnv = @{
        RUSTFLAGS = "-Cprofile-use=$mergedProfile -Cllvm-args=-pgo-warn-missing-function"
        LLVM_PROFILE_FILE = ""
    }
    $steps.Add((Invoke-LoggedCommand -Executable $cargo -Arguments $buildArguments -WorkingDirectory $repoRoot -LogPath $artifactPaths.optimized_build_log -Environment $optimizedEnv -DryRunOnly:$DryRun))
    if (-not $DryRun) {
        if (-not (Test-Path $buildOutputExe -PathType Leaf)) { throw "PGO Echo binary not found after build: $buildOutputExe" }
        Copy-Item -LiteralPath $buildOutputExe -Destination $artifactPaths.optimized_binary -Force
    }
}

$echoChatReplayManifest = $null
if (-not [string]::IsNullOrWhiteSpace($EchoChatReplay)) {
    $resolvedReplay = (Resolve-Path $EchoChatReplay).Path
    $echoChatReplayManifest = [ordered]@{
        path = $resolvedReplay
        sha256 = (Get-FileHash -Algorithm SHA256 $resolvedReplay).Hash
        speedup = $EchoChatReplaySpeedup
        report = $artifactPaths.replay_report
        result = $replayResult
    }
}

$decision = "PGO-optimized Echo release build completed; compare with saved benchmark artifacts before making performance claims"
if ($DryRun) {
    $decision = "dry-run only; PGO build not executed"
} elseif ($SkipOptimizedBuild) {
    $decision = "profile generated; optimized rebuild skipped"
}

$runManifest = [ordered]@{
    schema_version = 1
    scenario = $Scenario
    created_at_utc = [DateTime]::UtcNow.ToString("o")
    repo_root = $repoRoot
    dry_run = [bool]$DryRun
    cargo = $cargo
    cargo_profile = $CargoProfile
    cargo_build_arguments = $buildArguments
    llvm_profdata = $llvmProfdata
    profile_data_dir = $profileDataRoot
    profile_data_file_count = $profrawFiles.Count
    merged_profile = $mergedProfile
    echo_chat_replay = $echoChatReplayManifest
    profile_run = $profileRun
    steps = $steps
    artifacts = $artifactPaths
    baseline_binary = Get-FileIdentity -Path $artifactPaths.baseline_binary
    optimized_binary = Get-FileIdentity -Path $artifactPaths.optimized_binary
    decision = $decision
}
$runManifest | ConvertTo-Json -Depth 12 | Set-Content -Path $artifactPaths.run_manifest -Encoding utf8

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
$metrics = Ensure-ObjectProperty -Object $summary -Name "metrics"
foreach ($entry in $artifactPaths.GetEnumerator()) {
    Set-JsonProperty -Object $rawArtifacts -Name $entry.Key -Value $entry.Value
}
Set-JsonProperty -Object $metrics -Name "pgo_build" -Value ([pscustomobject]$runManifest)
Set-JsonProperty -Object $summary -Name "measurements_recorded" -Value (-not [bool]$DryRun)
$summaryStatus = "pgo-built"
if ($DryRun) {
    $summaryStatus = "pgo-dry-run"
}

Set-JsonProperty -Object $summary -Name "status" -Value $summaryStatus
Set-JsonProperty -Object $summary -Name "decision" -Value $runManifest.decision
$summary | ConvertTo-Json -Depth 12 | Set-Content -Path $summaryPath -Encoding utf8

$notesPath = Join-Path $scenarioDir "notes.md"
$existingNotes = ""
if (Test-Path $notesPath) {
    $existingNotes = [System.IO.File]::ReadAllText($notesPath)
    $measurementText = if ($DryRun) { "false" } else { "true" }
    $existingNotes = $existingNotes -replace "(?m)^- Status: initialized\r?$", "- Status: $summaryStatus"
    $existingNotes = $existingNotes -replace "(?m)^- Measurements recorded: false\r?$", "- Measurements recorded: $measurementText"
}
$pgoNotes = [string]::Join([Environment]::NewLine, @(
    "## PGO build workflow",
    "",
    "- Manifest: $($artifactPaths.run_manifest)",
    "- Profile data dir: $profileDataRoot",
    "- Merged profile: $mergedProfile",
    "- Cargo profile: $CargoProfile",
    "- Baseline binary: $($artifactPaths.baseline_binary)",
    "- Optimized binary: $($artifactPaths.optimized_binary)",
    "- Replay report: $($artifactPaths.replay_report)",
    "- Dry run: $DryRun",
    "- Decision: $($runManifest.decision)"
))
$notesContent = $pgoNotes
if (-not [string]::IsNullOrWhiteSpace($existingNotes)) {
    $notesContent = $existingNotes.TrimEnd() + [Environment]::NewLine + [Environment]::NewLine + $pgoNotes
}
[System.IO.File]::WriteAllText($notesPath, $notesContent + [Environment]::NewLine)

Write-Output $artifactPaths.run_manifest
