# GPUI fork

Echo builds against a reproducible local fork of Zed's GPUI instead of the
plain pinned Zed revision.

- Default fork location: sibling directory `../zed-gpui-fork` (override with
  `-ForkPath` for the PowerShell script or the first argument of the shell
  script)
- Branch: `echo-paint-fork`
- Base revision: the `rev` that the root `Cargo.toml` pins for `gpui`,
  `gpui_platform` and `reqwest_client` in `[workspace.dependencies]`; every
  workspace member inherits those three dependencies from there.
- Patch series and expected source tree: `scripts/gpui-fork-patches.txt` lists
  the patch files in `patches/` (30 files, 31 commits) in application order and records the
  `fork-tree` the series must produce. The patch files are the source of
  record, so the fork is reconstructible from this repository plus the pinned
  upstream revision. The sections below describe each patch.
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
pwsh -NoProfile -File scripts/setup-gpui-fork.ps1
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

`LineLayout::split_at` splits in logical order and normalizes each half's physical
glyph origins to start at x=0 only when the line has bidi runs or glyphs out of byte
order. Every other line splits as upstream does: the prefix keeps its positions and
the suffix shifts by the boundary glyph's physical and logical x. These are exactly
the lines that `ShapedLineCursor` advances incrementally, so the cursor and
successive `split_at` calls produce the same fragments.

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
- Slots live in the scene, the way upstream tracks timed sprite animations
  (`SpriteAnimationId`). `Scene::insert_animated_image` records the slot in the
  scene's slot table and stores `index + 1` in the primitive: the
  `PolychromeSprite`'s unused `pad`, or `PaintedText::slot` (0 means no slot;
  renderers ignore both). Upstream assigns draw orders late (a primitive inside
  a layer gets its order when the outermost layer closes), so after sorting,
  `Scene::finish` walks the sorted sprites and painted texts once and records
  where each slot's primitive landed. `Scene::replay` re-inserts the slots of a
  reused cached view into the new scene. The window maps each key to its slot
  and marks a key painted twice as ambiguous.
- `Window::patch_animated_image` swaps the sprite's atlas tile in the
  presented scene and in the paint operation cached views replay, records the
  sprite bounds as damage and requests a present-only frame. It refuses when
  the key was not painted, was painted twice, the sprite was not found, the
  frame data or atlas tile is unavailable, or a draw is running.
- A patch-only frame is presented without redrawing its views, and by default
  it is presented whole. `Window::set_patch_present_damage(true)` passes the
  patched regions to the platform instead (`PlatformWindow::set_present_damage`);
  the Direct3D 11 renderer then presents with `IDXGISwapChain1::Present1` dirty
  rectangles, and other backends ignore the damage. Every frame still redraws
  the whole back buffer, so the rectangles are only a compositor hint. A
  present is always full after a resize, a device recovery or a failed draw or
  present, and while the scene runs upstream's timed scene animations, which
  move sprites outside the patched regions. Echo turns the rectangles on only
  with `ECHO_PRESENT_DIRTY_RECTS=1`, for diagnosis.
- Why whole presents are the default. One dist binary was measured on one
  Windows machine, switching only by environment variable, in ABBA order, on
  isolated warmed profiles. Arm a is dirty rectangles, arm b whole presents and
  arm c `ECHO_ANIMATION_PATCH=0`. Each arm had 4 runs, except c in the focused
  case (2). Figures are medians, with the range in brackets.
  - Busy: the perf rig, three splits at about 50 presents per second, 75 s.
    | | a | b | c |
    | --- | --- | --- | --- |
    | Process CPU, % of one core | 10.6 (9.2-15.2) | 6.4 (6.0-8.3) | 9.9 (6.3-10.9) |
    | Main thread, % of one core | 7.6 | 4.1 | 6.9 |
    | Present call, ms | 1.47 | 0.55 | 0.42 |
    | GPU 3D busy | 1.33 % | 0.17 % | 0.17 % |

    Patches had no rejections in any arm. a and b drew 140 frames per run and c
    about 3,720.
  - One animated emote, process cycles per minute over 60 s:
    | | a | b | c |
    | --- | --- | --- | --- |
    | 600x480 window, unfocused | 15.7 G (8.9-16.6) | 8.8 G (8.7-15.9) | 11.2 G (11.1-18.2) |
    | 1200x960 window, focused | 14.9 G (14.6-15.0) | 15.3 G (15.0-15.3) | 18.8 G (18.7-19.0) |
  - Idle: one animated emote, window unfocused, 240 s. Process cycles per
    minute were a 10.9 G (9.1-17.0), b 9.8 G (8.9-14.5) and c 14.6 G
    (12.7-19.2).
- The slow state.
  - Present calls of 1.5-2 ms appeared in every arm. With one emote, b reached
    it in 1 of 4 unfocused runs, against 3 of 4 for a and 1 of 4 for c; in the
    busy rig only a reached it, in all 4 runs. With the window focused, every
    arm sat at about 1.5 ms. Whole presents therefore do not avoid the state,
    but they reach it less often, and a whole present costs less in either
    state.
  - The one case where b cost more was the focused single emote: 2.4 % more
    cycles than a, with 17 % less GPU time.
- Unit tests in `crates/gpui/src/window.rs` (`animated_image_patch_tests`) paint
  slots inside nested layers and in a replayed cached view and assert that they
  resolve and patch, and that a duplicated key is refused. They also check that
  patched frames are presented whole by default without a redraw, are partial
  only with dirty rectangles on, and are always whole while scene animations
  run.
- `Window::frame_stage_statistics` returns cumulative CPU time per frame
  stage, view render/reuse counts, presents, patch-only presents
  (`patch_presents`, sent whole or as dirty rectangles), partial presents (the
  subset sent as dirty rectangles), patches and refusals. The Direct3D 11
  renderer adds upload, encode and present-call time
  (`PlatformWindow::renderer_cpu_statistics`). Echo logs them as the "GPUI
  frame stages" line (`gpui_presents`, `gpui_patch_presents`,
  `gpui_partial_presents`, `gpui_frames` for drawn frames, ...), so a patched
  frame presented whole counts in `gpui_patch_presents` and not in
  `gpui_frames`.

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
- `PaintedText` (scene primitive, 916 bytes including the `slot` field,
  `#[repr(C)]` matching the HLSL struct) draws a 7TV-style name paint: stroke rings and a solid, linear,
  radial or image fill through a glyph mask, then drop shadows from
  precomputed weights. `PaintMask` is its 8-bit mask tile
  (`AtlasKey::PaintMask`), stacking the coverage and one block per shadow.
  `Window::paint_painted_text` paints it, optionally as an animated slot whose
  fill frame `patch_animated_image` swaps; `App::drop_paint_mask` /
  `Window::drop_paint_mask` retire masks; `PlatformWindow::supports_painted_text`
  is true only for the Direct3D 11 renderer, which draws it with the
  `painted_text` shader (mask `t0`, instances `t1`, fill `t4`; upstream's
  path renderer keeps its curve and paint buffers at `t2` and `t3` for the
  whole frame). The vertex shader clips the quad to the content mask with
  upstream's `clip_to_mask`, and the fragment shader derives canvas pixels from
  `SV_Position`, so clipping does not move them. The draw uses the shared
  depth-stencil state, so opaque quads painted later still occlude it. A
  painted text never shares a draw order with a painted text it overlaps,
  because painted texts batch by texture rather than in paint order. The wgpu
  and Metal renderers skip the batch, and Echo falls back to CPU-composited
  name paints there.

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
size of a `white-space: normal` text element depended on measure order. With the
patch an affected probe reshapes, which is a line-layout-cache lookup when the text was already
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
re-rendered. The patch keeps the finished nodes of each frame, and
`PrepaintStateIndex` records a node range that `reuse_prepaint` replays under the
current parent, together with node bounds, focus registrations and (moved) action
listeners. Nothing changes while accessibility is inactive. Upstream Zed has the
same bug, so this patch is a candidate to upstream.

### Frames on demand on Windows (`gpui-windows-frame-demand.patch`)

Upstream's Windows backend invalidated every window on every vsync, so each
window got a `WM_PAINT` and ran GPUI's frame callback at the display rate even
when nothing was dirty: an idle, unfocused Echo kept the VSyncProvider thread
and the main thread awake at the refresh rate all day.

Design:

- GPUI already reports frame demand for macOS and the web: the frame waker
  (`WindowInvalidator::platform_waker`) fires when a window first becomes
  dirty, and `PlatformWindow::schedule_frame` is called for next-frame
  callbacks, patched frames waiting to be presented, running scene animations
  and, at the end of every effect flush, for each window that still has
  demand (the patch adds running scene animations to that flush condition).
  The Windows backend now implements both: they set a per-window
  `FrameDemand` (`crates/gpui_windows/src/vsync.rs`), and the vsync thread
  invalidates a window only when it has demand that is due, consuming it.
- A frame that GPUI's frame-rate throttle defers (inactive-window cap, low
  power, thermal, a scene animation's interval) is handed to the platform with
  its deadline (`PlatformWindow::schedule_frame_after`, false by default so
  other platforms keep retrying). Windows holds it until then instead of
  re-requesting every vsync. The deadline replaces the demand GPUI just
  deferred; later demand (a notify, activation, the start of high-rate input,
  which lifts the cap) makes the frame due at once.
- The backend adds demand where it needs a frame itself: a draw deferred by
  re-entrancy (and GPUI's own re-entrant deferral, which now wakes the frame
  source), `WM_SIZE` (resize, restore, DPI), `WM_SHOWWINDOW`, activation, a
  renderer resize failure, device-loss recovery, and a draw or present that
  fails because the device was removed (which also starts recovery at once).
  Direct Manipulation (touchpad) only advances a gesture when it is updated
  from a draw: the viewport's status handler requests every vsync while the
  gesture runs or coasts, and the window polls at about 30 Hz for a second
  after a contact and for 250 ms after the gesture ends.
- `draw_window` validates the window's update region before it draws, so an
  invalidation the vsync thread issues during the draw (for demand the frame
  itself re-armed) produces another `WM_PAINT` instead of being erased.
- Hidden and minimized windows keep their demand without being invalidated,
  and their requests do not wake the vsync thread; showing or restoring
  requests a frame.
- The vsync thread keeps waiting for vsyncs for 250 ms after it last delivered
  a frame (covering animation and typing gaps), then parks until a request,
  the next deferred frame or the next safety frame. Woken by a request, it
  invalidates at once rather than first waiting on `DwmFlush`.
- Demand that arrives on the UI thread when the window has not painted for a
  refresh period is painted at once (the frame waker and `schedule_frame` call
  `InvalidateRect` themselves), as with a request on every vsync, where a
  `WM_PAINT` was nearly always pending. Otherwise the vsync thread paces the
  window, so it still draws at most about once per refresh period.

Safety frame: every second each visible window is invalidated anyway. A demand
source that bypasses the paths above therefore shows up as an update in
one-second steps rather than a frozen window, and the same deadline bounds how
late a parked thread notices a lost device or runs the hardware-adapter probe.
It costs one `WM_PAINT` per visible window per second.

Opt-out: `Window::set_frame_demand_gating(false)` restores a request on every
vsync for that window and keeps the thread ticking while such a window exists.
Echo calls it for every native window from `ECHO_FRAME_DEMAND_GATING`
(`src/components/ui/native_window_root.rs`); `0` restores per-vsync frames.

Measured in the fork with debug builds of its examples on one machine (143 Hz
display), per-thread context switches from `NtQuerySystemInformation`, ABBA:

- `hello_world`, idle, visible and unfocused, 60 s: main thread 369 and 156
  switches/s and VSyncProvider 204 and 169/s before; 1.2 and 1.2/s and 2.6 and
  2.6/s after.
- `frame_demand --animate`, unfocused (33 ms inactive cap), 30 s: main 188 and
  183/s without the deadline, 97 and 97/s with it. VSyncProvider stays at about
  175/s while frames keep coming (the 250 ms wait).
- Timer notify to render (`frame_demand --fps N --report 20`, focused, mean
  ms per run): at 2 Hz, where the thread parks between frames, 6.8 and 6.5 with
  the first version of the patch, 3.9 and 4.1 with per-vsync frames, 0.15 and
  0.16 now; at 20 Hz 5.2 and 4.8, 4.4 and 3.7, and 4.2 and 3.9.

Measured in Echo, one dist binary, `ECHO_FRAME_DEMAND_GATING` unset (on) against
`0` (off), isolated profiles, ABBA, 240 s per run after 35 s of settling, the
window visible and unfocused unless stated; per run, main-thread and
VSyncProvider context switches per second and process cycles per minute:

| Scenario | on | off |
| --- | --- | --- |
| Idle, one quiet split, nothing animating | main 6.6 / 6.5, vsync 2.8 / 2.9, 1.18 / 1.20 G | main 162.6 / 162.7, vsync 218.8 / 218.7, 2.72 / 2.69 G |
| One animated emote visible | main 99.7 / 96.5, vsync 201.9 / 204.5, 12.6 / 12.7 G | main 217.8 / 217.0, vsync 207.1 / 210.3, 14.9 / 15.2 G |
| Hidden (SW_HIDE, as the tray does), three busy splits at 10 messages/s | main 49.6 / 51.4, vsync 2.8 / 2.2, 2.17 / 2.50 G | main 51.5 / 54.4, vsync 175.8 / 154.6, 2.99 / 2.85 G |

The reconnect logo pulse (a scene animation at 25 fps), unfocused, 30 s:
main 73 and 88/s on against 183/s off, with the logo still changing about 26
times a second. Keystroke in the focused composer to its pixels changing on
screen, 90 keys after 1.5-2.5 s of idle and 180 keys at 110 ms intervals per
setting over three ABBA runs: median 13.4 ms and p95 19.4 ms on against 14.0
and 20.3 ms off after idle, 12.5 and 16.3 ms against 15.1 and 23.0 ms while
typing. Without the immediate paint above, the first keystroke after idle was
slower than off (median 15.6 against 14.6 ms, p95 24.9 against 20.1 ms).
Animated emotes, hover states, tooltips, caret blink, smooth scroll, divider
drag, resize, minimize and restore (also twice), and the settings, channel
popup, user card and emote picker windows were checked on the same binary
with screen captures; none showed one-second steps, and animation patches had
no rejections.

A window hidden with `ShowWindow(SW_HIDE)` and shown again by another process
(`SW_SHOW` from outside Echo) keeps its emote animations stopped, with gating
on and off alike: GPUI reports the visibility change from `WM_SHOWWINDOW`,
before the window is visible, and when the report runs before the show
completes, Echo's canvases never hear that the window is visible again. Echo's
own tray and activation paths show the window from its UI thread, where the
report runs after the show; that path was not driven here.

Unit tests (`vsync::tests`, `animated_image_patch_tests::a_patch_wakes_the_frame_source`)
cover the demand transitions, deferred frames and which requests override
them, hidden and restored windows, an invalidation landing during a draw, the
thread's wait, sleep and safety-frame schedule, the sleeping mark across a
thread restart, and a request waking the parked thread. The `frame_demand`
example (`--tick`, `--fps`, `--animate`, `--no-gating`, `--report`) exercises
it on a real window.

Not verified on hardware: a precision touchpad (Direct Manipulation status
timing, tap cost, the transform reset), IME composition, OLE drag and drop,
monitors with different DPI or refresh rates, display and system sleep, a GPU
reset (TDR), and RDP.

### Presents after high-rate input on Apple platforms only (`gpui-high-rate-input-present-apple-only.patch`)

Upstream's `InputRateTracker` presents the unchanged frame on every frame
request for a second after high-rate input (six invalidating inputs within
100 ms). It exists for ProMotion displays, which lower their refresh rate when
an app stops presenting (zed#7305, #7347, #45369). With per-vsync frame
requests on Windows that meant a present every vsync for a second after the
pointer moved over hover-sensitive content. The presents are now limited to
macOS and iOS (which also turns them off for the web backend). High-rate input
still lifts the inactive-window frame cap everywhere. No effect on presentation
is expected with GPUI's default DirectComposition swap chain or on Linux;
untested are NVIDIA windowed G-SYNC with `GPUI_DISABLE_DIRECT_COMPOSITION`,
Windows 11 dynamic refresh rate, and fullscreen VRR on Linux. Not measured
separately.

### Ring as the only TLS provider (`gpui-http-client-tls-ring-only.patch`)

`http_client_tls` took rustls with its default features and installed the
AWS-LC provider when none was installed, which linked AWS-LC next to ring (used
by zed-reqwest and by Echo). The workspace `rustls` dependency is now
`default-features = false`, `http_client_tls` enables `ring`, `std`, `tls12`
and `logging`, and it installs ring under the same rule (only when no provider
is installed). What is lost is post-quantum key exchange (X25519MLKEM768,
AWS-LC only), and only where this function's install took effect; Echo
installs ring at start-up, before any client is built, so its handshakes are
unchanged. `aws-lc-rs`, `aws-lc-sys`, `cmake` and `fs_extra` left Echo's
lockfile.

### Upstream features Echo relies on

Echo uses upstream's `WindowOptions::inactive_frame_interval` for unfocused
windows: a 16 ms main-window interval for its ordinary 20 ms (50 fps) animation
cadence, a ~33.3 ms interval with `ECHO_INACTIVE_THROTTLE=1`, and `None` with
`ECHO_INACTIVE_THROTTLE=0`. Without a cap, unfocused windows render at display
rate, so the bounded default is intentional. Echo also uses the window visibility
observer (`observe_window_visibility`) and the `on_system_sleep` hook.

## Updating the pinned GPUI rev

1. Update the three `rev = ...` pins in the root `Cargo.toml`'s
   `[workspace.dependencies]` (`gpui`, `gpui_platform`, `reqwest_client`).
2. In the fork, fetch the new revision and rebase the series onto it:
   `git fetch origin <new-rev>` and
   `git rebase --onto <new-rev> <old-rev> echo-paint-fork`. Resolve conflicts
   (`rasterize_shaped_line_mask` follows `paint_glyph`'s internals; compare against
   the upstream `paint_glyph` when it drifts). Build and test the fork's affected
   crates, including `cargo test -p gpui --lib`: `animated_image_patch_tests`
   fail if animation patching silently stops resolving its slots. Debug builds
   compile the HLSL at runtime, so also run `cargo check --release -p gpui_windows`,
   whose build script compiles the shaders with fxc.
3. Regenerate each patch file from its rebased commit with
   `git format-patch -1 <commit> --stdout > <echo>/patches/<name>.patch`
   (`gpui-paint-fork.patch` holds two commits:
   `git format-patch <new-rev>..<second-commit> --stdout`). Update
   `scripts/gpui-fork-patches.txt` if the series layout changes.
4. Set `fork-tree` in `scripts/gpui-fork-patches.txt` to the rebased fork's
   `git rev-parse 'HEAD^{tree}'`, and update the `$PinnedRev` and
   `$ExpectedForkTree` literals in `scripts/setup-gpui-fork.ps1` (the release
   verification reads them; the script fails if they disagree with Cargo.toml
   or the manifest). Then confirm that both setup scripts reconstruct that tree
   from a clean checkout.
