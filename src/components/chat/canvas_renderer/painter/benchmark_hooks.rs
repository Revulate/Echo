//! Teardown hook for the GPUI benches (`gpui-bench` feature).

use super::*;

impl CanvasMessageList {
    /// Release retained wake loops and report whether benchmark-only async
    /// layout work has drained, allowing GPUI's leak detector to tear down the
    /// benchmark window without weakening leak detection globally.
    pub fn prepare_benchmark_teardown(&mut self) -> bool {
        self.release_animation_waker();
        self.release_async_layout_waker();
        self.release_image_load_waker();
        self.release_width_stabilization_waker();
        self.poll_async_layouts();
        !self.async_layout_manager.has_in_flight()
    }
}
