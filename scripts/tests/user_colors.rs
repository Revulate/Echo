//! Focused harness for `backend::irc::user_colors`.
//!
//! This intentionally stubs only the module's logging, config-root, atomic
//! write, and colorize-setting boundaries. It never supplies a config root, so
//! persistence calls cannot touch a real user profile.

#![allow(dead_code, unused_variables)]

#[macro_export]
macro_rules! log_cache {
    ($($tokens:tt)*) => {
        ()
    };
}

#[macro_export]
macro_rules! log_persist {
    ($($tokens:tt)*) => {
        ()
    };
}

mod benchmark_profile {
    pub fn config_root() -> Option<std::path::PathBuf> {
        None
    }
}

mod config {
    pub fn atomic_write_json(
        _file_path: &std::path::Path,
        _content: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        Ok(())
    }
}

mod backend {
    pub mod settings {
        pub fn get_colorize_users_without_color() -> bool {
            true
        }
    }
}

#[path = "../../src/backend/irc/user_colors.rs"]
mod user_colors;
