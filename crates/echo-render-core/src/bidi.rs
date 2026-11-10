//! BiDi (Bidirectional Text) support using Unicode BiDi algorithm
//!
//! Provides RTL text detection and visual reordering for Hebrew, Arabic,
//! and mixed-direction text. Uses the unicode-bidi crate for UAX #9 compliance.

use smallvec::SmallVec;
use unicode_bidi::{BidiClass, BidiInfo, bidi_class};

// ============================================================================
// RTL Detection
// ============================================================================

/// Quick check if text contains any RTL characters
///
/// Uses early exit on first RTL character for fast common case (LTR only)
/// Checks for:
/// - Arabic (AL class)
/// - Hebrew and other strong RTL (R class)
/// - RTL override/embedding markers
#[inline]
pub fn contains_rtl(text: &str) -> bool {
    text.chars().any(|c| {
        matches!(
            bidi_class(c),
            BidiClass::R | BidiClass::AL | BidiClass::RLE | BidiClass::RLO | BidiClass::RLI
        )
    })
}

// ============================================================================
// Visual Reordering
// ============================================================================

/// Result of BiDi analysis for a text segment
#[derive(Clone, Debug)]
pub struct BidiAnalysis {
    /// Visual-order indices mapping to logical positions
    /// visual_to_logical[visual_pos] = logical_char_idx
    pub visual_to_logical: SmallVec<[usize; 64]>,

    /// Whether any reordering was actually needed
    pub needs_reordering: bool,
}

impl Default for BidiAnalysis {
    fn default() -> Self {
        Self {
            visual_to_logical: SmallVec::new(),
            needs_reordering: false,
        }
    }
}

impl BidiAnalysis {
    /// Analyze text and compute visual reordering information
    ///
    /// Fast path for pure LTR text (most common case)
    pub fn new(text: &str) -> Self {
        // Fast path: if no RTL characters, skip full analysis.
        // Return an EMPTY mapping to represent identity instead of
        // materializing (0..n). For messages >64 chars this avoids a heap spill
        // out of the inline SmallVec. Consumers treat a length mismatch (here,
        // empty) as identity (see `ShapedLineData::logical_index_from_visual_boundary`).
        if !contains_rtl(text) {
            return Self {
                visual_to_logical: SmallVec::new(),
                needs_reordering: false,
            };
        }

        // Full BiDi analysis needed
        let bidi_info = BidiInfo::new(text, None); // Auto-detect base direction

        // Get the first paragraph (we process single-paragraph text)
        if bidi_info.paragraphs.is_empty() {
            return Self::default();
        }

        let para = &bidi_info.paragraphs[0];
        let para_text = &text[para.range.clone()];

        // Get the visual reordering - returns (Vec<Level>, Vec<LevelRun>)
        let line = para.range.clone();
        let (levels, visual_runs) = bidi_info.visual_runs(para, line);

        // Build character-level mappings
        let char_count = para_text.chars().count();
        let mut visual_to_logical: SmallVec<[usize; 64]> = SmallVec::with_capacity(char_count);

        // Note: byte_to_char mapping would be used for more complex selection handling
        // Currently we build char indices directly in the run processing loop

        // Flatten visual runs into character-level ordering
        for run_range in &visual_runs {
            // Get the level for this run (from the first byte in the run)
            let run_level = levels
                .get(run_range.start.saturating_sub(para.range.start))
                .copied();
            let is_rtl_run = run_level.map(|l| l.is_rtl()).unwrap_or(false);

            // Get character indices for this run
            let run_char_indices: Vec<usize> = para_text
                .char_indices()
                .enumerate()
                .filter(|(_, (byte_idx, _))| {
                    let abs_byte = para.range.start + byte_idx;
                    run_range.contains(&abs_byte)
                })
                .map(|(char_idx, _)| char_idx)
                .collect();

            // If this run is RTL (odd level), characters within it are reversed
            if is_rtl_run {
                for logical_idx in run_char_indices.into_iter().rev() {
                    if logical_idx < char_count {
                        visual_to_logical.push(logical_idx);
                    }
                }
            } else {
                for logical_idx in run_char_indices {
                    if logical_idx < char_count {
                        visual_to_logical.push(logical_idx);
                    }
                }
            }
        }

        // If visual_to_logical doesn't match char_count, fall back to identity
        if visual_to_logical.len() != char_count {
            return Self {
                visual_to_logical: (0..char_count).collect(),
                needs_reordering: false,
            };
        }

        // Check if any reordering is actually needed
        let needs_reordering = visual_to_logical
            .iter()
            .enumerate()
            .any(|(vis, &log)| vis != log);

        Self {
            visual_to_logical,
            needs_reordering,
        }
    }

    /// Build character boundary X positions in visual order for hit testing.
    ///
    /// `logical_char_positions` must be character-start boundaries plus trailing end
    /// boundary in logical source order (len = logical_char_count + 1).
    pub fn build_visual_char_positions(
        &self,
        logical_char_positions: &[f32],
        logical_char_count: usize,
    ) -> SmallVec<[f32; 64]> {
        let expected_len = logical_char_count.saturating_add(1);
        let fallback = || {
            logical_char_positions
                .iter()
                .take(expected_len)
                .copied()
                .collect::<SmallVec<[f32; 64]>>()
        };

        if logical_char_positions.len() < expected_len {
            return fallback();
        }
        if logical_char_count == 0 {
            return fallback();
        }
        if self.visual_to_logical.len() != logical_char_count {
            return fallback();
        }

        let mut visual_positions = SmallVec::with_capacity(expected_len);
        let mut cursor_x = logical_char_positions[0];
        visual_positions.push(cursor_x);

        for &logical_idx in self.visual_to_logical.iter().take(logical_char_count) {
            if logical_idx >= logical_char_count {
                return fallback();
            }
            let start_x = logical_char_positions[logical_idx];
            let end_x = logical_char_positions[logical_idx + 1];
            cursor_x += (end_x - start_x).max(0.0);
            visual_positions.push(cursor_x);
        }

        if visual_positions.len() == expected_len {
            visual_positions
        } else {
            fallback()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_contains_rtl() {
        // Pure LTR
        assert!(!contains_rtl("Hello World"));
        assert!(!contains_rtl("12345"));
        assert!(!contains_rtl(""));

        // Hebrew
        assert!(contains_rtl("שלום"));
        assert!(contains_rtl("Hello שלום"));

        // Arabic
        assert!(contains_rtl("مرحبا"));
        assert!(contains_rtl("Hello مرحبا"));
    }

    #[test]
    fn test_pure_ltr_fast_path() {
        let analysis = BidiAnalysis::new("Hello World");
        assert!(!analysis.needs_reordering);
    }

    #[test]
    fn test_pure_rtl() {
        let analysis = BidiAnalysis::new("שלום");
        // Hebrew text should be reordered for visual display
        assert!(analysis.needs_reordering);
    }

    #[test]
    fn build_visual_char_positions_uses_identity_for_ltr() {
        let analysis = BidiAnalysis::new("Hello");
        let logical = [10.0, 12.0, 14.0, 16.0, 18.0, 20.0];
        let visual = analysis.build_visual_char_positions(&logical, 5);
        assert_eq!(visual.as_slice(), logical);
    }

    #[test]
    fn build_visual_char_positions_reorders_advances_for_rtl() {
        let analysis = BidiAnalysis {
            visual_to_logical: SmallVec::from_slice(&[3, 2, 1, 0]),
            needs_reordering: true,
        };
        let logical = [10.0, 12.0, 13.0, 15.0, 16.0];
        let visual = analysis.build_visual_char_positions(&logical, 4);

        assert_eq!(visual.as_slice(), &[10.0, 11.0, 13.0, 14.0, 16.0]);
    }
}
