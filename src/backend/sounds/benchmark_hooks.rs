//! Hooks the feedback UI fixture uses to capture and restore the sound store; each is a no-op unless that fixture is enabled.

use super::*;

impl SoundStore {
    /// Restore the exact model captured by the disposable feedback fixture.
    pub fn restore_settings_for_benchmark(&self, settings: SoundSettings) -> bool {
        if !crate::bench::ui_fixture::feedback_ui_enabled() {
            return false;
        }
        let mut current = self.settings.write_or_recover();
        *current = settings;
        true
    }

    pub fn file_bytes_for_benchmark(&self) -> Option<Vec<u8>> {
        crate::bench::ui_fixture::feedback_ui_enabled()
            .then(|| fs::read(&self.file_path).ok())
            .flatten()
    }

    pub fn restore_file_bytes_for_benchmark(&self, bytes: Option<&[u8]>) -> bool {
        if !crate::bench::ui_fixture::feedback_ui_enabled() {
            return false;
        }
        match bytes {
            Some(bytes) => fs::write(&self.file_path, bytes).is_ok(),
            None if self.file_path.exists() => fs::remove_file(&self.file_path).is_ok(),
            None => true,
        }
    }
}
