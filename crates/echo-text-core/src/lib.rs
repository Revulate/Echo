//! GPUI-free emoji and text-similarity logic shared by Echo and verification tools.

pub mod emoji;
pub mod similarity;
mod style;
pub mod timestamp;

pub use emoji::{TextSegment, get_emoji_url, parse_emoji};
pub use similarity::{
    SimilarityCandidate, SimilarityMatch, SimilaritySettings, calculate_similarity,
    detect_similarity, levenshtein_distance,
};
pub use style::EmojiStyle;
