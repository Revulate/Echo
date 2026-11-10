//! Provider source dimensions converted to logical chat geometry.

/// Convert decoded source dimensions into logical 1x dimensions.
///
/// `source_scale` is the density of the selected URL (1, 2, or 4). Twitch's
/// `3.0` and BTTV's `3x` labels normally serve 4x pixels (28/56/112); callers
/// pass 4 for those selected variants. The fallback dimensions are already
/// logical and are returned when decoding has not published native dimensions
/// yet.
#[inline]
pub fn logical_native_dimensions(
    decoded: Option<(u32, u32)>,
    source_scale: f32,
    fallback: Option<(u32, u32)>,
) -> Option<(u32, u32)> {
    let (width, height) = match decoded {
        Some(dimensions) => dimensions,
        None => return fallback,
    };
    if width == 0 || height == 0 {
        return fallback;
    }
    let divisor = if source_scale.is_finite() && source_scale > 0.0 {
        source_scale
    } else {
        1.0
    };
    Some((
        (width as f32 / divisor).round().max(1.0) as u32,
        (height as f32 / divisor).round().max(1.0) as u32,
    ))
}

#[cfg(test)]
mod tests {
    use super::logical_native_dimensions;

    #[test]
    fn normalizes_source_density_without_changing_ratio() {
        assert_eq!(
            logical_native_dimensions(Some((112, 56)), 4.0, None),
            Some((28, 14))
        );
        assert_eq!(
            logical_native_dimensions(Some((84, 84)), 3.0, None),
            Some((28, 28))
        );
        assert_eq!(
            logical_native_dimensions(Some((56, 112)), 2.0, None),
            Some((28, 56))
        );
        assert_eq!(
            logical_native_dimensions(None, 2.0, Some((28, 28))),
            Some((28, 28))
        );
        assert_eq!(
            logical_native_dimensions(Some((112, 84)), 14.0 / 3.0, None),
            Some((24, 18))
        );
    }
}
