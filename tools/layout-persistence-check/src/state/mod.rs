pub mod workspace_state {
    pub use echo_core::workspace::*;
}

pub use workspace_state::{SplitNode, SplitOrientation};

#[path = "../../../../src/state/tab_state.rs"]
pub mod tab_state;

pub use tab_state::{TabHighlightState, TabState, TabVisibility};
