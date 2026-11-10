//! Scalar animation tick-interval policy.

pub const MAX_TICK_INTERVAL_MS: u64 = 100;
pub const STABLE_MIN_TICK_INTERVAL_MS: u64 = 1;
pub const DEFAULT_TICK_INTERVAL_MS: u64 = 33;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AnimationInterval {
    pub min_frame_duration_ms: u64,
    pub interval_ms: u64,
}

pub fn interval_from_min_durations(
    durations_ms: impl IntoIterator<Item = u64>,
    minimum_interval_ms: u64,
) -> AnimationInterval {
    let minimum = durations_ms
        .into_iter()
        .filter(|duration| *duration > 0)
        .min();
    let Some(min_frame_duration_ms) = minimum else {
        return AnimationInterval {
            min_frame_duration_ms: DEFAULT_TICK_INTERVAL_MS,
            interval_ms: DEFAULT_TICK_INTERVAL_MS,
        };
    };
    AnimationInterval {
        min_frame_duration_ms,
        interval_ms: min_frame_duration_ms.clamp(
            minimum_interval_ms.max(STABLE_MIN_TICK_INTERVAL_MS),
            MAX_TICK_INTERVAL_MS,
        ),
    }
}

/// Animation metadata
#[derive(Debug, Clone)]
pub struct AnimationMetadata {
    /// Number of frames
    pub frame_count: u16,
    /// Duration of each frame in milliseconds
    pub frame_durations_ms: Vec<u32>,
    /// Total animation duration in milliseconds
    pub total_duration_ms: u64,
    /// Smallest non-zero per-frame duration in milliseconds, or 0 when there are no
    /// frames. Consumed by the dynamic tick scheduler to bound its wake cadence to
    /// the fastest animation on screen without re-scanning `frame_durations_ms`.
    pub min_frame_duration_ms: u32,
    /// Cumulative frame-end times [d0, d0+d1, d0+d1+d2, …] for O(log n) frame lookup.
    /// Invariant: frame_cumulative_ms.len() == frame_durations_ms.len()
    frame_cumulative_ms: Vec<u64>,
}

impl AnimationMetadata {
    /// Create new metadata
    pub fn new(frame_durations_ms: Vec<u32>) -> Self {
        let frame_count = frame_durations_ms.len() as u16;
        let total_duration_ms: u64 = frame_durations_ms.iter().map(|&d| u64::from(d)).sum();

        // Build prefix-sum for O(log n) frame_index_at via partition_point.
        // Track the smallest non-zero frame duration in the same pass (zero extra cost).
        let mut frame_cumulative_ms = Vec::with_capacity(frame_durations_ms.len());
        let mut acc = 0u64;
        let mut min_frame_duration_ms = 0u32;
        for &d in &frame_durations_ms {
            acc += u64::from(d);
            frame_cumulative_ms.push(acc);
            if d > 0 && (min_frame_duration_ms == 0 || d < min_frame_duration_ms) {
                min_frame_duration_ms = d;
            }
        }

        Self {
            frame_count,
            frame_durations_ms,
            total_duration_ms,
            min_frame_duration_ms,
            frame_cumulative_ms,
        }
    }

    /// Calculate frame index for a given animation time (O(log n) binary search).
    #[inline]
    pub fn frame_index_at(&self, time_ms: u64) -> u16 {
        if self.total_duration_ms == 0 || self.frame_count == 0 {
            return 0;
        }

        let cycle_time = time_ms % self.total_duration_ms;
        // partition_point returns the first index where frame_cumulative_ms[i] > cycle_time,
        // which is exactly the frame whose cumulative end time exceeds cycle_time — same
        // result as the previous linear scan.
        self.frame_cumulative_ms
            .partition_point(|&t| t <= cycle_time) as u16
    }

    /// Milliseconds from `time_ms` until [`Self::frame_index_at`] next changes,
    /// or `None` when the image never changes frame.
    #[inline]
    pub fn ms_until_next_frame(&self, time_ms: u64) -> Option<u64> {
        if self.frame_count <= 1 || self.total_duration_ms == 0 {
            return None;
        }
        let cycle_time = time_ms % self.total_duration_ms;
        let index = self
            .frame_cumulative_ms
            .partition_point(|&t| t <= cycle_time);
        let frame_end = self
            .frame_cumulative_ms
            .get(index)
            .copied()
            .unwrap_or(self.total_duration_ms);
        Some(frame_end.saturating_sub(cycle_time).max(1))
    }

    /// Memory size estimate for metadata
    pub fn memory_size(&self) -> usize {
        std::mem::size_of::<Self>()
            + self.frame_durations_ms.len() * 4
            + self.frame_cumulative_ms.len() * 8
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_animation_metadata() {
        let metadata = AnimationMetadata::new(vec![100, 100, 100, 100]);

        assert_eq!(metadata.frame_count, 4);
        assert_eq!(metadata.total_duration_ms, 400);

        // Test frame index calculation
        assert_eq!(metadata.frame_index_at(0), 0);
        assert_eq!(metadata.frame_index_at(50), 0);
        assert_eq!(metadata.frame_index_at(100), 1);
        assert_eq!(metadata.frame_index_at(250), 2);
        assert_eq!(metadata.frame_index_at(350), 3);
        assert_eq!(metadata.frame_index_at(400), 0); // Wraps around
        assert_eq!(metadata.frame_index_at(500), 1); // 100ms into next cycle
    }

    #[test]
    fn ms_until_next_frame_lands_on_the_frame_change() {
        let metadata = AnimationMetadata::new(vec![100, 40, 0, 60]);

        assert_eq!(metadata.ms_until_next_frame(0), Some(100));
        assert_eq!(metadata.ms_until_next_frame(99), Some(1));
        assert_eq!(metadata.ms_until_next_frame(100), Some(40));
        assert_eq!(metadata.ms_until_next_frame(150), Some(50));
        assert_eq!(metadata.ms_until_next_frame(199), Some(1));
        assert_eq!(metadata.ms_until_next_frame(200), Some(100));

        for time_ms in 0..400 {
            let wait = metadata.ms_until_next_frame(time_ms).unwrap();
            let frame = metadata.frame_index_at(time_ms);
            assert_eq!(metadata.frame_index_at(time_ms + wait - 1), frame);
            assert_ne!(metadata.frame_index_at(time_ms + wait), frame);
        }
    }

    #[test]
    fn ms_until_next_frame_is_none_for_still_images() {
        assert_eq!(AnimationMetadata::new(vec![]).ms_until_next_frame(5), None);
        assert_eq!(
            AnimationMetadata::new(vec![80]).ms_until_next_frame(5),
            None
        );
        assert_eq!(
            AnimationMetadata::new(vec![0, 0]).ms_until_next_frame(5),
            None
        );
    }

    #[test]
    fn test_empty_list() {
        let result = interval_from_min_durations([], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, DEFAULT_TICK_INTERVAL_MS);
    }

    #[test]
    fn test_single_30fps_emote() {
        let result = interval_from_min_durations([33], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, 33);
    }

    #[test]
    fn test_single_60fps_emote() {
        let result = interval_from_min_durations([16], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, 16);
    }

    #[test]
    fn test_single_10fps_emote() {
        let result = interval_from_min_durations([100], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, 100);
    }

    #[test]
    fn test_mixed_fps_emotes() {
        let result = interval_from_min_durations([16, 100], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, 16);
    }

    #[test]
    fn test_variable_frame_durations() {
        let result = interval_from_min_durations([30], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, 30);
    }

    #[test]
    fn test_min_clamp() {
        let result = interval_from_min_durations([5], 20);
        assert_eq!(result.interval_ms, 20);
    }

    #[test]
    fn test_max_clamp() {
        let result = interval_from_min_durations([500], STABLE_MIN_TICK_INTERVAL_MS);
        assert_eq!(result.interval_ms, MAX_TICK_INTERVAL_MS);
    }
}
