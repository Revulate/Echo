//! App settings adapters for GPUI-free similarity algorithms.

use super::lock_utils::MutexExt;

pub use echo_text_core::similarity::*;

/// Get similarity settings from global settings.
pub fn get_similarity_settings() -> SimilaritySettings {
    super::settings::SETTINGS
        .lock_or_recover()
        .similarity
        .clone()
}

fn update_similarity_settings(update: impl FnOnce(&mut SimilaritySettings)) {
    {
        let mut settings = super::settings::SETTINGS.lock_or_recover();
        update(&mut settings.similarity);
    }
    super::settings::save_settings();
}

/// Set similarity enabled.
pub fn set_similarity_enabled(enabled: bool) {
    update_similarity_settings(|settings| settings.enabled = enabled);
}

/// Set similarity threshold.
pub fn set_similarity_threshold(threshold: f32) {
    let threshold = threshold.clamp(0.0, 1.0);
    update_similarity_settings(|settings| settings.threshold = threshold);
}

/// Set hide similar mode.
pub fn set_similarity_hide_similar(hide: bool) {
    update_similarity_settings(|settings| settings.hide_similar = hide);
}

/// Set color similar mode.
pub fn set_similarity_color_similar(color: bool) {
    update_similarity_settings(|settings| settings.color_similar = color);
}

/// Set same user only mode.
pub fn set_similarity_same_user_only(same_user: bool) {
    update_similarity_settings(|settings| settings.same_user_only = same_user);
}

/// Set max delay in seconds.
pub fn set_similarity_max_delay_secs(secs: u32) {
    let secs = secs.clamp(1, 60);
    update_similarity_settings(|settings| settings.max_delay_secs = secs);
}

/// Set max messages to check.
pub fn set_similarity_max_messages_to_check(count: u32) {
    let count = count.clamp(1, 20);
    update_similarity_settings(|settings| settings.max_messages_to_check = count);
}
