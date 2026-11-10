//! GPUI-free confirmation and toast state policy.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

/// Cheat sheet state
#[derive(Clone, Debug, Default)]
pub struct HotkeyCheatSheetState {
    pub is_open: bool,
}

impl HotkeyCheatSheetState {
    pub fn close(&mut self) {
        self.is_open = false;
    }
}

#[derive(Clone, Debug, Default)]
pub struct ConfirmationState {
    pub is_open: bool,
    pub title: String,
    pub message: String,
    pub confirm_text: String,
    pub cancel_text: String,
    pub alternate_text: Option<String>,
    pub tertiary_text: Option<String>,
    pub is_destructive: bool,
    pub confirm_focused: bool,
}

impl ConfirmationState {
    pub fn close(&mut self) {
        self.is_open = false;
    }

    pub fn toggle_focus(&mut self) {
        self.confirm_focused = !self.confirm_focused;
    }
}

#[derive(Clone, Debug, PartialEq, Default)]
pub enum ToastType {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl ToastType {
    pub fn icon(&self) -> &'static str {
        match self {
            ToastType::Info => "i",
            ToastType::Success => "✓",
            ToastType::Warning => "!",
            ToastType::Error => "✕",
        }
    }
}

/// Optional button on a toast that runs a callback and dismisses the toast.
#[derive(Clone, Default)]
pub struct ToastAction {
    label: Option<String>,
    run: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
}

impl std::fmt::Debug for ToastAction {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ToastAction")
            .field("label", &self.label)
            .finish()
    }
}

impl ToastAction {
    pub fn new(label: impl Into<String>, run: impl Fn() + Send + Sync + 'static) -> Self {
        Self {
            label: Some(label.into()),
            run: Some(Arc::new(run)),
        }
    }

    pub fn has_button(&self) -> bool {
        self.label.is_some() && self.run.is_some()
    }

    pub fn label(&self) -> Option<&str> {
        self.label.as_deref()
    }

    pub fn run(&self) {
        if let Some(run) = &self.run {
            run();
        }
    }
}

static NEXT_TOAST_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Clone)]
pub struct Toast {
    pub id: u64,
    pub toast_type: ToastType,
    pub message: String,
    pub detail: Option<String>,
    pub duration_secs: u8,
    pub created_at: Instant,
    pub paused_duration: Duration,
    pub paused_at: Option<Instant>,
    pub on_click: Option<Arc<dyn Fn() + Send + Sync + 'static>>,
    pub action: ToastAction,
}

impl std::fmt::Debug for Toast {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Toast")
            .field("id", &self.id)
            .field("toast_type", &self.toast_type)
            .field("message", &self.message)
            .field("detail", &self.detail)
            .field("duration_secs", &self.duration_secs)
            .field("paused", &self.paused_at.is_some())
            .field("has_click_handler", &self.on_click.is_some())
            .field("action", &self.action)
            .finish()
    }
}

impl Toast {
    pub fn new(toast_type: ToastType, message: impl Into<String>) -> Self {
        Self {
            id: NEXT_TOAST_ID.fetch_add(1, Ordering::SeqCst),
            toast_type,
            message: message.into(),
            detail: None,
            duration_secs: 5,
            created_at: Instant::now(),
            paused_duration: Duration::ZERO,
            paused_at: None,
            on_click: None,
            action: ToastAction::default(),
        }
    }

    pub fn info(message: impl Into<String>) -> Self {
        Self::new(ToastType::Info, message)
    }

    pub fn success(message: impl Into<String>) -> Self {
        Self::new(ToastType::Success, message)
    }

    pub fn warning(message: impl Into<String>) -> Self {
        Self::new(ToastType::Warning, message)
    }

    pub fn error(message: impl Into<String>) -> Self {
        Self::new(ToastType::Error, message)
    }

    pub fn with_duration(mut self, secs: u8) -> Self {
        self.duration_secs = secs.clamp(3, 10);
        self
    }

    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    pub fn with_on_click(mut self, on_click: impl Fn() + Send + Sync + 'static) -> Self {
        self.on_click = Some(Arc::new(on_click));
        self
    }

    pub fn with_action(
        mut self,
        label: impl Into<String>,
        run: impl Fn() + Send + Sync + 'static,
    ) -> Self {
        self.action = ToastAction::new(label, run);
        self
    }

    pub fn pause(&mut self) {
        if self.paused_at.is_none() {
            self.paused_at = Some(Instant::now());
        }
    }

    pub fn resume(&mut self) {
        if let Some(paused_at) = self.paused_at.take() {
            self.paused_duration += paused_at.elapsed();
        }
    }

    pub fn is_paused(&self) -> bool {
        self.paused_at.is_some()
    }

    pub fn effective_elapsed(&self) -> Duration {
        let total_elapsed = self.created_at.elapsed();
        let current_pause = self
            .paused_at
            .map(|paused| paused.elapsed())
            .unwrap_or(Duration::ZERO);
        total_elapsed.saturating_sub(self.paused_duration + current_pause)
    }

    pub fn is_expired(&self) -> bool {
        if self.is_paused() {
            return false;
        }
        self.effective_elapsed().as_secs() >= self.duration_secs as u64
    }
}

#[derive(Clone, Debug, Default)]
pub struct ToastManagerState {
    pub toasts: Vec<Toast>,
    pub hovered_id: Option<u64>,
}

impl ToastManagerState {
    /// Toasts shown at once; pushing past this drops the oldest.
    pub const MAX_TOASTS: usize = 5;

    pub fn push(&mut self, toast: Toast) {
        while self.toasts.len() >= Self::MAX_TOASTS {
            self.toasts.remove(0);
        }
        self.toasts.push(toast);
    }

    pub fn dismiss(&mut self, id: u64) {
        self.toasts.retain(|toast| toast.id != id);
    }

    pub fn pause(&mut self, id: u64) {
        if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
            toast.pause();
        }
    }

    pub fn resume(&mut self, id: u64) {
        if let Some(toast) = self.toasts.iter_mut().find(|toast| toast.id == id) {
            toast.resume();
        }
    }

    pub fn remove_expired(&mut self) -> bool {
        let before = self.toasts.len();
        self.toasts.retain(|toast| !toast.is_expired());
        self.toasts.len() != before
    }

    pub fn has_toasts(&self) -> bool {
        !self.toasts.is_empty()
    }

    pub fn set_hovered(&mut self, id: u64) {
        if let Some(old_id) = self.hovered_id
            && old_id != id
        {
            self.resume(old_id);
        }
        self.hovered_id = Some(id);
        self.pause(id);
    }

    pub fn clear_hovered(&mut self) {
        if let Some(id) = self.hovered_id.take() {
            self.resume(id);
        }
    }

    pub fn is_hovered(&self, id: u64) -> bool {
        self.hovered_id == Some(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_state_close() {
        let mut state = HotkeyCheatSheetState { is_open: true };
        state.close();
        assert!(!state.is_open);
    }

    fn make_state(title: &str, message: &str, is_destructive: bool) -> ConfirmationState {
        ConfirmationState {
            is_open: true,
            title: title.to_string(),
            message: message.to_string(),
            confirm_text: if is_destructive { "Delete" } else { "Confirm" }.to_string(),
            cancel_text: "Cancel".to_string(),
            alternate_text: None,
            tertiary_text: None,
            is_destructive,
            confirm_focused: !is_destructive,
        }
    }

    #[test]
    fn test_confirmation_state_new() {
        let state = make_state("Delete Item", "Are you sure?", true);
        assert!(state.is_open);
        assert_eq!(state.title, "Delete Item");
        assert_eq!(state.message, "Are you sure?");
        assert!(state.is_destructive);
        assert!(!state.confirm_focused);
    }

    #[test]
    fn test_non_destructive_defaults() {
        let state = make_state("Confirm Action", "Proceed?", false);
        assert!(!state.is_destructive);
        assert!(state.confirm_focused);
        assert_eq!(state.confirm_text, "Confirm");
    }

    #[test]
    fn test_destructive_defaults() {
        let state = make_state("Delete", "Sure?", true);
        assert!(state.is_destructive);
        assert_eq!(state.confirm_text, "Delete");
    }

    #[test]
    fn test_toggle_focus() {
        let mut state = make_state("Test", "Test", false);
        assert!(state.confirm_focused);
        state.toggle_focus();
        assert!(!state.confirm_focused);
        state.toggle_focus();
        assert!(state.confirm_focused);
    }

    #[test]
    fn test_toast_creation() {
        let toast = Toast::info("Test message");
        assert_eq!(toast.toast_type, ToastType::Info);
        assert_eq!(toast.message, "Test message");
        assert_eq!(toast.duration_secs, 5);
        assert!(!toast.is_expired());
    }

    #[test]
    fn test_toast_with_detail() {
        let toast = Toast::success("Success!")
            .with_detail("Operation completed")
            .with_duration(3);
        assert_eq!(toast.detail, Some("Operation completed".to_string()));
        assert_eq!(toast.duration_secs, 3);
    }

    #[test]
    fn test_duration_clamping() {
        assert_eq!(Toast::info("Test").with_duration(1).duration_secs, 3);
        assert_eq!(Toast::info("Test").with_duration(20).duration_secs, 10);
    }

    #[test]
    fn test_toast_manager_max_toasts() {
        let mut state = ToastManagerState::default();
        for index in 0..10 {
            state.push(Toast::info(format!("Toast {index}")));
        }
        assert_eq!(state.toasts.len(), ToastManagerState::MAX_TOASTS);
    }
}
