//! Pointer-x to character-index mapping for shaped chat lines.

use crate::layout::ShapedLineData;

#[inline]
pub fn nearest_boundary_index(positions: &[f32], target_x: f32) -> usize {
    match positions.binary_search_by(|&pos| {
        pos.partial_cmp(&target_x)
            .unwrap_or(std::cmp::Ordering::Equal)
    }) {
        Ok(exact) => exact,
        Err(insert_point) => {
            if insert_point == 0 {
                0
            } else if insert_point >= positions.len() {
                positions.len() - 1
            } else {
                let dist_prev = (positions[insert_point - 1] - target_x).abs();
                let dist_curr = (positions[insert_point] - target_x).abs();
                if dist_prev < dist_curr {
                    insert_point - 1
                } else {
                    insert_point
                }
            }
        }
    }
}

#[inline]
pub fn line_char_index_for_x(line_data: &ShapedLineData, rel_x: f32) -> (usize, f32) {
    let positions = line_data.hit_test_char_positions();
    let visual_boundary = nearest_boundary_index(positions, rel_x).min(line_data.source_len_chars);
    let logical_idx = line_data.logical_index_from_visual_boundary(visual_boundary);
    let x_dist = (positions.get(visual_boundary).copied().unwrap_or(rel_x) - rel_x).abs();
    (logical_idx.min(line_data.source_len_chars), x_dist)
}

#[cfg(test)]
mod tests {
    use super::*;
    use smallvec::{SmallVec, smallvec};

    #[test]
    fn hit_test_uses_visual_to_logical_mapping_for_rtl_lines() {
        let line_data = ShapedLineData {
            text: "שלום".into(),
            gpui_runs: SmallVec::new(),
            gpui_runs_hash: 0,
            // Logical-order boundaries (source order).
            char_positions: smallvec![0.0, 1.0, 2.0, 3.0, 4.0],
            // Visual-order boundaries used during hit testing.
            visual_char_positions: vec![0.0, 1.0, 2.0, 3.0, 4.0],
            // The leftmost visual character is the final logical character.
            visual_to_logical: vec![3, 2, 1, 0],
            source_start_char: 0,
            source_len_chars: 4,
            seventv_paint: None,
        };

        // The left edge of the line is the logical end, the right edge the
        // logical start, so a drag across the word covers all of it.
        let carets: Vec<usize> = [0.2, 1.1, 2.0, 2.9, 3.8]
            .into_iter()
            .map(|x| line_char_index_for_x(&line_data, x).0)
            .collect();
        assert_eq!(carets, [4, 3, 2, 1, 0]);
    }

    #[test]
    fn hit_test_maps_mixed_direction_lines() {
        // Logical "abשלcd": the two Hebrew characters swap places visually.
        let line_data = ShapedLineData {
            text: "abשלcd".into(),
            gpui_runs: SmallVec::new(),
            gpui_runs_hash: 0,
            char_positions: smallvec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            visual_char_positions: vec![0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0],
            visual_to_logical: vec![0, 1, 3, 2, 4, 5],
            source_start_char: 0,
            source_len_chars: 6,
            seventv_paint: None,
        };

        let carets: Vec<usize> = [0.0, 1.0, 2.0, 3.0, 4.0, 5.0, 6.0]
            .into_iter()
            .map(|x| line_char_index_for_x(&line_data, x).0)
            .collect();
        assert_eq!(carets, [0, 1, 4, 3, 4, 5, 6]);
    }
}
