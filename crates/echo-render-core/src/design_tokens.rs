//! GPUI-free design token scales.
//!
//! Spacing, radii, font sizes, and component sizes are plain numbers so the
//! scale itself carries no toolkit dependency. Converting them to GPUI
//! `Pixels` stays in the application, next to the widgets that consume them.
//!
//! Each scale also documents how an off-scale literal maps onto it, so the
//! literal-to-token migration can be applied mechanically. Elevation has no
//! token type: use the GPUI `shadow_sm()` / `shadow_md()` / `shadow_lg()`
//! presets directly.

// ============================================================================
// Spacing - 4pt grid with two half-steps
// ============================================================================

/// Spacing constants on a 4pt grid.
///
/// Use these for all margins, padding, and gaps to maintain visual consistency.
///
/// # Scale
/// - `HAIRLINE` (1px): Rules, 1px nudges, divider insets
/// - `XXS` (2px): Label-to-helper gaps, tight inline nudges
/// - `XS` (4px): Inline gaps between icon and text, tight spacing
/// - `XS_PLUS` (6px): Half-step - menu/card insets, compact row gaps
/// - `SM` (8px): Message padding, small button padding, list item gaps
/// - `SM_PLUS` (10px): Half-step - action button padding, group gaps
/// - `MD` (12px): Container padding, standard gaps between elements
/// - `LG` (16px): Modal padding, section headers, larger gaps
/// - `XL` (20px): Wide panel padding
/// - `XXL` (24px): Major layout gutters, large section spacing
/// - `XXXL` (32px): Hero sections, maximum spacing
///
/// The two half-steps exist because 6px and 10px are established rhythms
/// (menu and settings metrics) that would visibly change if rounded.
///
/// # Mapping for off-scale literals
/// | literal | token |
/// |---------|-------|
/// | 0.5     | `HAIRLINE` |
/// | 3       | `XS` |
/// | 5       | `XS_PLUS` |
/// | 7, 9    | `SM` |
/// | 36      | `XXXL` as spacing; a 36px width/height is a size, not spacing |
pub struct Spacing;

impl Spacing {
    /// Hairline: 1px - Rules and 1px nudges
    pub const HAIRLINE: f32 = 1.0;

    /// Extra extra small: 2px - Label/helper gaps
    pub const XXS: f32 = 2.0;

    /// Extra small: 4px - Inline gaps (icon + text)
    pub const XS: f32 = 4.0;

    /// Half-step: 6px - Menu/card insets, compact row gaps
    pub const XS_PLUS: f32 = 6.0;

    /// Small: 8px - Message padding, small buttons
    pub const SM: f32 = 8.0;

    /// Half-step: 10px - Action button padding, group gaps
    pub const SM_PLUS: f32 = 10.0;

    /// Medium: 12px - Container padding, list gaps
    pub const MD: f32 = 12.0;

    /// Large: 16px - Modal padding, section headers
    pub const LG: f32 = 16.0;

    /// Extra large: 20px - Wide panel padding
    pub const XL: f32 = 20.0;

    /// Extra extra large: 24px - Major layout gutters
    pub const XXL: f32 = 24.0;

    /// Maximum: 32px - Hero sections
    pub const XXXL: f32 = 32.0;
}

// ============================================================================
// Border Radii
// ============================================================================

/// Border radius constants for consistent corner rounding.
///
/// # Scale
/// - `XS` (2px): Checkboxes, small tooltips, badges
/// - `SM` (4px): Small buttons, tags, chips
/// - `MD` (6px): Standard buttons, inputs, cards
/// - `LG` (8px): Large cards, user cards, popovers
/// - `XL` (10px): Modals, large panels
/// - `XXL` (12px): Dialogs, hero cards
/// - `FULL` (9999px): Pills, avatars, toggles (same as GPUI `rounded_full()`)
///
/// # Mapping for off-scale literals
/// | literal | token |
/// |---------|-------|
/// | 1, 1.5  | `XS` |
/// | 3       | `SM` |
/// | 5       | `MD` |
/// | 7       | `LG` |
/// | 14      | `XXL` |
/// | 15, 999 | `FULL` (15 rounds a 30px-tall pill) |
pub struct Radii;

impl Radii {
    /// Extra small: 2px - Checkboxes, small tooltips
    pub const XS: f32 = 2.0;

    /// Small: 4px - Small buttons, tags
    pub const SM: f32 = 4.0;

    /// Medium: 6px - Standard buttons, inputs
    pub const MD: f32 = 6.0;

    /// Large: 8px - Cards, user cards, popovers
    pub const LG: f32 = 8.0;

    /// Extra large: 10px - Modals, large panels
    pub const XL: f32 = 10.0;

    /// Extra extra large: 12px - Dialogs, hero cards
    pub const XXL: f32 = 12.0;

    /// Fully rounded: pills, avatars, toggles
    pub const FULL: f32 = 9999.0;
}

// ============================================================================
// Font Sizes
// ============================================================================

/// Font size constants.
///
/// `XS` (11px) is the declared minimum for any readable UI text.
///
/// # Scale
/// - `XS` (11px): Helper text, shortcuts, timestamps - the minimum
/// - `SM` (12px): Secondary labels, autocomplete rows
/// - `BASE` (13px): Body text, labels, menu items, tab titles
/// - `MD` (14px): Section titles
/// - `LG` (15px): Page/dialog titles
/// - `XL` (18px): Large headings
/// - `XXL` (20px): Hero headings
/// - `DISPLAY_SM` (28px): Display text
/// - `DISPLAY` (32px): Largest display text
///
/// # Mapping for off-scale literals
/// | literal  | token | note |
/// |----------|-------|------|
/// | 8, 9, 10 | `XS`  | Raised to the 11px minimum; a visible change |
pub struct FontSize;

impl FontSize {
    /// Extra small: 11px - The minimum readable UI text size
    pub const XS: f32 = 11.0;

    /// Small: 12px - Secondary labels
    pub const SM: f32 = 12.0;

    /// Base: 13px - Body text, labels, menu items
    pub const BASE: f32 = 13.0;

    /// Medium: 14px - Section titles
    pub const MD: f32 = 14.0;

    /// Large: 15px - Page/dialog titles
    pub const LG: f32 = 15.0;

    /// Extra large: 18px - Large headings
    pub const XL: f32 = 18.0;

    /// Extra extra large: 20px - Hero headings
    pub const XXL: f32 = 20.0;

    /// Small display: 28px
    pub const DISPLAY_SM: f32 = 28.0;

    /// Display: 32px
    pub const DISPLAY: f32 = 32.0;
}

// ============================================================================
// Component-Specific Tokens
// ============================================================================

/// Standard sizes for common UI elements
pub struct Sizes;

impl Sizes {
    // Icons
    /// Large icon: 24px
    pub const ICON_LG: f32 = 24.0;

    // Buttons
    /// Button height small: 28px
    pub const BUTTON_SM: f32 = 28.0;
    /// Avatar medium: 32px
    pub const AVATAR_MD: f32 = 32.0;

    // Overlay positioning
    /// User card width: 320px
    pub const USER_CARD_WIDTH: f32 = 320.0;
    /// Context menu min width: 160px
    pub const CONTEXT_MENU_MIN_WIDTH: f32 = 160.0;
    /// Autocomplete width: 300px
    pub const AUTOCOMPLETE_WIDTH: f32 = 300.0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_strictly_increasing(scale: &[f32]) {
        for pair in scale.windows(2) {
            assert!(pair[0] < pair[1], "{} is not below {}", pair[0], pair[1]);
        }
    }

    #[test]
    fn spacing_scale_is_ordered_and_on_grid() {
        let scale = [
            Spacing::HAIRLINE,
            Spacing::XXS,
            Spacing::XS,
            Spacing::XS_PLUS,
            Spacing::SM,
            Spacing::SM_PLUS,
            Spacing::MD,
            Spacing::LG,
            Spacing::XL,
            Spacing::XXL,
            Spacing::XXXL,
        ];
        assert_strictly_increasing(&scale);
        for value in &scale[1..] {
            assert_eq!(value % 2.0, 0.0, "{value} is off the 2px grid");
        }
        for value in [
            Spacing::XS,
            Spacing::SM,
            Spacing::MD,
            Spacing::LG,
            Spacing::XL,
            Spacing::XXL,
            Spacing::XXXL,
        ] {
            assert_eq!(value % 4.0, 0.0, "{value} is off the 4px grid");
        }
    }

    #[test]
    fn radii_scale_is_ordered() {
        assert_strictly_increasing(&[
            Radii::XS,
            Radii::SM,
            Radii::MD,
            Radii::LG,
            Radii::XL,
            Radii::XXL,
            Radii::FULL,
        ]);
    }

    #[test]
    fn font_size_scale_is_ordered_and_respects_minimum() {
        assert_strictly_increasing(&[
            FontSize::XS,
            FontSize::SM,
            FontSize::BASE,
            FontSize::MD,
            FontSize::LG,
            FontSize::XL,
            FontSize::XXL,
            FontSize::DISPLAY_SM,
            FontSize::DISPLAY,
        ]);
        assert_eq!(FontSize::XS, 11.0);
    }
}
