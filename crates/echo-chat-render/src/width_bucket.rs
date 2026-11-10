//! Width-bucket stabilization policy for layout cache keys.

use crate::layout::width_from_quantized;

/// Debounce width-bucket flips briefly to avoid rebuild thrash during layout settling.
pub const WIDTH_BUCKET_STABILIZE_MS: u128 = 60;
/// Eventually apply a pending width bucket even if it hovers near the boundary.
pub const WIDTH_BUCKET_FORCE_APPLY_MS: u128 = 240;
/// Boundary hysteresis for width-bucket flips to prevent ping-ponging on resize jitter.
/// Set to half a bucket (4px): 2px sat below typical GPUI/DWM resize rounding noise, so
/// resizes near a bucket edge triggered repeated full rebuilds (each flushing the local LRU).
pub const WIDTH_BUCKET_HYSTERESIS_PX: f32 = 4.0;

#[inline]
pub fn should_apply_quantized_width_bucket_change(
    old_quantized: u32,
    pending_q: u32,
    container_width: f32,
    elapsed_ms: u128,
) -> bool {
    if pending_q == old_quantized {
        return true;
    }
    if elapsed_ms < WIDTH_BUCKET_STABILIZE_MS {
        return false;
    }

    let force_apply = elapsed_ms >= WIDTH_BUCKET_FORCE_APPLY_MS;
    let hysteresis_satisfied = if old_quantized == 0 || pending_q == old_quantized {
        true
    } else if pending_q > old_quantized {
        container_width >= width_from_quantized(pending_q) + WIDTH_BUCKET_HYSTERESIS_PX
    } else {
        container_width <= width_from_quantized(old_quantized) - WIDTH_BUCKET_HYSTERESIS_PX
    };

    hysteresis_satisfied || force_apply
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_bucket_change_requires_stabilization_delay() {
        let old = 100_u32;
        let pending = 101_u32;
        let width = width_from_quantized(pending) + WIDTH_BUCKET_HYSTERESIS_PX + 1.0;

        assert!(!should_apply_quantized_width_bucket_change(
            old, pending, width, 59
        ));
        assert!(should_apply_quantized_width_bucket_change(
            old, pending, width, 60
        ));
    }

    #[test]
    fn width_bucket_growth_respects_hysteresis_threshold() {
        let old = 100_u32;
        let pending = 101_u32;
        let below = width_from_quantized(pending) + WIDTH_BUCKET_HYSTERESIS_PX - 0.25;
        let at_or_above = width_from_quantized(pending) + WIDTH_BUCKET_HYSTERESIS_PX;

        assert!(!should_apply_quantized_width_bucket_change(
            old, pending, below, 120
        ));
        assert!(should_apply_quantized_width_bucket_change(
            old,
            pending,
            at_or_above,
            120
        ));
    }

    #[test]
    fn width_bucket_shrink_respects_hysteresis_threshold() {
        let old = 101_u32;
        let pending = 100_u32;
        let above = width_from_quantized(old) - WIDTH_BUCKET_HYSTERESIS_PX + 0.25;
        let at_or_below = width_from_quantized(old) - WIDTH_BUCKET_HYSTERESIS_PX;

        assert!(!should_apply_quantized_width_bucket_change(
            old, pending, above, 120
        ));
        assert!(should_apply_quantized_width_bucket_change(
            old,
            pending,
            at_or_below,
            120
        ));
    }

    #[test]
    fn width_bucket_force_apply_overrides_hysteresis() {
        let old = 101_u32;
        let pending = 100_u32;
        let above_threshold = width_from_quantized(old) - WIDTH_BUCKET_HYSTERESIS_PX + 1.0;

        assert!(!should_apply_quantized_width_bucket_change(
            old,
            pending,
            above_threshold,
            WIDTH_BUCKET_FORCE_APPLY_MS - 1
        ));
        assert!(should_apply_quantized_width_bucket_change(
            old,
            pending,
            above_threshold,
            WIDTH_BUCKET_FORCE_APPLY_MS
        ));
    }
}
