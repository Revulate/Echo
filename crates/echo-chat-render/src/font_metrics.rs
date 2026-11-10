//! Font metrics provider using GPUI's text system for accurate measurement
//!
//! Replaces the hardcoded character width estimation with actual GPUI text measurement.
//! Provides pixel-perfect text positioning for crystal-clear rendering.

use dashmap::DashMap;
use gpui::{
    App, Font, FontFeatures, FontStyle, FontWeight, Pixels, SharedString, TextRun, Window, px,
};
use smallvec::SmallVec;
use std::cell::RefCell;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, OnceLock, RwLock, RwLockWriteGuard};

/// Capacity for the per-thread GPUI width-measurement cache.
const WIDTH_CACHE_CAPACITY: usize = 4096;

/// Key for the cached GPUI width measurement.
///
/// PERF: keyed on an `FxHash` of the text rather than an owned `String`, so a
/// cache hit needs no heap allocation (the previous `text.to_string()` ran on
/// every lookup, including hits, and `measure_string_gpui` is called many times
/// per layout). The owned `String` is never materialized for width memoization;
/// FxHash collisions are acceptable here (they would only return a slightly
/// wrong cached width for two distinct strings that hash identically at the same
/// weight + font size, which never measurably affects wrapping).
#[derive(Clone, Copy, Eq, Hash, PartialEq)]
struct WidthCacheKey {
    text_hash: u64,
    is_bold: bool,
    font_size_bits: u32,
}

impl WidthCacheKey {
    #[inline]
    fn new(text: &str, is_bold: bool, font_size: f32) -> Self {
        use std::hash::{Hash, Hasher};
        let mut hasher = rustc_hash::FxHasher::default();
        text.hash(&mut hasher);
        Self {
            text_hash: hasher.finish(),
            is_bold,
            font_size_bits: font_size.to_bits(),
        }
    }
}

/// Bounded FIFO cache mapping a shaped-text key to its measured width.
#[derive(Default)]
struct WidthCache {
    map: HashMap<WidthCacheKey, f32>,
    order: VecDeque<WidthCacheKey>,
}

impl WidthCache {
    #[inline]
    fn get(&self, key: &WidthCacheKey) -> Option<f32> {
        self.map.get(key).copied()
    }

    #[inline]
    fn insert(&mut self, key: WidthCacheKey, width: f32) {
        if let std::collections::hash_map::Entry::Occupied(mut e) = self.map.entry(key) {
            e.insert(width);
            return;
        }
        while self.map.len() >= WIDTH_CACHE_CAPACITY {
            let Some(oldest) = self.order.pop_front() else {
                self.map.clear();
                break;
            };
            self.map.remove(&oldest);
        }
        self.order.push_back(key);
        self.map.insert(key, width);
    }
}

thread_local! {
    /// Per-thread cache for `measure_string_gpui`. The method requires a
    /// `&mut Window` so it only ever runs on the main thread; this kills the
    /// per-wrap-check uncached `shape_line` + `SharedString` allocation.
    static WIDTH_CACHE: RefCell<WidthCache> = RefCell::new(WidthCache::default());
}

use crate::ports;

// ============================================================================
// Font Configuration
// ============================================================================

/// Default font family for chat messages; a host may install another through
/// [`crate::text_run_intern::install_font_families`].
pub const DEFAULT_FONT_FAMILY: &str = ".SystemUIFont";

/// 10 points at 96 DPI. GPUI sizes are logical pixels, so the point
/// conversion is 10 * 4/3 = 13⅓ px at `font_scale == 1.0`.
pub const DEFAULT_FONT_SIZE: f32 = 40.0 / 3.0;

/// Bold weight multiplier (approximate, used when not measured)
pub const BOLD_MULTIPLIER: f32 = 1.08;

/// Get current font size scaled by the font_scale setting
/// PERF: Called during layout and painting. font_scale read is a fast mutex lock.
#[inline]
pub fn scaled_font_size() -> f32 {
    DEFAULT_FONT_SIZE * ports::font_scale()
}

// ============================================================================
// Cached Font Metrics
// ============================================================================

/// Cached metrics for a specific font configuration
#[derive(Clone, Debug)]
pub struct CachedFontMetrics {
    /// Per-character advance widths measured from GPUI
    pub char_advances: DashMap<char, f32>,
    /// Space width (common lookup)
    pub space_width: f32,
    /// Average character width for unknown characters
    pub average_width: f32,
    /// Whether this has been initialized with GPUI measurements
    pub is_initialized: bool,
}

impl Default for CachedFontMetrics {
    fn default() -> Self {
        Self {
            char_advances: DashMap::with_capacity(256),
            space_width: 4.0,
            average_width: 7.0,
            is_initialized: false,
        }
    }
}

// ============================================================================
// Font Metrics Provider
// ============================================================================

/// Provides accurate font metrics by querying GPUI's text system.
///
/// This replaces the hardcoded FontMetrics with actual measured values.
/// Falls back to estimation when GPUI context is not available.
#[derive(Clone)]
pub struct FontMetricsProvider {
    /// Font for normal text
    pub font: Font,
    /// Font size in pixels
    pub font_size: Pixels,
    /// Cached metrics for normal weight
    normal_metrics: CachedFontMetrics,
    /// Cached metrics for bold weight
    bold_metrics: CachedFontMetrics,
}

impl FontMetricsProvider {
    /// Create a new font metrics provider with default settings
    pub fn new() -> Self {
        Self {
            font: Font {
                family: crate::text_run_intern::system_font_family(),
                weight: FontWeight::NORMAL,
                style: FontStyle::Normal,
                features: FontFeatures::default(),
                fallbacks: None,
            },
            font_size: px(DEFAULT_FONT_SIZE),
            normal_metrics: Self::create_estimated_metrics(),
            bold_metrics: Self::create_estimated_metrics_bold(),
        }
    }

    /// Create estimated metrics (fallback when GPUI not available)
    fn create_estimated_metrics() -> CachedFontMetrics {
        let metrics = CachedFontMetrics::default();

        // Pre-populate with estimated character widths for common ASCII
        // These are calibrated for 14px system font and slightly overestimate

        // Very narrow characters (3-4px)
        for c in ['i', 'l', '|', '!', '.', ',', ';', '\'', '`', 'I', 'j', '1'] {
            metrics.char_advances.insert(c, 4.0);
        }

        // Narrow characters (5px)
        for c in [':', 'f', 't', 'r', '(', ')', '[', ']', '{', '}'] {
            metrics.char_advances.insert(c, 5.0);
        }

        // Medium-narrow characters (6-7px)
        for c in [
            'a', 'c', 'e', 'g', 'n', 'o', 'p', 'q', 's', 'u', 'v', 'x', 'y', 'z', '2', '3', '4',
            '5', '6', '7', '8', '9', '0', '-', '_', '"', '/',
        ] {
            metrics.char_advances.insert(c, 7.0);
        }

        // Medium characters (8px)
        for c in [
            'b', 'd', 'h', 'k', 'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'J', 'K', 'L', 'N', 'O',
            'P', 'Q', 'R', 'S', 'T', 'U', 'V', 'X', 'Y', 'Z', '?', '+', '=', '<', '>',
        ] {
            metrics.char_advances.insert(c, 8.0);
        }

        // Wide characters (10px)
        for c in ['m', 'w', 'M', 'W', '@', '#', '$', '%', '&', '*', '~'] {
            metrics.char_advances.insert(c, 10.0);
        }

        // Space
        metrics.char_advances.insert(' ', 4.0);

        // Tab
        metrics.char_advances.insert('\t', 32.0);

        CachedFontMetrics {
            char_advances: metrics.char_advances,
            space_width: 4.0,
            average_width: 7.0,
            is_initialized: false,
        }
    }

    /// Create estimated bold metrics
    fn create_estimated_metrics_bold() -> CachedFontMetrics {
        let normal = Self::create_estimated_metrics();
        let bold = CachedFontMetrics::default();

        // Copy with bold multiplier
        for entry in normal.char_advances.iter() {
            bold.char_advances
                .insert(*entry.key(), *entry.value() * BOLD_MULTIPLIER);
        }

        CachedFontMetrics {
            char_advances: bold.char_advances,
            space_width: normal.space_width * BOLD_MULTIPLIER,
            average_width: normal.average_width * BOLD_MULTIPLIER,
            is_initialized: false,
        }
    }

    /// Initialize metrics using GPUI's text system (call once with context)
    pub fn initialize_with_gpui(&mut self, window: &mut Window, _cx: &mut App) {
        if self.normal_metrics.is_initialized {
            return;
        }

        // Measure common ASCII characters
        let test_chars: Vec<char> = (32u8..127u8).map(|b| b as char).collect();

        for &ch in &test_chars {
            if let Some(width) = self.measure_char_with_gpui(ch, false, window) {
                self.normal_metrics.char_advances.insert(ch, width);
            }
            if let Some(width) = self.measure_char_with_gpui(ch, true, window) {
                self.bold_metrics.char_advances.insert(ch, width);
            }
        }

        // Update space width and average
        if let Some(space) = self.normal_metrics.char_advances.get(&' ') {
            self.normal_metrics.space_width = *space;
        }
        if let Some(space) = self.bold_metrics.char_advances.get(&' ') {
            self.bold_metrics.space_width = *space;
        }

        // Calculate actual average from measured characters
        let total: f32 = self
            .normal_metrics
            .char_advances
            .iter()
            .map(|e| *e.value())
            .sum();
        let count = self.normal_metrics.char_advances.len();
        if count > 0 {
            self.normal_metrics.average_width = total / count as f32;
        }

        let total_bold: f32 = self
            .bold_metrics
            .char_advances
            .iter()
            .map(|e| *e.value())
            .sum();
        let count_bold = self.bold_metrics.char_advances.len();
        if count_bold > 0 {
            self.bold_metrics.average_width = total_bold / count_bold as f32;
        }

        self.normal_metrics.is_initialized = true;
        self.bold_metrics.is_initialized = true;

        echo_core::log_ui!(
            debug,
            "FontMetricsProvider initialized with GPUI: space={}px, avg={}px",
            self.normal_metrics.space_width,
            self.normal_metrics.average_width
        );
    }

    /// Measure a single character using GPUI text shaping
    fn measure_char_with_gpui(&self, ch: char, is_bold: bool, window: &mut Window) -> Option<f32> {
        let mut encoded = [0_u8; 4];
        let text = ch.encode_utf8(&mut encoded);
        let font = if is_bold {
            Font {
                weight: FontWeight::SEMIBOLD,
                ..self.font.clone()
            }
        } else {
            self.font.clone()
        };

        // Use GPUI's text shaping
        let runs = vec![TextRun {
            len: text.len(),
            font,
            color: ports::text_primary(),
            background_color: None,
            underline: None,
            strikethrough: None,
        }];

        // Shape the line to get accurate width (no wrapping)
        let shaped_line = window.text_system().shape_line(
            SharedString::from(text),
            self.font_size,
            &runs,
            None, // No wrap width - measure full width
        );
        Some(shaped_line.width.as_f32())
    }

    /// Whether the advances were measured with GPUI rather than estimated.
    #[inline]
    pub fn is_calibrated(&self) -> bool {
        self.normal_metrics.is_initialized
    }

    /// Get character width (uses cached value or estimates)
    #[inline]
    pub fn char_width(&self, ch: char, is_bold: bool) -> f32 {
        let metrics = if is_bold {
            &self.bold_metrics
        } else {
            &self.normal_metrics
        };

        if let Some(width) = metrics.char_advances.get(&ch) {
            return *width;
        }

        // For emoji and other wide characters, estimate based on character width hint
        if ch.is_ascii() {
            metrics.average_width
        } else {
            // Non-ASCII typically wider (emoji, CJK, etc.)
            metrics.average_width * 2.0
        }
    }

    /// Measure string width using cached character widths
    pub fn measure_string(&self, text: &str, is_bold: bool) -> f32 {
        let scale = ports::font_scale();
        text.chars()
            .map(|c| self.char_width(c, is_bold))
            .sum::<f32>()
            * scale
    }

    /// Measure string width with GPUI for accuracy (when context available)
    ///
    /// PERF: Routes through a per-thread width cache keyed on
    /// `(text, bold, font_size)`. Wrap-checks re-measure the same tokens (esp.
    /// the literal space width) repeatedly; without the cache each call shaped a
    /// fresh `SharedString` uncached. The `shape_line` only runs on a miss.
    pub fn measure_string_gpui(&self, text: &str, is_bold: bool, window: &mut Window) -> f32 {
        if text.is_empty() {
            return 0.0;
        }

        let font_size = scaled_font_size();
        let key = WidthCacheKey::new(text, is_bold, font_size);

        if let Some(width) = WIDTH_CACHE.with(|cache| cache.borrow().get(&key)) {
            return width;
        }

        let font = if is_bold {
            Font {
                weight: FontWeight::SEMIBOLD,
                ..self.font.clone()
            }
        } else {
            self.font.clone()
        };

        let run = TextRun {
            len: text.len(),
            font,
            color: ports::text_primary(),
            background_color: None,
            underline: None,
            strikethrough: None,
        };

        let shaped_line = window.text_system().shape_line(
            SharedString::from(text),
            px(font_size),
            std::slice::from_ref(&run),
            None, // No wrap width - measure full width
        );
        let width = shaped_line.width.as_f32();
        WIDTH_CACHE.with(|cache| cache.borrow_mut().insert(key, width));
        width
    }

    /// Batch build character positions into existing vectors (zero-allocation fast path)
    /// Returns the final X position after all characters
    #[inline]
    pub fn batch_char_positions_into(
        &self,
        text: &str,
        start_x: f32,
        is_bold: bool,
        source_positions: &mut SmallVec<[f32; 48]>,
        pending_positions: &mut SmallVec<[f32; 24]>,
        source_text: &mut String,
    ) -> f32 {
        let scale = ports::font_scale();
        let mut x = start_x;

        for ch in text.chars() {
            source_positions.push(x);
            pending_positions.push(x);
            source_text.push(ch);
            x += self.char_width(ch, is_bold) * scale;
        }

        x
    }
}

impl Default for FontMetricsProvider {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Global Instance
// ============================================================================

/// Global font metrics provider instance
///
/// Uses estimated metrics initially, then can be initialized with GPUI
/// measurements when Window/App context becomes available.
pub static FONT_METRICS_PROVIDER: LazyLock<RwLock<FontMetricsProvider>> =
    LazyLock::new(|| RwLock::new(FontMetricsProvider::new()));

fn write_font_metrics_provider(
    provider: &RwLock<FontMetricsProvider>,
) -> RwLockWriteGuard<'_, FontMetricsProvider> {
    match provider.write() {
        Ok(provider) => provider,
        Err(poisoned) => {
            let mut provider_guard = poisoned.into_inner();
            *provider_guard = FontMetricsProvider::new();
            provider.clear_poison();
            echo_core::log_ui!(warn, "Reset poisoned font metrics provider");
            provider_guard
        }
    }
}

/// Immutable copy of the provider published once GPUI calibration finished,
/// so measurements never wait on or race the provider lock.
static CALIBRATED_FONT_METRICS: OnceLock<FontMetricsProvider> = OnceLock::new();

/// The GPUI-calibrated metrics, once `initialize_font_metrics` has run.
#[inline]
pub fn calibrated_font_metrics() -> Option<&'static FontMetricsProvider> {
    CALIBRATED_FONT_METRICS.get()
}

/// Initialize the global font metrics with GPUI (call once during app startup)
pub fn initialize_font_metrics(window: &mut Window, cx: &mut App) {
    if CALIBRATED_FONT_METRICS.get().is_some() {
        return;
    }
    let mut provider = write_font_metrics_provider(&FONT_METRICS_PROVIDER);
    provider.initialize_with_gpui(window, cx);
    if provider.is_calibrated() {
        let _ = CALIBRATED_FONT_METRICS.set(provider.clone());
    }
}

// ============================================================================
// Compatibility Layer
// ============================================================================

/// Compatibility wrapper that matches the old FontMetrics API
/// This allows gradual migration from the old system
pub struct FontMetrics {
    provider: Arc<FontMetricsProvider>,
}

impl FontMetrics {
    pub fn new() -> Self {
        Self {
            provider: Arc::new(FontMetricsProvider::new()),
        }
    }

    /// Windowless measurements use the GPUI-calibrated advances once
    /// `initialize_font_metrics` ran, and the estimated table before that.
    #[inline]
    fn with_char_metrics<R>(&self, measure: impl FnOnce(&FontMetricsProvider) -> R) -> R {
        measure(calibrated_font_metrics().unwrap_or(&self.provider))
    }

    pub fn measure_string_bold(&self, text: &str, is_bold: bool) -> f32 {
        self.with_char_metrics(|provider| provider.measure_string(text, is_bold))
    }

    /// Measure text width accurately using GPUI text shaping
    ///
    /// This method uses GPUI's text system to shape the text with the actual font,
    /// providing pixel-perfect width measurement that accounts for:
    /// - Kerning pairs
    /// - Ligatures
    /// - Sub-pixel positioning
    /// - Font-specific metrics
    ///
    /// Use this when you need accurate measurements on the main thread with a window available.
    /// Falls back to cached metrics when window is None (background thread or async layout).
    pub fn measure_string_gpui(&self, text: &str, is_bold: bool, window: &mut Window) -> f32 {
        self.provider.measure_string_gpui(text, is_bold, window)
    }

    #[inline]
    pub fn batch_char_positions_into(
        &self,
        text: &str,
        start_x: f32,
        is_bold: bool,
        source_positions: &mut SmallVec<[f32; 48]>,
        pending_positions: &mut SmallVec<[f32; 24]>,
        source_text: &mut String,
    ) -> f32 {
        self.with_char_metrics(|provider| {
            provider.batch_char_positions_into(
                text,
                start_x,
                is_bold,
                source_positions,
                pending_positions,
                source_text,
            )
        })
    }
}

impl Default for FontMetrics {
    fn default() -> Self {
        Self::new()
    }
}

/// Global FontMetrics instance (compatibility with existing code)
pub static FONT_METRICS: LazyLock<FontMetrics> = LazyLock::new(FontMetrics::new);

#[cfg(test)]
mod tests {
    use super::*;
    use std::panic::{AssertUnwindSafe, catch_unwind};

    #[test]
    fn poisoned_provider_resets_to_estimated_metrics_and_allows_reuse() {
        let provider = RwLock::new(FontMetricsProvider::new());
        let _ = catch_unwind(AssertUnwindSafe(|| {
            let mut provider = provider
                .write()
                .expect("test provider should start healthy");
            provider.normal_metrics.is_initialized = true;
            provider.bold_metrics.is_initialized = true;
            provider.normal_metrics.average_width = 123.0;
            panic!("poison test provider");
        }));

        assert!(provider.is_poisoned());
        let provider_guard = write_font_metrics_provider(&provider);
        assert!(!provider_guard.normal_metrics.is_initialized);
        assert!(!provider_guard.bold_metrics.is_initialized);
        assert_eq!(provider_guard.normal_metrics.average_width, 7.0);
        drop(provider_guard);
        assert!(!provider.is_poisoned());
        assert!(provider.write().is_ok());
    }

    #[test]
    fn test_estimated_char_widths() {
        let provider = FontMetricsProvider::new();

        // Narrow characters should be narrower
        assert!(provider.char_width('i', false) < provider.char_width('m', false));
        assert!(provider.char_width('l', false) < provider.char_width('w', false));

        // Bold should be wider
        assert!(provider.char_width('a', true) > provider.char_width('a', false));
    }

    #[test]
    fn test_measure_string() {
        let provider = FontMetricsProvider::new();

        let width = provider.measure_string("Hello", false);
        assert!(width > 0.0);
        assert!(width < 100.0); // Reasonable range

        let bold_width = provider.measure_string("Hello", true);
        assert!(bold_width > width);
    }
}
