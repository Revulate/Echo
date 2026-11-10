//! Slot-based message parsing system
//!
//! Emotes are resolved immediately at parse time:
//! - If an emote isn't loaded, the word stays as text until the message is
//!   reparsed against a newer emote catalog
//! - No false positive emote lookups (no heuristics)
//! - No render-time resolution overhead

pub mod parser;
pub mod render_units;
pub mod resolver;

pub use parser::parse_to_slots;
pub(crate) use parser::resolve_input_emote_token;
pub use resolver::{ResolvedSlot, SlotResolver};

pub use echo_core::slot::{MessageSlot, compute_structure_hash, slots_semantically_equal};
