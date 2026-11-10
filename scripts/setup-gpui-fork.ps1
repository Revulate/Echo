<#
.SYNOPSIS
Reconstruct, or validate, the sibling GPUI fork that Cargo.toml's
[patch."https://github.com/zed-industries/zed"] section points at.

.DESCRIPTION
The fork is Zed at the revision Cargo.toml pins, plus the patches listed in
scripts/gpui-fork-patches.txt applied in order with `git am`, checked against
that manifest's fork-tree. An existing echo-paint-fork branch is accepted only
when its source tree matches. See docs/GPUI_FORK.md for why the fork exists.
setup-gpui-fork.sh is the Bash twin; keep their checks and messages alike.

.EXAMPLE
pwsh -NoProfile -File scripts/setup-gpui-fork.ps1 [-ForkPath <path>]
#>

param(
    [string]$ForkPath
)

$ErrorActionPreference = "Stop"

# scripts/release/verify-release.ps1 reads these two literals. They duplicate
# Cargo.toml's rev and the manifest's fork-tree, and the checks below fail when
# either copy drifts.
$PinnedRev = "9a8ab54db4f9f9af4dc6efb42b304c55c6a62bd3"
$ExpectedForkTree = "7a802743da3a4ba05833b47153b01f2cf567904f"
$Branch = "echo-paint-fork"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$PatchManifest = Join-Path $RepoRoot "scripts/gpui-fork-patches.txt"
# `git am` records a committer; the tree check does not depend on it, so a fixed
# identity keeps fresh machines without one working.
$GitAmIdentity = @("-c", "user.name=Echo GPUI fork", "-c", "user.email=gpui-fork@echo.invalid")

function Fail([string]$Message) {
    throw "GPUI fork setup failed: $Message"
}

function Assert-CleanFork {
    $TrackedChanges = git -C $ForkPath status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0) { Fail "git status failed in $ForkPath" }
    if ($TrackedChanges) {
        Fail "$ForkPath contains tracked working-tree changes. Restore or commit them, then rerun."
    }
}

if (-not (Test-Path -LiteralPath $PatchManifest)) { Fail "missing patch manifest: $PatchManifest" }

$CargoRevs = @(
    Select-String -Path (Join-Path $RepoRoot "Cargo.toml") -Pattern 'rev = "([0-9a-f]{40})"' -AllMatches |
        ForEach-Object { $_.Matches } | ForEach-Object { $_.Groups[1].Value } | Sort-Object -Unique
)
if ($CargoRevs.Count -eq 0) { Fail "could not read the GPUI revision from Cargo.toml" }
if ($CargoRevs.Count -gt 1) { Fail "Cargo.toml pins more than one Zed revision: $($CargoRevs -join ' ')" }
if ($CargoRevs[0] -ne $PinnedRev) {
    Fail "Cargo.toml pins $($CargoRevs[0]) but this script's `$PinnedRev is $PinnedRev. Update both together."
}

$ManifestLines = Get-Content -LiteralPath $PatchManifest | ForEach-Object { $_.Trim() } |
    Where-Object { $_ -and -not $_.StartsWith('#') }
$ManifestTree = $ManifestLines | ForEach-Object {
    if ($_ -match '^fork-tree\s*=\s*([0-9a-f]{40})$') { $Matches[1] }
} | Select-Object -First 1
if (-not $ManifestTree) { Fail "missing or malformed fork-tree in $PatchManifest" }
if ($ManifestTree -ne $ExpectedForkTree) {
    Fail "$PatchManifest has fork-tree $ManifestTree but this script's `$ExpectedForkTree is $ExpectedForkTree. Update both together."
}

$PatchFiles = @($ManifestLines | Where-Object { $_ -notmatch '=' } | ForEach-Object { Join-Path $RepoRoot $_ })
if ($PatchFiles.Count -eq 0) { Fail "patch manifest lists no patches: $PatchManifest" }
foreach ($PatchFile in $PatchFiles) {
    if (-not (Test-Path -LiteralPath $PatchFile)) { Fail "missing patch: $PatchFile" }
}

if (-not $ForkPath) {
    $ForkPath = Join-Path (Split-Path -Parent $RepoRoot) "zed-gpui-fork"
}
$ForkPath = [System.IO.Path]::GetFullPath($ForkPath)

if (-not (Test-Path (Join-Path $ForkPath ".git"))) {
    Write-Host "Cloning Zed into $ForkPath (blob-filtered)..."
    git clone --filter=blob:none https://github.com/zed-industries/zed $ForkPath
    if ($LASTEXITCODE -ne 0) { Fail "git clone failed" }
}

Assert-CleanFork

git -C $ForkPath show-ref --verify --quiet "refs/heads/$Branch"
if ($LASTEXITCODE -eq 0) {
    git -C $ForkPath switch $Branch *> $null
    if ($LASTEXITCODE -ne 0) { Fail "git switch $Branch failed" }
    $Tree = git -C $ForkPath rev-parse 'HEAD^{tree}'
    if ($LASTEXITCODE -ne 0) { Fail "git rev-parse failed" }
    if ($Tree -ne $ExpectedForkTree) {
        Fail "existing $Branch has tree $Tree; expected $ExpectedForkTree. Preserve or rename the stale branch, then rerun."
    }
    Assert-CleanFork
    Write-Host "GPUI fork ready at $ForkPath (tree $Tree)."
    return
}

git -C $ForkPath cat-file -e "$PinnedRev^{commit}" *> $null
if ($LASTEXITCODE -ne 0) {
    git -C $ForkPath remote get-url origin *> $null
    if ($LASTEXITCODE -ne 0) { Fail "the GPUI fork has no origin remote for fetching $PinnedRev" }
    git -C $ForkPath fetch origin $PinnedRev
    if ($LASTEXITCODE -ne 0) { Fail "git fetch of $PinnedRev failed" }
}

git -C $ForkPath switch --detach $PinnedRev *> $null
if ($LASTEXITCODE -ne 0) { Fail "git switch --detach $PinnedRev failed" }
git -C $ForkPath switch -c $Branch *> $null
if ($LASTEXITCODE -ne 0) { Fail "git switch -c $Branch failed" }

foreach ($PatchFile in $PatchFiles) {
    Write-Host "Applying $PatchFile..."
    git @GitAmIdentity -C $ForkPath am $PatchFile
    if ($LASTEXITCODE -ne 0) {
        Fail "git am stopped at $PatchFile. Run git am --abort in $ForkPath and inspect the patch."
    }
}

$Tree = git -C $ForkPath rev-parse 'HEAD^{tree}'
if ($LASTEXITCODE -ne 0) { Fail "git rev-parse failed" }
if ($Tree -ne $ExpectedForkTree) {
    Fail "reconstructed tree is $Tree; expected $ExpectedForkTree. Regenerate the patch series or update fork-tree."
}
Assert-CleanFork

Write-Host "GPUI fork ready at $ForkPath (tree $Tree)."
