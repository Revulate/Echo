extern crate bincode;
extern crate echo_core;
extern crate echo_emote_core;
extern crate postcard;
extern crate serde;
extern crate serde_json;
extern crate zstd;

pub use echo_emote_core::{Emote, EmoteProvider};

const DISK_CACHE_REFRESH_DELAY_SECS: u64 = 5 * 60;

macro_rules! log_persist {
    ($level:ident, $($arg:tt)*) => {
        let _ = format_args!($($arg)*);
    };
}
pub(crate) use log_persist;

#[path = "../../src/utils/atomic_file.rs"]
mod atomic_file;

mod utils {
    pub(crate) use crate::atomic_file::replace_file;
}

#[path = "../../src/backend/emotes/cache_codec.rs"]
mod cache_codec;
