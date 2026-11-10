<#
.SYNOPSIS
Builds the two retained local Echo artifacts without accumulating Cargo intermediates.

.DESCRIPTION
Without -Build, validates that Cargo resolves to this repository's target directory and
prints the exact build/retention plan. With -Build, runs one locked debug build followed
by one canonical locked dist build. After each successful profile build it retains
echo.exe, echo.pdb when emitted, and root-level runtime DLL sidecars, then removes only
that profile's regenerable build, deps, examples, incremental, and .fingerprint folders.
The dist PDB is also archived under dist/symbols/<version>/<exe sha256 prefix>/ beside a
SHA256SUMS file so crash reports from a published build can be symbolized. The archive
stays local; only echo.exe is distributed, never the PDB.
It refuses cleanup if Cargo is active, Cargo targets another directory, or a reparse point
is present under target. No global Cargo cache or any other target profile is touched.

.EXAMPLE
.\scripts\build-local.ps1
Reports the bounded workflow without building or deleting files.

.EXAMPLE
.\scripts\build-local.ps1 -Build
Builds debug and dist sequentially, prunes their regenerable intermediates, and records
SHA-256 hashes plus retained byte totals in each profile manifest.

.EXAMPLE
.\scripts\build-local.ps1 -SelfTest
Runs the isolated cleanup-safety fixture without invoking Cargo or using the repository target.
#>
[CmdletBinding()]
param(
    # Without -Build this script only verifies the configured Cargo target and prints its plan.
    [switch]$Build,

    # Exercises cleanup guards against an isolated temporary fixture. It never invokes Cargo.
    [switch]$SelfTest
)

Set-StrictMode -Version 3.0
$ErrorActionPreference = "Stop"

function Get-NormalizedFullPath {
    param([Parameter(Mandatory)][string]$Path)

    return [IO.Path]::GetFullPath($Path).TrimEnd([IO.Path]::DirectorySeparatorChar, [IO.Path]::AltDirectorySeparatorChar)
}

function Assert-ChildPath {
    param(
        [Parameter(Mandatory)][string]$Root,
        [Parameter(Mandatory)][string]$Path,
        [Parameter(Mandatory)][string]$Description
    )

    $normalizedRoot = Get-NormalizedFullPath $Root
    $normalizedPath = Get-NormalizedFullPath $Path
    $prefix = $normalizedRoot + [IO.Path]::DirectorySeparatorChar
    if (-not $normalizedPath.StartsWith($prefix, [StringComparison]::OrdinalIgnoreCase)) {
        throw "$Description escapes its allowed root: $normalizedPath"
    }
}

function Test-ReparsePoint {
    param([Parameter(Mandatory)][string]$Path)

    return (([IO.File]::GetAttributes($Path) -band [IO.FileAttributes]::ReparsePoint) -ne 0)
}

function Assert-NoReparsePoints {
    param([Parameter(Mandatory)][string]$Path)

    if (-not (Test-Path -LiteralPath $Path -PathType Container)) {
        return
    }
    if (Test-ReparsePoint $Path) {
        throw "Refusing cleanup through reparse-point directory: $Path"
    }

    $pending = [System.Collections.Generic.Stack[string]]::new()
    $pending.Push($Path)
    while ($pending.Count -gt 0) {
        $directory = $pending.Pop()
        try {
            $children = [IO.Directory]::EnumerateFileSystemEntries($directory)
            foreach ($child in $children) {
                if (Test-ReparsePoint $child) {
                    throw "Refusing cleanup because a reparse-point descendant exists: $child"
                }
                if ([IO.Directory]::Exists($child)) {
                    $pending.Push($child)
                }
            }
        }
        catch {
            throw "Unable to safely inspect cleanup path $directory. $($_.Exception.Message)"
        }
    }
}

function Assert-NoActiveCargoProcesses {
    param([scriptblock]$ProcessProbe = {
        Get-Process -ErrorAction Stop | Where-Object { $_.ProcessName -eq "cargo" }
    })

    try {
        $processes = @(& $ProcessProbe)
    }
    catch {
        throw "Unable to determine whether Cargo is active; refusing cleanup. $($_.Exception.Message)"
    }
    if ($processes.Count -gt 0) {
        $details = @($processes | ForEach-Object {
                if ($null -ne $_.Id) { "pid $($_.Id)" } else { "active cargo process" }
            }) -join ", "
        throw "Cargo is active ($details); refusing to prune target artifacts."
    }
}

function Get-CargoOutputDirectories {
    param([Parameter(Mandatory)][string]$RepositoryRoot)

    Push-Location $RepositoryRoot
    try {
        $metadata = (& cargo metadata --locked --no-deps --format-version 1 | ConvertFrom-Json)
        if ($LASTEXITCODE -ne 0 -or $null -eq $metadata -or [string]::IsNullOrWhiteSpace($metadata.target_directory) -or [string]::IsNullOrWhiteSpace($metadata.build_directory)) {
            throw "cargo metadata did not provide target_directory and build_directory."
        }
        return [pscustomobject]@{
            target = Get-NormalizedFullPath $metadata.target_directory
            build = Get-NormalizedFullPath $metadata.build_directory
            version = [string](@($metadata.packages | Where-Object { $_.name -eq 'echo_gpui' })[0].version)
        }
    }
    finally {
        Pop-Location
    }
}

function Get-RetainedArtifacts {
    param([Parameter(Mandatory)][string]$ProfileRoot)

    $executable = Join-Path $ProfileRoot "echo.exe"
    if (-not (Test-Path -LiteralPath $executable -PathType Leaf)) {
        throw "Successful Cargo build did not produce the required executable: $executable"
    }

    $artifacts = [System.Collections.Generic.List[object]]::new()
    $artifacts.Add([pscustomobject]@{ role = "executable"; path = $executable })
    $pdb = Join-Path $ProfileRoot "echo.pdb"
    if (Test-Path -LiteralPath $pdb -PathType Leaf) {
        $role = if ((Split-Path -Leaf $ProfileRoot) -eq 'dist') { "distribution-symbols" } else { "debug-symbols" }
        $artifacts.Add([pscustomobject]@{ role = $role; path = $pdb })
    }

    # Native DLLs copied beside the executable are runtime sidecars. Do not retain deps/;
    # it contains build-time artifacts rather than files used by echo.exe at launch.
    Get-ChildItem -LiteralPath $ProfileRoot -File -Filter "*.dll" | Sort-Object -Property Name | ForEach-Object {
        $artifacts.Add([pscustomobject]@{ role = "runtime-sidecar"; path = $_.FullName })
    }
    return $artifacts.ToArray()
}

function Write-ArtifactManifest {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][string]$Profile,
        [Parameter(Mandatory)][string]$ProfileRoot,
        [Parameter(Mandatory)][object[]]$Artifacts
    )

    $records = @($Artifacts | ForEach-Object {
            $item = Get-Item -LiteralPath $_.path -ErrorAction Stop
            [pscustomobject][ordered]@{
                role = $_.role
                path = [IO.Path]::GetRelativePath($RepositoryRoot, $item.FullName) -replace "\\", "/"
                bytes = $item.Length
                sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $item.FullName).Hash.ToLowerInvariant()
            }
        })
    $manifest = [ordered]@{
        schema_version = 1
        created_utc = [DateTimeOffset]::UtcNow.ToString("o")
        profile = $Profile
        retained_bytes = [Int64](($records | Measure-Object -Property bytes -Sum).Sum)
        retained_artifacts = $records
    }
    $manifestPath = Join-Path $ProfileRoot "echo.local-build-manifest.json"
    $temporaryPath = "$manifestPath.$([Guid]::NewGuid().ToString('N')).tmp"
    try {
        $manifest | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath $temporaryPath -Encoding utf8
        Move-Item -LiteralPath $temporaryPath -Destination $manifestPath -Force
    }
    finally {
        if (Test-Path -LiteralPath $temporaryPath -PathType Leaf) {
            Remove-Item -LiteralPath $temporaryPath -Force
        }
    }
    return $manifestPath
}

function Invoke-ProfileCleanup {
    param(
        [Parameter(Mandatory)][string]$TargetRoot,
        [Parameter(Mandatory)][string]$Profile,
        [Parameter(Mandatory)][object[]]$Artifacts,
        [scriptblock]$CargoProcessProbe
    )

    $profileRoot = Join-Path $TargetRoot $Profile
    Assert-ChildPath -Root $TargetRoot -Path $profileRoot -Description "Profile cleanup root"
    if (-not (Test-Path -LiteralPath $profileRoot -PathType Container)) {
        throw "Missing profile output directory: $profileRoot"
    }

    if ($PSBoundParameters.ContainsKey("CargoProcessProbe")) {
        Assert-NoActiveCargoProcesses -ProcessProbe $CargoProcessProbe
    }
    else {
        Assert-NoActiveCargoProcesses
    }
    Assert-NoReparsePoints $TargetRoot
    foreach ($artifact in $Artifacts) {
        Assert-ChildPath -Root $profileRoot -Path $artifact.path -Description "Retained artifact"
        if (-not (Test-Path -LiteralPath $artifact.path -PathType Leaf)) {
            throw "Required retained artifact disappeared before cleanup: $($artifact.path)"
        }
    }

    foreach ($directoryName in @("build", "deps", "examples", "incremental", ".fingerprint")) {
        $candidate = Join-Path $profileRoot $directoryName
        Assert-ChildPath -Root $profileRoot -Path $candidate -Description "Cleanup directory"
        if (Test-Path -LiteralPath $candidate -PathType Container) {
            Assert-NoReparsePoints $candidate
            Remove-Item -LiteralPath $candidate -Recurse -Force
        }
    }
}

function Save-DistributionSymbols {
    param(
        [Parameter(Mandatory)][string]$RepositoryRoot,
        [Parameter(Mandatory)][string]$ProfileRoot,
        [Parameter(Mandatory)][string]$Version
    )

    if ($Version -notmatch '^[0-9A-Za-z.+-]+$') {
        throw "Refusing to archive symbols for an unexpected version string: $Version"
    }
    $executable = Join-Path $ProfileRoot 'echo.exe'
    $symbols = Join-Path $ProfileRoot 'echo.pdb'
    if (-not (Test-Path -LiteralPath $symbols -PathType Leaf)) {
        Write-Warning "Dist build produced no echo.pdb; crash reports from this build cannot be symbolized."
        return $null
    }
    $executableHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $executable).Hash.ToLowerInvariant()
    $symbolsHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $symbols).Hash.ToLowerInvariant()
    $archive = Join-Path $RepositoryRoot (Join-Path 'dist\symbols' (Join-Path $Version $executableHash.Substring(0, 16)))
    Assert-ChildPath -Root $RepositoryRoot -Path $archive -Description 'Symbol archive'
    New-Item -ItemType Directory -Path $archive -Force | Out-Null
    Assert-NoReparsePoints (Join-Path $RepositoryRoot 'dist')
    Copy-Item -LiteralPath $symbols -Destination (Join-Path $archive 'echo.pdb') -Force
    Set-Content -LiteralPath (Join-Path $archive 'SHA256SUMS') -Value @("$executableHash  echo.exe", "$symbolsHash  echo.pdb") -Encoding ascii
    return $archive
}

function Invoke-SelfTest {
    $fixtureRoot = Join-Path ([IO.Path]::GetTempPath()) ("echo-build-local-selftest-" + [Guid]::NewGuid().ToString("N"))
    $temporaryRoot = Get-NormalizedFullPath ([IO.Path]::GetTempPath())
    Assert-ChildPath -Root $temporaryRoot -Path $fixtureRoot -Description "Self-test fixture"
    $junctionPath = $null
    try {
        $targetRoot = Join-Path $fixtureRoot "target"
        $profileRoot = Join-Path $targetRoot "debug"
        New-Item -ItemType Directory -Path (Join-Path $profileRoot "deps") -Force | Out-Null
        New-Item -ItemType Directory -Path (Join-Path $profileRoot "build") -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $profileRoot "echo.exe"), "test executable")
        [IO.File]::WriteAllText((Join-Path $profileRoot "echo.pdb"), "test symbols")
        [IO.File]::WriteAllText((Join-Path $profileRoot "runtime.dll"), "test sidecar")
        [IO.File]::WriteAllText((Join-Path $profileRoot "deps\regenerable.bin"), "regenerable")

        $artifacts = Get-RetainedArtifacts $profileRoot
        Invoke-ProfileCleanup -TargetRoot $targetRoot -Profile "debug" -Artifacts $artifacts -CargoProcessProbe { @() }
        if ((Test-Path -LiteralPath (Join-Path $profileRoot "deps")) -or (Test-Path -LiteralPath (Join-Path $profileRoot "build"))) {
            throw "Self-test cleanup did not remove regenerable directories."
        }
        if ($artifacts | Where-Object { -not (Test-Path -LiteralPath $_.path -PathType Leaf) }) {
            throw "Self-test cleanup removed a retained artifact."
        }
        $manifestPath = Write-ArtifactManifest -RepositoryRoot $fixtureRoot -Profile "debug" -ProfileRoot $profileRoot -Artifacts $artifacts
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        if ($manifest.retained_bytes -le 0 -or @($manifest.retained_artifacts).Count -ne 3) {
            throw "Self-test manifest did not record each retained artifact and byte total."
        }

        $distRoot = Join-Path $targetRoot 'dist'
        New-Item -ItemType Directory -Path $distRoot -Force | Out-Null
        [IO.File]::WriteAllText((Join-Path $distRoot 'echo.exe'), 'test distribution')
        [IO.File]::WriteAllText((Join-Path $distRoot 'echo.pdb'), 'optional distribution symbols')
        $distArtifacts = @(Get-RetainedArtifacts $distRoot)
        Invoke-ProfileCleanup -TargetRoot $targetRoot -Profile 'dist' -Artifacts $distArtifacts -CargoProcessProbe { @() }
        if ($distArtifacts.Count -ne 2 -or -not (Test-Path -LiteralPath (Join-Path $distRoot 'echo.pdb') -PathType Leaf)) {
            throw 'Self-test expected the dist executable and its PDB to be retained.'
        }
        $symbolArchive = Save-DistributionSymbols -RepositoryRoot $fixtureRoot -ProfileRoot $distRoot -Version '1.2.3'
        Assert-ChildPath -Root (Join-Path $fixtureRoot 'dist\symbols\1.2.3') -Path $symbolArchive -Description 'Self-test symbol archive'
        $archivedChecksums = @(Get-Content -LiteralPath (Join-Path $symbolArchive 'SHA256SUMS'))
        if (-not (Test-Path -LiteralPath (Join-Path $symbolArchive 'echo.pdb') -PathType Leaf) -or $archivedChecksums.Count -ne 2 -or $archivedChecksums[1] -notmatch '^[0-9a-f]{64}  echo\.pdb$') {
            throw 'Self-test expected the dist PDB archived beside SHA256SUMS for the executable and symbols.'
        }

        $missingOutputRejected = $false
        try {
            Get-RetainedArtifacts (Join-Path $targetRoot "missing") | Out-Null
        }
        catch {
            $missingOutputRejected = $_.Exception.Message -match "did not produce the required executable"
        }
        if (-not $missingOutputRejected) {
            throw "Self-test expected missing echo.exe to block cleanup."
        }

        $escapeRejected = $false
        try {
            Invoke-ProfileCleanup -TargetRoot $targetRoot -Profile "..\escape" -Artifacts $artifacts -CargoProcessProbe { @() }
        }
        catch {
            $escapeRejected = $_.Exception.Message -match "escapes its allowed root"
        }
        if (-not $escapeRejected) {
            throw "Self-test expected an escaping profile path to be rejected."
        }

        $junctionSource = Join-Path $fixtureRoot "junction-source"
        New-Item -ItemType Directory -Path $junctionSource -Force | Out-Null
        $junctionPath = Join-Path $profileRoot "unsafe-link"
        New-Item -ItemType Junction -Path $junctionPath -Target $junctionSource -ErrorAction Stop | Out-Null
        $reparseRejected = $false
        try {
            Invoke-ProfileCleanup -TargetRoot $targetRoot -Profile "debug" -Artifacts $artifacts -CargoProcessProbe { @() }
        }
        catch {
            $reparseRejected = $_.Exception.Message -match "reparse-point"
        }
        if (-not $reparseRejected) {
            throw "Self-test expected cleanup to reject a junction descendant."
        }

        $cargoRejected = $false
        try {
            Assert-NoActiveCargoProcesses -ProcessProbe { [pscustomobject]@{ Id = 42 } }
        }
        catch {
            $cargoRejected = $_.Exception.Message -match "Cargo is active"
        }
        if (-not $cargoRejected) {
            throw "Self-test expected cleanup to reject an active Cargo process."
        }

        $probeFailureRejected = $false
        try {
            Assert-NoActiveCargoProcesses -ProcessProbe { throw "synthetic process probe failure" }
        }
        catch {
            $probeFailureRejected = $_.Exception.Message -match "Unable to determine whether Cargo is active"
        }
        if (-not $probeFailureRejected) {
            throw "Self-test expected a process-probe failure to block cleanup."
        }
    }
    finally {
        if ($null -ne $junctionPath -and (Test-Path -LiteralPath $junctionPath) -and (Test-ReparsePoint $junctionPath)) {
            Remove-Item -LiteralPath $junctionPath -Force
        }
        if (Test-Path -LiteralPath $fixtureRoot -PathType Container) {
            Assert-ChildPath -Root $temporaryRoot -Path $fixtureRoot -Description "Self-test fixture cleanup"
            if (Test-ReparsePoint $fixtureRoot) {
                throw "Refusing to remove a reparse-point self-test fixture: $fixtureRoot"
            }
            Remove-Item -LiteralPath $fixtureRoot -Recurse -Force
        }
    }
    Write-Host "PASS: build-local cleanup safeguards and retained-artifact manifest fixture passed."
}

if ($SelfTest) {
    Invoke-SelfTest
    return
}

$repositoryRoot = Get-NormalizedFullPath (Join-Path $PSScriptRoot "..")
$expectedTargetRoot = Join-Path $repositoryRoot "target"
$effectiveDirectories = Get-CargoOutputDirectories $repositoryRoot
$normalizedExpectedTargetRoot = Get-NormalizedFullPath $expectedTargetRoot
if (-not [string]::Equals($effectiveDirectories.target, $normalizedExpectedTargetRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Cargo target_directory must be $expectedTargetRoot, but cargo metadata reports $($effectiveDirectories.target). Refusing this bounded workflow."
}
if (-not [string]::Equals($effectiveDirectories.build, $normalizedExpectedTargetRoot, [StringComparison]::OrdinalIgnoreCase)) {
    throw "Cargo build_directory must be $expectedTargetRoot, but cargo metadata reports $($effectiveDirectories.build). Refusing this bounded workflow."
}

if (-not $Build) {
    Write-Host "Dry run: Cargo target_directory and build_directory are $normalizedExpectedTargetRoot"
    Write-Host "Would run sequentially: cargo debug-build, then cargo dist-build."
    Write-Host "After each successful build, would retain echo.exe, the debug PDB when produced, and root-level runtime DLL sidecars; then prune build, deps, examples, incremental, and .fingerprint. The dist PDB is kept and archived under dist\symbols\<version> (never published)."
    Write-Host "Run .\\scripts\\build-local.ps1 -Build to perform the builds and bounded cleanup."
    return
}

# Verify the target tree before Cargo creates or changes any output. Cleanup repeats this
# check before deletion; do not start another build while this workflow is running.
if (Test-Path -LiteralPath $normalizedExpectedTargetRoot -PathType Container) {
    Assert-NoReparsePoints $normalizedExpectedTargetRoot
}

Push-Location $repositoryRoot
try {
    foreach ($profileBuild in @(
            [pscustomobject]@{ profile = "debug"; command = @("debug-build") },
            [pscustomobject]@{ profile = "dist"; command = @("dist-build") }
        )) {
        Write-Host "Building Echo $($profileBuild.profile) profile..."
        & cargo $profileBuild.command
        if ($LASTEXITCODE -ne 0) {
            throw "cargo $($profileBuild.command -join ' ') failed with exit code $LASTEXITCODE; no cleanup was performed for $($profileBuild.profile)."
        }
        $profileRoot = Join-Path $normalizedExpectedTargetRoot $profileBuild.profile
        $artifacts = Get-RetainedArtifacts $profileRoot
        Invoke-ProfileCleanup -TargetRoot $normalizedExpectedTargetRoot -Profile $profileBuild.profile -Artifacts $artifacts
        $manifestPath = Write-ArtifactManifest -RepositoryRoot $repositoryRoot -Profile $profileBuild.profile -ProfileRoot $profileRoot -Artifacts $artifacts
        $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
        Write-Host "Retained $($manifest.retained_bytes) bytes for $($profileBuild.profile): $manifestPath"
        if ($profileBuild.profile -eq 'dist') {
            $symbolArchive = Save-DistributionSymbols -RepositoryRoot $repositoryRoot -ProfileRoot $profileRoot -Version $effectiveDirectories.version
            if ($null -ne $symbolArchive) {
                Write-Host "Archived dist symbols (not for publishing): $symbolArchive"
            }
        }
    }
}
finally {
    Pop-Location
}
