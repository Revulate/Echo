//! IRC connection and message handling for Twitch chat.
//!
//! `twitch-irc` owns the connection and typed-message boundary. The
//! `tmi_adapter` module retains the allocation-aware formatting helpers used by
//! live ingestion and deterministic chat replay.

mod connection;
mod proxy_transport;
pub mod room_state;
pub mod tmi_adapter;
mod user_colors;

pub use connection::*;
pub use user_colors::*;
