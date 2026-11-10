<#
.SYNOPSIS
Guard: every Echo-owned native GPUI window must open through
components::ui::open_native_window, which installs the shared, bounded URI
image cache at the window root.

.DESCRIPTION
Scans src/ for window-opening calls and compares them with this script's
explicit caller list; update that list in the same change that adds or moves a
window. -SelfTest first proves the scanner against built-in fixtures.
#>
[CmdletBinding()]
param(
    [switch]$SelfTest
)

$ErrorActionPreference = "Stop"

$repositoryRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")).Path
$sourceRoot = Join-Path $repositoryRoot "src"
$wrapperRelativePath = "components/ui/native_window_root.rs"
$wrapperPath = Join-Path $sourceRoot $wrapperRelativePath
$directOpenWindowPattern = "\.open_window\s*\("
$wrapperCallPattern = "\bopen_native_window\s*\("

if (-not (Test-Path -LiteralPath $wrapperPath)) {
    throw "Missing shared native-window wrapper: $wrapperRelativePath"
}

$wrapperSource = Get-Content -LiteralPath $wrapperPath -Raw
foreach ($requiredPattern in @(
    "pub fn open_native_window",
    "\.image_cache\(crate::cache::uri_layer::provider\(cx\)\)"
)) {
    if ($wrapperSource -notmatch $requiredPattern) {
        throw "The native-window wrapper is missing required URI-cache behavior: $requiredPattern"
    }
}

if ($SelfTest) {
    $negativeFixture = "cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| View));"
    if ($negativeFixture -notmatch $directOpenWindowPattern) {
        throw "Internal negative fixture did not match a forbidden direct open_window call."
    }
}

$directCalls = @()
$wrapperCalls = @()
Get-ChildItem -LiteralPath $sourceRoot -Recurse -File -Filter "*.rs" | ForEach-Object {
    $relativePath = [IO.Path]::GetRelativePath($sourceRoot, $_.FullName) -replace "\\", "/"
    $source = Get-Content -LiteralPath $_.FullName -Raw
    if ($relativePath -ne $wrapperRelativePath -and $source -match $directOpenWindowPattern) {
        $directCalls += $relativePath
    }
    if ($relativePath -ne $wrapperRelativePath -and $source -match $wrapperCallPattern) {
        $wrapperCalls += $relativePath
    }
}

if ($directCalls.Count -gt 0) {
    throw "Direct GPUI open_window calls bypass the shared URI-cache root: $($directCalls -join ', ')"
}

$expectedCurrentCallers = @(
    "main.rs",
    "components/chat/channel_popup.rs",
    "components/debug_popup.rs",
    "components/streamer_overlay/window.rs",
    "components/overlays/emote_picker/popout.rs",
    "components/overlays/media_inspector.rs",
    "components/overlays/user_card_popout.rs",
    "components/settings/settings_popout.rs"
)
$missingCallers = @($expectedCurrentCallers | Where-Object { $_ -notin $wrapperCalls })
if ($missingCallers.Count -gt 0) {
    throw "Current native-window callsites do not use open_native_window: $($missingCallers -join ', ')"
}

$unexpectedCallers = @($wrapperCalls | Where-Object { $_ -notin $expectedCurrentCallers })
if ($unexpectedCallers.Count -gt 0) {
    throw "New native-window callers must be added to this contract deliberately: $($unexpectedCallers -join ', ')"
}

Write-Host "PASS: all $($wrapperCalls.Count) Echo native-window callsites use the shared URI-cache root."
