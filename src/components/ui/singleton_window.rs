//! Process-wide bookkeeping for a native window that exists at most once.
//!
//! Each popout (emote picker, user card, media inspector, debug, streamer
//! overlay) keeps one static [`SingletonWindow`]. It owns the open flag, the
//! live window handle, and the payload a caller asked to show while the window
//! was already open; the window's root view takes that payload on its next
//! render. Opening the window itself stays with the caller, through
//! [`super::open_native_window`].

use crate::backend::lock_utils::MutexExt;
use gpui::{AnyWindowHandle, App};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

pub struct SingletonWindow<P> {
    open: AtomicBool,
    handle: Mutex<Option<AnyWindowHandle>>,
    pending: Mutex<Option<P>>,
}

/// Outcome of [`SingletonWindow::retarget`].
pub enum Retarget<P> {
    /// The live window was handed the payload, activated and refreshed.
    Retargeted,
    /// No window is open; the payload comes back for a fresh open.
    NotOpen(P),
    /// The window was marked open but its handle no longer resolves. The
    /// payload comes back; the caller must run its close bookkeeping before
    /// reopening.
    Stale(P),
}

impl<P> SingletonWindow<P> {
    pub const fn new() -> Self {
        Self {
            open: AtomicBool::new(false),
            handle: Mutex::new(None),
            pending: Mutex::new(None),
        }
    }

    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::SeqCst)
    }

    /// Set the open flag, returning whether it was already set.
    pub fn mark_open(&self) -> bool {
        self.open.swap(true, Ordering::SeqCst)
    }

    /// Clear the open flag, the handle and any pending payload, returning
    /// whether the window was marked open.
    pub fn mark_closed(&self) -> bool {
        let was_open = self.open.swap(false, Ordering::SeqCst);
        *self.handle.lock_or_recover() = None;
        *self.pending.lock_or_recover() = None;
        was_open
    }

    pub fn set_handle(&self, handle: AnyWindowHandle) {
        *self.handle.lock_or_recover() = Some(handle);
    }

    /// The window handle, copied out so the lock is never held across a GPUI
    /// window update: an update can run application code that reopens or
    /// closes this window.
    pub fn handle(&self) -> Option<AnyWindowHandle> {
        *self.handle.lock_or_recover()
    }

    pub fn set_pending(&self, payload: P) {
        *self.pending.lock_or_recover() = Some(payload);
    }

    pub fn take_pending(&self) -> Option<P> {
        self.pending.lock_or_recover().take()
    }

    /// Inspect the pending payload without taking it.
    pub fn with_pending<R>(&self, read: impl FnOnce(Option<&P>) -> R) -> R {
        read(self.pending.lock_or_recover().as_ref())
    }

    /// If the window is open, queue `payload` for it, then activate and
    /// refresh it so its next render applies the payload.
    pub fn retarget(&self, payload: P, cx: &mut App) -> Retarget<P> {
        if !self.is_open() {
            return Retarget::NotOpen(payload);
        }
        self.set_pending(payload);
        let activated = self.handle().is_some_and(|handle| {
            handle
                .update(cx, |_, window, cx| {
                    cx.activate(true);
                    window.activate_window();
                    window.refresh();
                })
                .is_ok()
        });
        if activated {
            return Retarget::Retargeted;
        }
        // A failed update ran nothing in the window, so the payload queued
        // above is still there.
        let Some(payload) = self.take_pending() else {
            unreachable!("pending payload queued above was taken by a dead window");
        };
        Retarget::Stale(payload)
    }

    /// Ask the live window to close. A handle that no longer resolves (or no
    /// handle at all) clears the bookkeeping directly; otherwise the window's
    /// root view clears it when it drops.
    pub fn close(&self, cx: &mut App) -> bool {
        let removed = self.handle().is_some_and(|handle| {
            handle
                .update(cx, |_, window, _| window.remove_window())
                .is_ok()
        });
        if !removed {
            self.mark_closed();
        }
        removed
    }

    /// Refresh the live window, returning whether it was reached.
    pub fn refresh(&self, cx: &mut App) -> bool {
        self.is_open()
            && self
                .handle()
                .is_some_and(|handle| handle.update(cx, |_, window, _| window.refresh()).is_ok())
    }
}

impl<P> Default for SingletonWindow<P> {
    fn default() -> Self {
        Self::new()
    }
}
