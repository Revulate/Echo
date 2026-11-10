//! IRC connection and message handling for Twitch chat.
//!
//! `twitch-irc` owns the connection and typed-message boundary; `format` holds
//! the text helpers that live ingestion, history and chat replay share.

mod connection;
pub mod format;
mod join_scheduler;
mod proxy_transport;
pub mod room_state;
mod user_colors;

pub use connection::*;
pub use user_colors::*;
