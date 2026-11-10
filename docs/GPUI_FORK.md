# GPUI fork

Echo builds against a reproducible local fork of Zed's GPUI instead of the
plain pinned Zed revision.

- Default fork location: sibling directory `../zed-gpui-fork` (override with
  `-ForkPath` for the PowerShell script or the first argument of the shell
  script)
- Branch: `echo-paint-fork`
- Base revision: `9a8ab54db4f9f9af4dc6efb42b304c55c6a62bd3` (the same rev pinned in
  `Cargo.toml` for `gpui`, `gpui_platform`, and `reqwest_client`)
- Expected source tree: `7a802743da3a4ba05833b47153b01f2cf567904f`
- Fork commits: 29 commits in 28 patch files, applied in the order listed by
  `scripts/gpui-fork-patches.txt`: the two-commit `patches/gpui-paint-fork.patch`,
  then
  `patches/gpui-image-features.patch`,
  `patches/gpui-gpu-timestamps.patch`,
  `patches/gpui-image-transport-retry.patch`,
  `patches/gpui-image-response-bounds.patch`,
  `patches/gpui-image-decode-bounds.patch`,
  `patches/gpui-svg-rendered-bounds.patch`,
  `patches/gpui-reqwest-runtime-bound-body.patch`,
  `patches/gpui-image-decode-budget.patch`,
  `patches/gpui-windows-window-teardown.patch`,
  `patches/gpui-windows-cursor-stability.patch`,
  `patches/gpui-asset-source-diagnostics.patch`,
  `patches/gpui-bidi-text-geometry.patch`,
  `patches/gpui-windows-atlas-retirement-stability.patch`,
  `patches/gpui-windows-device-lost-retry.patch`,
  `patches/gpui-windows-hardware-adapter-preference.patch`,
  `patches/gpui-windows-lazy-path-msaa.patch`,
  `patches/gpui-skip-transparent-glyphs.patch`,
  `patches/gpui-animated-image-patching.patch`,
  `patches/gpui-windows-minimize-vsync-recovery-fixes.patch`,
  `patches/gpui-shaped-line-inline-decorations.patch`,
  `patches/gpui-releasable-images-painted-text.patch`,
  `patches/gpui-windows-mouse-leave-exit.patch`,
  `patches/gpui-windows-ime-caret-active-end.patch`,
  `patches/gpui-text-wrap-measure-cache.patch`,
  `patches/gpui-atlas-drop-scoped-refresh.patch`,
  `patches/gpui-linux-activation-token.patch`,
  and `patches/gpui-a11y-reuse-cached-view-nodes.patch`,
  so the fork is reconstructible from this repository plus the pinned upstream
  revision. The patch files are the source of record.
- Wiring: the `[patch."https://github.com/zed-industries/zed"]` section at the bottom
  of `Cargo.toml` redirects `gpui`, `gpui_platform`, and `reqwest_client` to relative
  path dependencies inside that sibling checkout. The setup script verifies the
  patched source tree rather than silently accepting a stale branch. It does not
  require a particular commit hash because `git am` legitimately assigns a new
  committer timestamp when reconstructing the same patch series. Tracked
  working-tree changes are rejected; unrelated untracked files do not alter Cargo
  inputs.

## Why a fork

Exact 7TV name-paint rendering needs `background-clip: text` semantics:
continuous gradient and image fills clipped to the username glyphs, plus
gaussian-blurred glyph-shaped drop shadows. That requires the grayscale coverage
mask of a shaped line's glyphs, and GPUI keeps glyph rasterization internals
(`WindowTextSystem::raster_bounds`, `rasterize_glyph`, `RenderGlyphParams`)
`pub(crate)`, so the mask extraction has to live inside GPUI. The other patches
are bounded fixes and diagnostics that Echo needs from the same framework.

Patching cargo's git checkout in place does not survive cargo re-fetching it. A
fork plus a `[patch]` section is durable: cargo treats the fork as an ordinary
path dependency, and every fork commit is version-controlled here as a patch
file.

## Rebuilding the fork

```sh
./scripts/setup-gpui-fork.sh
```

```pwsh
./scripts/setup-gpui-fork.ps1
```

The script clones Zed (blob-filtered) into `../zed-gpui-fork`, checks out the pinned
revision, creates `echo-paint-fork`, applies the ordered patch series declared in
`scripts/gpui-fork-patches.txt` with `git am`, and verifies the resulting tree.
Existing forks are accepted only when their checked-out source tree exactly matches
the expected patch-series tree.

## What the fork adds

### Glyph coverage masks (`gpui-paint-fork.patch`, commit 1)

```rust
pub struct ShapedLineRasterMask {
    pub coverage: Vec<u8>,            // one coverage byte per device pixel, row-major
    pub size: Size<DevicePixels>,
    pub origin_offset: Point<Pixels>, // line origin -> mask top-left, logical px
    pub scale_factor: f32,
}

impl Window {
    pub fn rasterize_shaped_line_mask(
        &mut self,
        line: &ShapedLine,
        line_height: Pixels,
        byte_range: Option<std::ops::Range<usize>>,
    ) -> Result<Option<ShapedLineRasterMask>>;
}
```

Glyph origins are quantized to whole device pixels (no subpixel variants), so the
mask is position-independent and cacheable. `byte_range` restricts the mask to the
username inside a larger shaped line (Echo shapes `username: ` as one segment).

All fill generation (exact CSS linear/radial/repeating gradients, image textures),
gaussian shadow blurring, layer compositing, and caching live in Echo
(`src/components/chat/canvas_renderer/seventv_paint.rs`), keeping the GPUI surface
minimal so rebases stay cheap.

### Present statistics (`gpui-paint-fork.patch`, commit 2)

```rust
pub struct PresentStatistics {
    pub present_count: u32,        // DXGI PresentCount
    pub present_latency_ms: f64,  // age of the last scanned-out frame (now QPC - SyncQPCTime)
}

impl Window {
    pub fn present_statistics(&self) -> Option<PresentStatistics>;
}
```

Backs the in-app metrics overlay's real present (vsync) latency. The Windows
DirectX backend maps `IDXGISwapChain::GetFrameStatistics` (the swap chain is
flip-model, `DXGI_SWAP_EFFECT_FLIP_SEQUENTIAL`) plus `QueryPerformanceCounter`
into the struct; other platforms (and the `PlatformWindow` trait default) return
`None`. Returns `None` whenever statistics are unavailable, DXGI reports them
transiently disjoint (`DXGI_ERROR_FRAME_STATISTICS_DISJOINT`), or before the first
vsync-aligned present, never a fabricated value. Plumbing:
`DirectXRenderer::present_statistics` (`crates/gpui_windows/src/directx_renderer.rs`)
-> `PlatformWindow::present_statistics` (default `None`) -> `Window`.

### Narrow image codec features (`gpui-image-features.patch`)

Disables the `image` crate's default format bundle for the GPUI workspace and enables
only the formats referenced by GPUI's image/clipboard code: BMP, GIF, ICO, JPEG, PNG,
PNM, TIFF, and WebP. This removes unused AVIF encoding, DDS, EXR, HDR, QOI, TGA, and
Rayon code from Echo's release graph without changing a format GPUI can dispatch in
the compiled paths. Echo's own provider negotiation is separately limited to secure,
portable decode formats in the root manifest.

### Non-blocking GPU frame timestamps (`gpui-gpu-timestamps.patch`)

```rust
pub struct GpuFrameStatistics {
    pub gpu_frame_time_ms: f64,
    pub sample_count: u64,
}

impl Window {
    pub fn set_gpu_timing_enabled(&self, enabled: bool);
    pub fn gpu_frame_statistics(&self) -> Option<GpuFrameStatistics>;
}
```

The Windows Direct3D 11 backend brackets the complete GPUI frame (including the
render-target clear) with `D3D11_QUERY_TIMESTAMP` queries inside a
`D3D11_QUERY_TIMESTAMP_DISJOINT` interval. Four query triplets form a bounded ring.
Completed results are polled on later frames with
`D3D11_ASYNC_GETDATA_DONOTFLUSH`: GPUI never spins, flushes, or waits on the render
thread, and skips the current sample if all ring slots are still pending. Disjoint
or invalid results are discarded rather than fabricated.

Linux windows use the trait defaults, so timing is a no-op there and the statistics
are `None`.

Timestamp collection is diagnostic-only. Echo enables it while the performance
profiler/dashboard is active; disabling diagnostics releases the query objects.
Device-loss recovery drops queries tied to the old device and recreates the ring.

### Bounded image URI loading (`gpui-image-transport-retry.patch`, `gpui-image-response-bounds.patch`, `gpui-image-decode-bounds.patch`, `gpui-svg-rendered-bounds.patch`, `gpui-reqwest-runtime-bound-body.patch`, `gpui-image-decode-budget.patch`)

GPUI's URI image loader retries request and successful-response body transport
failures up to three total attempts with exponential backoff and URI-jitter. This
prevents one broken pooled HTTP/2 connection from permanently failing a cluster of
emote and badge assets. HTTP status failures and image/SVG decode failures remain
terminal, so missing or corrupt assets do not generate retry traffic.

Echo configures this finite-media client through
`ReqwestClient::proxy_user_agent_and_read_timeout` with a four-second idle-read
deadline. The fork adds independent terminal bounds for generic URI images: a
32 MiB response-body cap enforced through both `Content-Length` and a max+1
streaming read, plus a 30-second wall-clock deadline for each whole request/body
attempt. Raster decodes reject an axis above 8192 px, more than 64 MiB of RGBA for
one frame or one complete URI resource, or more than 1,024 frames. The limits travel
as an `ImageDecodeLimits` into the shared `decode_gif_image`/`decode_webp_image`
helpers; `Image::to_image_data` (files, clipboard) passes no limits. Echo's owned
animated-media decoder remains stricter and is still preferred for chat/provider
animation. Remote SVG rasterization receives the same 64 MiB rendered-RGBA budget
before `Pixmap::new`; filesystem and bundled SVG call paths do not receive this
remote-input policy. No long-lived streaming provider uses this GPUI image client.

Reqwest owns its connection and idle-read timer on a Tokio runtime, while GPUI
polls the returned `AsyncBody` on its independent background executor. The fork
therefore re-enters the client-owned Tokio handle for each response-body poll;
this preserves the idle timeout without letting zed-reqwest lazily construct a
Tokio timer outside a reactor.

Echo also replaces GPUI's retain-all per-root URI cache with one app-owned,
shared cache at every Echo native window root. This is an Echo-layer lifecycle
policy rather than a fork patch: completed URI images share the existing
decoded-media Static budget and can consume at most one quarter of its resolved
ceiling (a 16-64 MiB hard range), alongside a 512-entry cap, 2 in-flight cap,
and 64 bounded failure records with a 30-second retry TTL.
It accounts for every decoded `RenderImage` frame, keeps the current and
previous visible generation for every live window pinned, wakes all requesting
views after a completion or slot release, and calls `App::drop_image` when an
unreferenced LRU entry is evicted. Direct `ImageAssetLoader` use intentionally
does not call `App::remove_asset`: this path bypasses `App::loading_assets`.
The Twitch stream preview keeps a 180-second cache bucket for live freshness;
old buckets participate in the same bounded residency policy.

### Single-owner Windows window teardown (`gpui-windows-window-teardown.patch`)

GPUI revokes OLE drag/drop registration while the HWND is still valid during
`WM_DESTROY`, and records that teardown has started before either Windows or GPUI
initiates destruction. The asynchronous platform-window drop checks the same flag
and validates the HWND before issuing Win32 calls. Normal main-window closure
therefore retains GPUI's non-reentrant drop path without logging invalid-window
errors from duplicate `RevokeDragDrop` and `DestroyWindow` calls.

### Stable Windows cursor ownership (`gpui-windows-cursor-stability.patch`)

The platform broadcasts cursor-style changes to its native windows. Each window
caches the received style, but only the real topmost pointer HWND or Win32
capture owner may call `SetCursor` from that asynchronous update. `WM_SETCURSOR`
and hide-until-typing restoration then reuse the cached window style.

`pwsh -NoProfile -File scripts/check-gpui-cursor-state.ps1` executes the production
handler bodies against deterministic Win32 boundaries and checks cursor
persistence, non-owner isolation, hidden-cursor restoration and native resize
deferral. Native hover behavior still needs a manual check.

### Asset error source diagnostics (`gpui-asset-source-diagnostics.patch`)

Includes the asset source in load-failure logs so production diagnostics identify
the failing resource without changing successful asset loading.

### Platform-backed bidi text geometry (`gpui-bidi-text-geometry.patch`)

```rust
impl ShapedLine {
    pub fn visual_spans_for_range(
        &self,
        byte_range: std::ops::Range<usize>,
    ) -> SmallVec<[std::ops::Range<Pixels>; 4]>;
}
```

This additive API converts a logical UTF-8 byte range into sorted, disjoint visual
spans. On Windows, `LineLayout` stores interaction geometry independently from
glyph ink: normalized `VisualCell`s and leading/trailing `Caret` positions at each
editable DirectWrite cluster boundary. The ranges originate from DirectWrite's
cluster map and the returned `HitTestTextRange` `textPosition`/`length`, not an
enumeration of Rust scalar boundaries. Production intersects those shaper-owned
boundaries with Unicode extended-grapheme boundaries: fallback-font splits therefore
cannot expose the interior of a combining sequence, ZWJ emoji, or variation selector,
while a ligature remains grouped when DirectWrite makes it one cluster. DirectWrite
supplies cells through `IDWriteTextLayout::HitTestTextRange` and carets through
`HitTestTextPosition`; selection, pointer hit testing, and caret lookup therefore do
not infer rectangles from glyph origins. The Windows shaper also honors
`baselineOriginX`, `bidiLevel`, advances, and offsets when placing RTL glyphs, and
uses `widthIncludingTrailingWhitespace` as the line width.

`ShapedLine` clamps out-of-bounds or non-boundary offsets to enclosing UTF-8
character boundaries. `visual_spans_for_range` intersects the platform cells,
ignores zero-width cells for paint, sorts in physical order, and coalesces only
touching or overlapping spans. `x_for_index_with_affinity_checked` exposes a physical
caret side as `Option<Pixels>`; `caret_for_x` returns the logical boundary together
with its physical affinity so pointer input can round-trip the same side. A degraded
bidi line returns no spans or checked caret coordinate. The legacy scalar
`x_for_index_with_affinity` returns its explicit `Pixels::ZERO` fail-closed sentinel
in that case, never a logical-glyph coordinate; exact input and selection code must
use the checked API or `bidi_geometry_complete`. macOS and wgpu retain the
glyph-based fallback until their platform shapers expose equivalent hit-test
geometry.

Every shaped glyph also carries a monotonic source-order advance independent from
its physical ink origin. Splits (including `ShapedLineCursor`) preserve the physical
bidi glyph order and extend a fragment's physical paint extent when necessary,
rather than substituting logical positions for RTL ink. They do not claim fresh
Unicode bidi re-resolution. Wrapping is explicitly refused for any line that
contains a DirectWrite bidi run, including a geometry-degraded line: the platform
contract is exact unwrapped text, and an already-shaped paragraph must be reshaped
per chosen row before wrapped bidi can be claimed. A refused bidi wrapped layout
reports the full unwrapped physical extent, not the requested wrap width, so it
cannot understate or clip retained RTL ink. Ordinary LTR wrapping keeps its
existing path.

DirectWrite's per-cluster range and caret queries run only after the shaper reports
an odd-bidi-level run. Ordinary LTR chat therefore keeps the glyph path without
extra COM hit-test calls. UTF-16 cluster positions map through one precomputed
table rather than rescanning the string for each cluster. Range and caret queries
recover independently per cluster: two endpoint carets synthesize a missing range,
while the full direction-aware union of range cells synthesizes a missing caret
pair. A range failure with only one endpoint is not exact geometry and does not
fabricate a zero-width selection cell or opposite caret; it degrades the line just
as both-source failure does. The layout then exposes
`bidi_geometry_complete == false`, emits one bounded diagnostic warning, and the
platform geometry APIs fail closed rather than presenting partial or logical-glyph
fallback as exact bidi geometry. DirectWrite draws tabs as zero-glyph runs; the
fork emits one invisible space glyph per tab at the tab's hit-test position with
its native width, so tab runs keep glyph and caret mapping.

`crates/gpui/examples/platform_text_geometry.rs` is a Windows non-test diagnostic
using the real platform text system. It checks mixed LTR/RTL, pure LTR and RTL,
long trailing whitespace, ligature-prone text, combining marks, astral text,
variation selectors, ZWJ emoji and tabs; validates cluster/grapheme-safe
reverse-hit boundaries and affinity-preserving caret round trips; requires
descending RTL ink origins; and requires a logical mixed-direction range to produce
multiple positive visual spans. Its deterministic failure seam validates
range-only, caret-only, range-plus-single-endpoint, and both-source
recovery/degradation, including multi-cell caret recovery and checked-API failure
closure. It also asserts that bidi wrapping is refused, has the full unwrapped
width, and retains physical ink order. `visual_range_spans.rs` remains the
platform-independent fallback diagnostic.

### Atlas retirement and scoped refresh (`gpui-windows-atlas-retirement-stability.patch`, `gpui-atlas-drop-scoped-refresh.patch`)

Cached views replay primitives from the last rendered scene, so a replayed sprite
must never point at an atlas tile that was just freed or reallocated.
`Window::drop_image` therefore rebuilds cached scenes after it retires an image's
atlas tiles. The Windows DirectX atlas lookup also returns a recoverable absence
when a stale ID survives the retirement boundary or a device-loss reset; the
renderer logs that frame error and lets the scheduled refresh rebuild it instead
of panicking. Explicit GPU retirement is kept; the fork does not return to
retain-all atlas residency.

A refresh disables cached-view reuse for the next frame, and Echo releases evicted
animation frames every 250 ms (`src/utils/image_drops.rs`), so the refresh is kept
narrow. `PlatformAtlas::remove` (`AtlasState` and the DirectX, wgpu, Metal and
headless atlases) returns the removed tile. `App::drop_image` fans out to every
window, and `Window::drop_image`/`Window::drop_paint_mask` refresh only when the
window's own rendered scene (polychrome, monochrome, subpixel or painted-text
primitives) still uses one of the removed tiles.

### GPU device-lost recovery retry (`gpui-windows-device-lost-retry.patch`)

A lost Direct3D device (driver reset or TDR) is recovered with a 0.5-5 s backoff
for up to 60 s before GPUI panics, and draws are skipped while devices or resources
are missing. A window whose renderer fails to recover counts as a failed attempt
and gets the same backoff even when the shared device recovery succeeded. An
episode ends only after a successful present. Each renderer keeps the
DirectComposition choice it made at creation, so a retry cannot rebuild a plain
HWND swap chain on a `WS_EX_NOREDIRECTIONBITMAP` window.

### Hardware adapter preference (`gpui-windows-hardware-adapter-preference.patch`)

Hardware adapters are tried first; an adapter flagged
`DXGI_ADAPTER_FLAG_SOFTWARE` (WARP, Microsoft Basic Render Driver) is only a
fallback and logs a warning that rendering is software-emulated. Without this,
device-lost recovery during a driver reset could land on WARP and stay there. The
chosen adapter is logged once at info after the device exists:
`Using GPU: <name> (vendor 0x...., device 0x...., software: <bool>)`.

While the shared device is on a software adapter, the VSync thread checks about
once per second whether the DXGI adapter enumeration changed
(`IDXGIFactory1::IsCurrent`) and whether a hardware adapter is enumerable. When one
is, it sets `invalidate_devices`, so the device-lost recovery path recreates devices
on it. A return attempt that still lands on a software adapter backs off 1-60 s
until the enumeration changes. The probe runs only outside a recovery episode, so
the recovery backoff and 60 s cap are unchanged.

### Lazy path intermediate textures (`gpui-windows-lazy-path-msaa.patch`)

The full-size path intermediate texture and 4x MSAA render target are created on
the first paths batch at the current window size instead of at window creation and
on every resize. Echo does not paint GPUI paths, so these are normally never
allocated. A resize recreates them only if they already exist, and device recovery
starts without them. Path drawing is otherwise unchanged.

### Transparent glyphs skipped (`gpui-skip-transparent-glyphs.patch`)

`Window::paint_glyph` returns early when the glyph color has alpha 0, instead of
rasterizing the glyph into the atlas and emitting a sprite that draws nothing.
Echo paints username runs covered by a 7TV name paint this way. Hit testing and
layout do not depend on glyph sprites. `paint_emoji` takes no color and is
unchanged.

### Animated image patching (`gpui-animated-image-patching.patch`)

Animated content that changes only its frame does not need a window redraw.

- `Window::paint_animated_image` paints an image as a keyed slot and returns
  whether it recorded one (a fully clipped image paints nothing).
  `reuse_paint` carries slots of replayed views, and `Frame::finish` resolves
  each key to its sprite in the sorted scene.
- `Window::patch_animated_image` swaps the sprite's atlas tile in the
  presented scene and in the paint operation cached views replay, records the
  sprite bounds as damage and requests a present-only frame. It refuses when
  the key was not painted, was painted twice, the sprite was not found, the
  frame data or atlas tile is unavailable, or a draw is running.
- `PlatformWindow::set_present_damage`: the Direct3D 11 renderer presents
  patch-only frames with `IDXGISwapChain1::Present1` dirty rectangles. Every
  frame still redraws the whole back buffer, so the rectangles are only a
  compositor hint. The first present after a resize or device recovery is
  full. Other backends ignore the damage.
- `Window::frame_stage_statistics` returns cumulative CPU time per frame
  stage, view render/reuse counts, presents, partial presents, patches and
  refusals; the Direct3D 11 renderer adds upload, encode and present-call
  time (`PlatformWindow::renderer_cpu_statistics`).

### Minimize, vsync and recovery fixes (`gpui-windows-minimize-vsync-recovery-fixes.patch`)

- A second `SIZE_MINIMIZED` before restore (a DPI change or `SetWindowPos` on a
  minimized window) keeps the parked frame callback instead of replacing it with
  `None`, which left the restored window unable to draw.
- The DWM refresh-rate fallback computes durations in u128 nanoseconds instead of
  dividing by zero, and rejects periods outside 1-200 ms so the default interval
  is used.
- Device-loss recovery copies the window handles out before calling
  `SendMessageW`, instead of holding the window-list read lock that the UI thread
  takes for writing when it opens or closes a window.

### Inline decoration runs (`gpui-shaped-line-inline-decorations.patch`)

`ShapedLine` keeps 8 `DecorationRun`s inline instead of 32 (about 3 KB per shaped
line); more spill to the heap with the same behavior.

### Releasable images and painted text (`gpui-releasable-images-painted-text.patch`)

- `RenderImage::new_releasable(width, height, delay, pixels, source)`: a
  single-frame image whose CPU pixels are dropped once a window has uploaded
  them to its sprite atlas, while only one window is open (`OPEN_WINDOWS`).
  `paint_image` and `patch_animated_image` share `image_atlas_tile`, which
  borrows the resident pixels for the upload or calls `source` to rebuild them
  on a later miss (a window opened afterwards, a lost device), and skips the
  sprite when they cannot be rebuilt. `as_bytes` is `None` for these images;
  `with_pixels` and `byte_len` work for every image.
- `PaintedText` (scene primitive, 912 bytes, `#[repr(C)]` matching the HLSL
  struct) draws a 7TV-style name paint: stroke rings and a solid, linear,
  radial or image fill through a glyph mask, then drop shadows from
  precomputed weights. `PaintMask` is its 8-bit mask tile
  (`AtlasKey::PaintMask`), stacking the coverage and one block per shadow.
  `Window::paint_painted_text` paints it, optionally as an animated slot whose
  fill frame `patch_animated_image` swaps; `App::drop_paint_mask` /
  `Window::drop_paint_mask` retire masks; `PlatformWindow::supports_painted_text`
  is true only for the Direct3D 11 renderer, which draws it with the
  `painted_text` shader (mask `t0`, instances `t1`, fill `t2`). The wgpu and
  Metal renderers skip the batch, and Echo falls back to CPU-composited name
  paints there.

### Pointer leave dispatches `MouseExited` on Windows (`gpui-windows-mouse-leave-exit.patch`)

A `WM_MOUSELEAVE`/`WM_NCMOUSELEAVE` that is not a move into the window's own
non-client area (`owns_cursor`) dispatches `MouseExited` with a position outside
the window, and `Window::dispatch_event` stores that position for later hit tests.
Clearing only the hovered flag left `on_hover(false)` listeners unrun, so tooltips
and hover states stayed latched after the pointer left the window or moved onto an
overlapping popout.

### IME window at the selection's active end (`gpui-windows-ime-caret-active-end.patch`)

`retrieve_caret_position` requests the collapsed caret at the selection's active
end (the start of a reversed selection, otherwise its end) instead of the bounds of
the whole selection, whose origin is its left edge, so the candidate window opens
on the correct side.

### Exact-width text measure cache (`gpui-text-wrap-measure-cache.patch`)

`TextLayout`'s measure closure (`crates/gpui/src/elements/text.rs`) reuses its
cached size only for the same wrap width, like the existing guard for truncated
layouts. Otherwise a `MinContent` or `MaxContent` probe after a constrained measure
got the wrapped size (one word per line for a wrap width of 0), so the intrinsic
size of a `white-space: normal` text element depended on measure order. An affected
probe now reshapes, which is a line-layout-cache lookup when the text was already
shaped this frame or the previous one. Chat messages are shaped by the canvas
renderer and do not use this element.

### Window activation with a handed-over token (`gpui-linux-activation-token.patch`)

Echo's Linux single-instance lock (`src/linux_activation.rs`) forwards a second
launch's `XDG_ACTIVATION_TOKEN` to the running Echo. `Window::activate_window` can
only request a fresh token from the window's own last input serial, which KWin
refuses for a background window and answers with a taskbar attention hint.
`Window::activate_window_with_token` and `PlatformWindow::activate_with_token` hand
the forwarded token to `xdg_activation_v1.activate` on Wayland, so the compositor
raises the window for the user's launch. X11, Windows, macOS, and compositors
without `xdg_activation_v1` fall back to `activate`.

### Accessibility nodes of reused cached views (`gpui-a11y-reuse-cached-view-nodes.patch`)

`Window::reuse_prepaint` replays hitboxes, tooltips, element states, line layouts
and the dispatch subtree of a reused cached view, and `A11y::begin_frame` clears
the accessibility tree every frame. Without this patch, a reused cached view's
whole subtree (for Echo: the chat list, its row nodes and the live region) dropped
out of the tree sent to screen readers on any frame where another view
re-rendered. The finished nodes of each frame are now kept, and
`PrepaintStateIndex` records a node range that `reuse_prepaint` replays under the
current parent, together with node bounds, focus registrations and (moved) action
listeners. Nothing changes while accessibility is inactive. Upstream Zed has the
same behavior, so this patch is a candidate to upstream.

### Upstream features Echo relies on

Echo uses upstream's `WindowOptions::inactive_frame_interval` for unfocused
windows: a 16 ms main-window interval for its ordinary 20 ms (50 fps) animation
cadence, a ~33.3 ms interval with `ECHO_INACTIVE_THROTTLE=1`, and `None` with
`ECHO_INACTIVE_THROTTLE=0`. Without a cap, unfocused windows render at display
rate, so the bounded default is intentional. Echo also uses the window visibility
observer (`observe_window_visibility`) and the `on_system_sleep` hook.

## Updating the pinned GPUI rev

1. Update the `rev = ...` pins in `Cargo.toml`.
2. In the fork, fetch the new revision and rebase the series onto it:
   `git fetch origin <new-rev>` and
   `git rebase --onto <new-rev> <old-rev> echo-paint-fork`. Resolve conflicts
   (`rasterize_shaped_line_mask` follows `paint_glyph`'s internals; compare against
   the upstream `paint_glyph` when it drifts). Build and test the fork's affected
   crates.
3. Regenerate each patch file from its rebased commit with
   `git format-patch -1 <commit> --stdout > <echo>/patches/<name>.patch`
   (`gpui-paint-fork.patch` holds two commits:
   `git format-patch <new-rev>..<second-commit> --stdout`). Update
   `scripts/gpui-fork-patches.txt` if the series layout changes.
4. Update the pinned revision and expected tree in `scripts/setup-gpui-fork.ps1`,
   `scripts/setup-gpui-fork.sh` and this document, then confirm that both setup
   scripts reconstruct that tree from a clean checkout.
