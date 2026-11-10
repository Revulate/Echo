use serde::{Deserialize, Serialize};
use std::fmt;
use uuid::Uuid;

/// Stable, unique identifier for a split pane.
///
/// Unlike path-based identifiers (`Vec<usize>`), `SplitId` does not change
/// when the layout tree is modified. This allows state to persist even when
/// splits are rearranged.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SplitId(Uuid);

impl SplitId {
    /// Create a new unique split identifier.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Convert to a `u64` for use as an element ID (uses lower 64 bits).
    pub fn as_u64(&self) -> u64 {
        self.0.as_u64_pair().0
    }
}

impl Default for SplitId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for SplitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SplitId({})", &self.0.to_string()[..8])
    }
}

impl fmt::Display for SplitId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", &self.0.to_string()[..8])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_split_id_uniqueness() {
        let id1 = SplitId::new();
        let id2 = SplitId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_split_id_stability() {
        let id = SplitId::new();
        let restored = SplitId(id.0);
        assert_eq!(id, restored);
    }
}
