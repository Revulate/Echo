pub mod eviction_cache {
    pub use echo_render_core::eviction_cache::*;
}

pub mod layout {
    use std::hash::{Hash, Hasher};

    #[derive(Clone, Debug)]
    pub enum LayoutSegment {
        Text,
        Image {
            source_range: Option<(usize, usize)>,
        },
        LayeredImage {
            source_start_char: usize,
            source_len_chars: usize,
        },
    }

    #[derive(Clone, Debug)]
    pub struct CanvasMessageLayout {
        pub source_text: String,
        pub source_char_positions: Vec<f32>,
        pub segments: Vec<LayoutSegment>,
    }

    impl CanvasMessageLayout {
        pub fn new(_message_id: String, _channel: &str) -> Self {
            Self {
                source_text: String::new(),
                source_char_positions: Vec::new(),
                segments: Vec::new(),
            }
        }
    }

    #[derive(Clone, Debug)]
    pub struct LayoutCacheKey {
        message_id: String,
        width_bits: u32,
        show_timestamp: bool,
    }

    impl LayoutCacheKey {
        pub fn new(
            message_id: impl AsRef<str>,
            container_width: f32,
            show_timestamp: bool,
        ) -> Self {
            Self {
                message_id: message_id.as_ref().to_string(),
                width_bits: container_width.to_bits(),
                show_timestamp,
            }
        }

        pub fn from_shared(
            message_id: crate::SharedString,
            container_width: f32,
            show_timestamp: bool,
        ) -> Self {
            Self::new(message_id, container_width, show_timestamp)
        }
    }

    impl PartialEq for LayoutCacheKey {
        fn eq(&self, other: &Self) -> bool {
            self.message_id == other.message_id
                && self.width_bits == other.width_bits
                && self.show_timestamp == other.show_timestamp
        }
    }

    impl Eq for LayoutCacheKey {}

    impl Hash for LayoutCacheKey {
        fn hash<H: Hasher>(&self, state: &mut H) {
            self.message_id.hash(state);
            self.width_bits.hash(state);
            self.show_timestamp.hash(state);
        }
    }
}

#[path = "../../../../../../crates/echo-chat-render/src/selection.rs"]
pub mod selection;
