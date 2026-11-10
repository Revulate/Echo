//! Line buffering for dynamic line heights
//!
//! Accumulates layout items for a single line before committing them with
//! calculated Y positions. This enables dynamic line heights based on the
//! tallest element in each line.
#![allow(clippy::large_enum_variant)] // Size difference is acceptable for buffer items
#![allow(clippy::too_many_arguments)] // Buffer functions need all context at once

use gpui::{Hsla, SharedString};
use smallvec::SmallVec;

#[cfg(test)]
use super::layout::TEXT_HEIGHT_BASE;
use super::layout::{ImageType, LayeredEmote, LayoutSegment, OverlayEmote, text_height};

// ============================================================================
// Line Item (Buffered Element)
// ============================================================================

/// An item buffered for the current line, waiting for final Y positioning.
///
/// Items are accumulated during token processing, then converted to
/// `LayoutSegment`s when the line is finalized (on wrap or message end).
#[derive(Clone, Debug)]
pub enum LineItem {
    /// Text segment (references pre-shaped line data)
    Text {
        /// Index into shaped_lines Vec
        line_idx: usize,
        /// X position (already calculated)
        x: f32,
        /// Width of this text segment
        width: f32,
        /// Height of text (from text_height(), scales with font_scale)
        height: f32,
    },

    /// Image segment (emote or badge)
    Image {
        /// Image URL
        url: SharedString,
        /// X position
        x: f32,
        /// Display width
        width: f32,
        /// Display height
        height: f32,
        /// Whether animated
        is_animated: bool,
        /// Image type for tooltip
        image_type: ImageType,
        /// Zero-width overlay emotes
        overlays: Vec<OverlayEmote>,
        /// Optional source-text range occupied by this image.
        source_range: Option<(usize, usize)>,
    },

    /// Layered emote with centered overlays
    ///
    /// This variant buffers a `LayeredEmote` which will compute centered
    /// positions for all layers when finalized.
    LayeredImage {
        /// The layered emote (base + overlays with lazy centering)
        emote: LayeredEmote,
        /// X position (left edge of bounding box)
        x: f32,
        /// Start char index of the base emote's code in `source_text` (see
        /// `LayoutSegment::LayeredImage::source_start_char`).
        source_start_char: usize,
        /// Char length of the base emote's code in `source_text` (see
        /// `LayoutSegment::LayeredImage::source_len_chars`).
        source_len_chars: usize,
    },

    /// Inline box (NEW badge, returning chatter badge)
    InlineBox {
        /// Text content
        text: SharedString,
        /// X position
        x: f32,
        /// Box width
        width: f32,
        /// Box height
        height: f32,
        /// Background color
        bg_color: Hsla,
        /// Text color
        text_color: Hsla,
        /// Font size
        font_size: f32,
    },
}

impl LineItem {
    /// Convert to a positioned LayoutSegment given the line's base Y and max height.
    ///
    /// Uses baseline/bottom alignment: items are positioned so their bottom
    /// edge aligns with the line's bottom edge.
    pub fn into_segment(self, line_y: f32, line_max_height: f32) -> LayoutSegment {
        match self {
            LineItem::Text {
                line_idx,
                x,
                width,
                height,
            } => {
                // Bottom-align text within line height
                let y = line_y + (line_max_height - height).max(0.0);
                LayoutSegment::Text {
                    line_idx,
                    x,
                    y,
                    width,
                    height: line_max_height, // Segment height is the full line height
                }
            }
            LineItem::Image {
                url,
                x,
                width,
                height,
                is_animated,
                image_type,
                overlays,
                source_range,
            } => {
                // Bottom-align image within line height
                let y = line_y + (line_max_height - height).max(0.0);
                LayoutSegment::Image {
                    url,
                    x,
                    y,
                    width,
                    height,
                    is_animated,
                    image_type,
                    overlays,
                    source_range,
                }
            }
            LineItem::LayeredImage {
                mut emote,
                x,
                source_start_char,
                source_len_chars,
            } => {
                // Get bounding box dimensions and compute centered layout
                let (bbox_width, bbox_height) = emote.bounds();

                // Bottom-align bounding box within line height (like regular images)
                let bbox_y = line_y + (line_max_height - bbox_height).max(0.0);

                // Convert to positioned layers with absolute coordinates
                let layers = emote.into_positioned_layers(x, bbox_y);

                LayoutSegment::LayeredImage {
                    layers,
                    x,
                    y: bbox_y,
                    width: bbox_width,
                    height: bbox_height,
                    source_start_char,
                    source_len_chars,
                }
            }
            LineItem::InlineBox {
                text,
                x,
                width,
                height,
                bg_color,
                text_color,
                font_size,
            } => {
                // Bottom-align inline boxes with the text band like badges/text. Centering
                // floated the pill on emote-tall lines; naive band-centering clips because
                // the box is taller than the text band.
                let y = line_y + (line_max_height - height).max(0.0);
                LayoutSegment::InlineBox {
                    text,
                    x,
                    y,
                    width,
                    height,
                    bg_color,
                    text_color,
                    font_size,
                }
            }
        }
    }
}

// ============================================================================
// Line Buffer
// ============================================================================

/// Buffer for accumulating items on the current line before committing.
///
/// This abstraction separates "what goes on a line" from "where the line is
/// positioned", enabling dynamic line heights based on content.
///
/// # Lifecycle
///
/// 1. Create buffer with `new()`
/// 2. Add items with `add_text()`, `add_image()`, `add_inline_box()`
/// 3. Finalize with `finalize()` to get positioned segments
///
/// # Performance
///
/// Uses `SmallVec` to keep most lines stack-allocated (typical messages
/// have < 8 items per line).
pub struct LineBuffer {
    /// Buffered items waiting to be committed
    items: SmallVec<[LineItem; 8]>,
    /// Maximum height of all items in buffer
    max_height: f32,
    /// Minimum line height (from settings, for empty lines)
    min_height: f32,
}

impl LineBuffer {
    /// Create a new empty line buffer with minimum height
    pub fn new(min_height: f32) -> Self {
        Self {
            items: SmallVec::new(),
            max_height: 0.0,
            min_height,
        }
    }

    /// Get the current maximum height of items in this buffer
    #[inline]
    pub fn max_height(&self) -> f32 {
        self.max_height.max(self.min_height)
    }

    /// Check if the buffer is empty
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Get the number of items in the buffer
    #[inline]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Add a text segment to the buffer
    pub fn add_text(&mut self, line_idx: usize, x: f32, width: f32) {
        let h = text_height();
        self.max_height = self.max_height.max(h);
        self.items.push(LineItem::Text {
            line_idx,
            x,
            width,
            height: h,
        });
    }

    /// Add an image segment to the buffer
    pub fn add_image(
        &mut self,
        url: SharedString,
        x: f32,
        width: f32,
        height: f32,
        is_animated: bool,
        image_type: ImageType,
        source_range: Option<(usize, usize)>,
    ) {
        self.max_height = self.max_height.max(height);
        self.items.push(LineItem::Image {
            url,
            x,
            width,
            height,
            is_animated,
            image_type,
            overlays: Vec::new(),
            source_range,
        });
    }

    /// Add an inline box segment to the buffer
    pub fn add_inline_box(
        &mut self,
        text: SharedString,
        x: f32,
        width: f32,
        height: f32,
        bg_color: Hsla,
        text_color: Hsla,
        font_size: f32,
    ) {
        self.max_height = self.max_height.max(height);
        self.items.push(LineItem::InlineBox {
            text,
            x,
            width,
            height,
            bg_color,
            text_color,
            font_size,
        });
    }

    /// Add a layered emote (base + overlays, centered)
    ///
    /// `source_start_char`/`source_len_chars` locate the base emote's text code
    /// within the layout's flat `source_text` buffer (see
    /// `LayoutSegment::LayeredImage`), enabling drag-selection and Ctrl+C copy.
    pub fn add_layered_emote(
        &mut self,
        mut emote: LayeredEmote,
        x: f32,
        _image_type: ImageType,
        source_start_char: usize,
        source_len_chars: usize,
    ) {
        // Get bounding box height to update max_height
        let (_, bbox_height) = emote.bounds();

        self.max_height = self.max_height.max(bbox_height);
        self.items.push(LineItem::LayeredImage {
            emote,
            x,
            source_start_char,
            source_len_chars,
        });
    }

    /// Get the index of the last image item in the buffer (for overlay tracking)
    pub fn last_image_index(&self) -> Option<usize> {
        self.items
            .iter()
            .enumerate()
            .rev()
            .find(|(_, item)| matches!(item, LineItem::Image { .. }))
            .map(|(idx, _)| idx)
    }

    /// Finalize the buffer: convert all items to positioned segments.
    ///
    /// Returns the positioned segments and the actual line height used.
    /// The buffer is consumed by this operation.
    ///
    /// # Arguments
    ///
    /// * `line_y` - The Y position where this line starts
    ///
    /// # Returns
    ///
    /// * `Vec<LayoutSegment>` - Positioned segments ready for rendering
    /// * `f32` - The actual height of this line (max of all items)
    pub fn finalize(self, line_y: f32) -> (SmallVec<[LayoutSegment; 8]>, f32) {
        let line_height = self.max_height();
        let segments: SmallVec<[LayoutSegment; 8]> = self
            .items
            .into_iter()
            .map(|item| item.into_segment(line_y, line_height))
            .collect();
        (segments, line_height)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_empty_buffer_uses_min_height() {
        let buffer = LineBuffer::new(26.0);
        assert_eq!(buffer.max_height(), 26.0);
    }

    #[test]
    fn test_text_height_tracking() {
        let mut buffer = LineBuffer::new(26.0);
        buffer.add_text(0, 10.0, 50.0);
        assert_eq!(buffer.max_height(), TEXT_HEIGHT_BASE.max(26.0));
    }

    #[test]
    fn test_tall_image_increases_height() {
        let mut buffer = LineBuffer::new(26.0);
        buffer.add_text(0, 10.0, 50.0);
        buffer.add_image(
            "http://example.com/emote.png".into(),
            60.0,
            48.0,
            48.0, // Tall emote
            false,
            ImageType::Emote {
                name: "test".into(),
                tooltip_meta: None,
            },
            None,
        );
        assert_eq!(buffer.max_height(), 48.0);
    }

    #[test]
    fn test_finalize_bottom_aligns_text() {
        let mut buffer = LineBuffer::new(20.0);
        buffer.add_text(0, 10.0, 50.0);
        buffer.add_image(
            "http://example.com/emote.png".into(),
            60.0,
            40.0,
            40.0,
            false,
            ImageType::Emote {
                name: "test".into(),
                tooltip_meta: None,
            },
            None,
        );

        let (segments, height) = buffer.finalize(100.0);
        assert_eq!(height, 40.0); // Max of text (18) and image (40)
        assert_eq!(segments.len(), 2);

        // Text should be bottom-aligned: y = 100 + (40 - 18) = 122
        if let LayoutSegment::Text { y, .. } = &segments[0] {
            assert_eq!(*y, 100.0 + (40.0 - TEXT_HEIGHT_BASE));
        } else {
            panic!("Expected Text segment");
        }

        // Image should be at line start: y = 100 + (40 - 40) = 100
        if let LayoutSegment::Image { y, .. } = &segments[1] {
            assert_eq!(*y, 100.0);
        } else {
            panic!("Expected Image segment");
        }
    }
}
