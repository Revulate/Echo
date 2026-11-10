//! Per-message paint layer ordering.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessagePaintLayer {
    Border,
    MentionHighlights,
    SelectionHighlights,
    Text,
    Images,
    /// Selection tint for emote/image segments. Unlike `SelectionHighlights`
    /// (painted *before* `Text`, relying on text drawing over an opaque band),
    /// emote images are themselves opaque, so this must paint *after* `Images`
    /// or the image would draw over the tint and hide it.
    ImageSelectionHighlights,
}

#[inline]
pub const fn message_paint_layers() -> [MessagePaintLayer; 6] {
    [
        MessagePaintLayer::Border,
        MessagePaintLayer::MentionHighlights,
        MessagePaintLayer::SelectionHighlights,
        MessagePaintLayer::Text,
        MessagePaintLayer::Images,
        MessagePaintLayer::ImageSelectionHighlights,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_paint_layers_keep_selection_behind_text_and_images() {
        let layers = message_paint_layers();
        assert_eq!(
            layers,
            [
                MessagePaintLayer::Border,
                MessagePaintLayer::MentionHighlights,
                MessagePaintLayer::SelectionHighlights,
                MessagePaintLayer::Text,
                MessagePaintLayer::Images,
                MessagePaintLayer::ImageSelectionHighlights,
            ]
        );

        let selection_idx = layers
            .iter()
            .position(|layer| *layer == MessagePaintLayer::SelectionHighlights)
            .expect("selection layer should exist");
        let text_idx = layers
            .iter()
            .position(|layer| *layer == MessagePaintLayer::Text)
            .expect("text layer should exist");
        let images_idx = layers
            .iter()
            .position(|layer| *layer == MessagePaintLayer::Images)
            .expect("images layer should exist");
        let image_selection_idx = layers
            .iter()
            .position(|layer| *layer == MessagePaintLayer::ImageSelectionHighlights)
            .expect("image selection layer should exist");

        assert!(selection_idx < text_idx);
        assert!(selection_idx < images_idx);
        // Emote selection tint must paint after the opaque emote images, or the
        // image would draw over (and hide) the tint.
        assert!(images_idx < image_selection_idx);
    }
}
