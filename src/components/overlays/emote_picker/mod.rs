//! Emote picker: one grid core shared by two hosts.
//!
//! - `inline`: the overlay anchored to a split's composer in the main window.
//! - `popout`: the standalone OS window that stays open while emotes are sent.
//!
//! Both hosts embed `grid::PickerGrid` (selection, virtual rows, search,
//! prefetch, load-more, keyboard navigation, tab bar and search field) and
//! render their grid through `rows`. They differ only in their
//! `grid::PickerHost` constants (viewport, element ids, tab list, metric
//! names, styling switches) and `grid::PickerShell` hooks (what activating an
//! emote, clearing the search and dismissing do), plus their own lifecycle,
//! chrome and animation wake loop. `signals` holds the per-surface row signals
//! that `uniform_list` callbacks raise; `core` the catalog, search ranking and
//! row packing; `prefetch` and `stability` the media plan and benchmark
//! telemetry.

mod core;
mod grid;
mod inline;
mod popout;
mod prefetch;
mod rows;
mod shared;
mod signals;
mod stability;

pub(crate) use self::core::record_emote_usage;
pub(super) use self::core::{
    EmoteScope, EmoteSearchIndex, RankedEmote, build_picker_emoji_emotes,
    emote_display_url_at_height,
};
pub(crate) use inline::EmotePickerEvent;
pub use inline::{EMOTE_PICKER_HEIGHT, EMOTE_PICKER_MAX_WIDTH, EmotePicker, EmotePickerState};
pub use popout::{
    EmotePickerPopout, PopoutMessage, benchmark_close_popout, drain_popout_messages,
    mark_popout_closed, open_emote_popout, popout_is_open, refresh_open_emote_picker_popout,
    send_to_main,
};
pub(crate) use popout::{open_emote_popout_for_split, popout_message_events};
