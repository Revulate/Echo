# Recreate the local GPUI fork that Echo's [patch."https://github.com/zed-industries/zed"]
# section in Cargo.toml points at.
#
# Why a fork: exact 7TV name-paint rendering needs glyph coverage masks for shaped
# lines, and GPUI keeps glyph rasterization (raster_bounds / rasterize_glyph /
# RenderGlyphParams) pub(crate). Patching cargo's git checkout in place does not
# survive a re-fetch; the fork + [patch] section is durable. See docs/GPUI_FORK.md.
#
# Usage: pwsh scripts/setup-gpui-fork.ps1 [-ForkPath <path>]

param(
    [string]$ForkPath
)

$ErrorActionPreference = "Stop"

# Keep in sync with the GPUI revision and patch series pinned in Cargo.toml.
$PinnedRev = "9a8ab54db4f9f9af4dc6efb42b304c55c6a62bd3"
$ExpectedForkTree = "7a802743da3a4ba05833b47153b01f2cf567904f"
$Branch = "echo-paint-fork"
$RepoRoot = Split-Path -Parent $PSScriptRoot
$PatchManifest = Join-Path $RepoRoot "scripts/gpui-fork-patches.txt"
$PatchFiles = Get-Content -LiteralPath $PatchManifest |
    ForEach-Object { $_.Trim() } |
    Where-Object { $_ -and -not $_.StartsWith('#') } |
    ForEach-Object { Join-Path $RepoRoot $_ }
if ($PatchFiles.Count -eq 0) {
    throw "GPUI patch manifest is empty: $PatchManifest"
}
if (-not $ForkPath) {
    $ForkPath = Join-Path (Split-Path -Parent $RepoRoot) "zed-gpui-fork"
}
$ForkPath = [System.IO.Path]::GetFullPath($ForkPath)

$CargoRev = Select-String -Path (Join-Path $RepoRoot "Cargo.toml") -Pattern 'rev = "([0-9a-f]{40})"' |
    Select-Object -First 1 | ForEach-Object { $_.Matches[0].Groups[1].Value }
if ($CargoRev -and $CargoRev -ne $PinnedRev) {
    throw "Cargo.toml pins gpui rev $CargoRev but this script expects $PinnedRev. Update both together."
}

if (-not (Test-Path (Join-Path $ForkPath ".git"))) {
    Write-Host "Cloning zed into $ForkPath (blob-filtered, this is large)..."
    git clone --filter=blob:none https://github.com/zed-industries/zed $ForkPath
    if ($LASTEXITCODE -ne 0) { throw "git clone failed" }
}

Push-Location $ForkPath
try {
    git rev-parse --verify --quiet "$Branch" *> $null
    if ($LASTEXITCODE -eq 0) {
        git checkout $Branch
        if ($LASTEXITCODE -ne 0) { throw "git checkout $Branch failed" }
        $Head = git rev-parse HEAD
        if ($LASTEXITCODE -ne 0) { throw "git rev-parse HEAD failed" }
        $Tree = git rev-parse 'HEAD^{tree}'
        if ($LASTEXITCODE -ne 0) { throw "git rev-parse HEAD tree failed" }
        if ($Tree -ne $ExpectedForkTree) {
            throw "Existing $Branch has tree $Tree; expected $ExpectedForkTree. Move or repair the checkout before building Echo."
        }
        $TrackedChanges = git status --porcelain --untracked-files=no
        if ($LASTEXITCODE -ne 0) { throw "git status failed" }
        if ($TrackedChanges) {
            throw "The GPUI fork contains tracked working-tree changes. Restore or commit them before building Echo."
        }
        Write-Host "GPUI fork ready at $ForkPath ($Head, tree $Tree)."
        return
    }

    git cat-file -e "$PinnedRev^{commit}" *> $null
    if ($LASTEXITCODE -ne 0) {
        git fetch origin $PinnedRev
        if ($LASTEXITCODE -ne 0) { throw "git fetch of pinned rev failed" }
    }

    git checkout $PinnedRev
    if ($LASTEXITCODE -ne 0) { throw "git checkout of pinned rev failed" }
    git checkout -b $Branch
    if ($LASTEXITCODE -ne 0) { throw "git checkout -b $Branch failed" }

    foreach ($PatchFile in $PatchFiles) {
        Write-Host "Applying $PatchFile..."
        git am $PatchFile
        if ($LASTEXITCODE -ne 0) { throw "git am failed; run 'git am --abort' in $ForkPath and inspect $PatchFile" }
    }

    $Head = git rev-parse HEAD
    if ($LASTEXITCODE -ne 0) { throw "git rev-parse HEAD failed" }
    $Tree = git rev-parse 'HEAD^{tree}'
    if ($LASTEXITCODE -ne 0 -or $Tree -ne $ExpectedForkTree) {
        throw "Reconstructed fork tree is $Tree; expected $ExpectedForkTree. Regenerate the patch series."
    }
    git log --oneline -2
    Write-Host "GPUI fork ready at $ForkPath ($Head, tree $Tree)."
}
finally {
    Pop-Location
}
