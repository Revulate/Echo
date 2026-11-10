//! Central dirty flags for UI invalidation sources that are not split-local.

use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

const SETTINGS_LAYOUT: u32 = 1 << 0;
const PROVIDER_METADATA: u32 = 1 << 1;
const WINDOW_STATE: u32 = 1 << 2;
const SHUTDOWN: u32 = 1 << 3;
const TOP_BAR_STATE: u32 = 1 << 4;

static DIRTY_FLAGS: AtomicU32 = AtomicU32::new(0);
static TOP_BAR_STATE_VERSION: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug)]
pub enum UiInvalidation {
    SettingsLayout,
    ProviderMetadata,
    WindowState,
    Shutdown,
    TopBarState,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct UiDirtyFlags(u32);

impl UiInvalidation {
    const fn bit(self) -> u32 {
        match self {
            Self::SettingsLayout => SETTINGS_LAYOUT,
            Self::ProviderMetadata => PROVIDER_METADATA,
            Self::WindowState => WINDOW_STATE,
            Self::Shutdown => SHUTDOWN,
            Self::TopBarState => TOP_BAR_STATE,
        }
    }
}

impl UiDirtyFlags {
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    pub const fn contains(self, invalidation: UiInvalidation) -> bool {
        self.0 & invalidation.bit() != 0
    }

    pub const fn bits(self) -> u32 {
        self.0
    }
}

pub fn request(invalidation: UiInvalidation) {
    if matches!(invalidation, UiInvalidation::TopBarState) {
        TOP_BAR_STATE_VERSION.fetch_add(1, Ordering::Release);
    }
    DIRTY_FLAGS.fetch_or(invalidation.bit(), Ordering::Release);
}

pub fn top_bar_state_version() -> u64 {
    TOP_BAR_STATE_VERSION.load(Ordering::Acquire)
}

pub fn drain() -> UiDirtyFlags {
    UiDirtyFlags(DIRTY_FLAGS.swap(0, Ordering::AcqRel))
}
