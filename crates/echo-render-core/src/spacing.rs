//! Scalar media-boundary spacing policy for chat layout.

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PriorContentKind {
    #[default]
    None,
    Text,
    Link,
    Emote,
}

#[derive(Clone, Copy, Debug)]
pub struct BoundarySpacingState {
    pub at_line_start: bool,
    pub prior_kind: PriorContentKind,
    pub authored_whitespace: bool,
    pub previous_media_height: Option<f32>,
    pub literal_space_width: f32,
    pub base_emote_size: f32,
}

fn scaled_media_gap(media_height: f32, base_emote_size: f32) -> f32 {
    (media_height.max(base_emote_size) * 0.16).ceil()
}

fn media_gap(state: BoundarySpacingState, media_height: f32) -> f32 {
    state
        .literal_space_width
        .max(scaled_media_gap(media_height, state.base_emote_size))
}

pub fn advance_before_text_like(state: BoundarySpacingState) -> f32 {
    if state.at_line_start {
        return 0.0;
    }
    match state.prior_kind {
        PriorContentKind::Link => state.literal_space_width,
        PriorContentKind::Emote => media_gap(
            state,
            state.previous_media_height.unwrap_or(state.base_emote_size),
        ),
        PriorContentKind::None | PriorContentKind::Text => 0.0,
    }
}

pub fn advance_before_emote_like(
    state: BoundarySpacingState,
    remove_emote_spacing: bool,
    incoming_media_height: f32,
) -> f32 {
    if state.at_line_start {
        return 0.0;
    }
    let adjacent_height = state
        .previous_media_height
        .unwrap_or(incoming_media_height)
        .max(incoming_media_height);
    let gap = media_gap(state, adjacent_height);
    match state.prior_kind {
        PriorContentKind::Text if !state.authored_whitespace => gap,
        PriorContentKind::Text => (gap - state.literal_space_width).max(0.0),
        PriorContentKind::Link => gap,
        PriorContentKind::Emote if !remove_emote_spacing => gap,
        PriorContentKind::None | PriorContentKind::Emote => 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EMOTE_SIZE: f32 = 28.0;

    fn spacing_state() -> BoundarySpacingState {
        BoundarySpacingState {
            at_line_start: false,
            prior_kind: PriorContentKind::None,
            authored_whitespace: false,
            previous_media_height: None,
            literal_space_width: 6.0,
            base_emote_size: EMOTE_SIZE,
        }
    }

    #[test]
    fn remove_emote_spacing_true_skips_emote_to_emote_gap() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Emote,
            ..spacing_state()
        };
        assert!(advance_before_emote_like(state, true, EMOTE_SIZE).abs() < 0.001);
    }

    #[test]
    fn remove_emote_spacing_false_keeps_emote_to_emote_gap() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Emote,
            ..spacing_state()
        };
        assert!(advance_before_emote_like(state, false, EMOTE_SIZE) > 0.0);
    }

    #[test]
    fn remove_emote_spacing_does_not_remove_text_to_emote_gap() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Text,
            ..spacing_state()
        };
        assert!(advance_before_emote_like(state, true, EMOTE_SIZE) > 0.0);
    }

    #[test]
    fn text_ending_with_whitespace_does_not_add_extra_gap_before_emote() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Text,
            authored_whitespace: true,
            ..spacing_state()
        };
        assert!(advance_before_emote_like(state, false, EMOTE_SIZE).abs() < 0.001);
    }

    #[test]
    fn emote_boundary_gap_scales_with_media_height() {
        let default_gap = advance_before_text_like(BoundarySpacingState {
            prior_kind: PriorContentKind::Emote,
            previous_media_height: Some(EMOTE_SIZE),
            ..spacing_state()
        });
        let large_gap = advance_before_text_like(BoundarySpacingState {
            prior_kind: PriorContentKind::Emote,
            previous_media_height: Some(EMOTE_SIZE * 3.0),
            ..spacing_state()
        });
        assert!(
            large_gap > default_gap,
            "larger rendered emotes should reserve a larger text boundary gap"
        );
    }

    #[test]
    fn incoming_large_emote_uses_media_aware_text_gap() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Text,
            ..spacing_state()
        };
        let default_gap = advance_before_emote_like(state, false, EMOTE_SIZE);
        let large_gap = advance_before_emote_like(state, false, EMOTE_SIZE * 3.0);
        assert!(
            large_gap > default_gap,
            "large incoming emotes should not sit against preceding text"
        );
    }

    #[test]
    fn incoming_large_emote_tops_up_authored_space() {
        let state = BoundarySpacingState {
            prior_kind: PriorContentKind::Text,
            authored_whitespace: true,
            ..spacing_state()
        };
        let default_gap = advance_before_emote_like(state, false, EMOTE_SIZE);
        let large_gap = advance_before_emote_like(state, false, EMOTE_SIZE * 3.0);
        assert!(default_gap.abs() < 0.001);
        assert!(
            large_gap > default_gap,
            "authored text whitespace should be topped up for large incoming emotes"
        );
    }
}
