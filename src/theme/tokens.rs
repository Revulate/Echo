//! Design tokens for Echo GPUI
//!
//! Provides consistent spacing, radii, and font-size scales.
//! All values are static constants for zero runtime cost. Elevation uses the
//! GPUI `shadow_sm()` / `shadow_md()` / `shadow_lg()` presets directly.
//!
//! # Usage
//! ```rust
//! use echo_gpui::theme::tokens::{FontSize, Radii, Spacing};
//! use gpui::*;
//!
//! let _card = div()
//!     .p(px(Spacing::MD))
//!     .rounded(px(Radii::LG))
//!     .text_size(px(FontSize::BASE))
//!     .shadow_md();
//! ```

use gpui::{Pixels, px};

pub use echo_render_core::design_tokens::{FontSize, Radii, Sizes, Spacing};

/// Pixel helpers for the [`Radii`] scale.
///
/// The scale itself is GPUI-free and lives in
/// `echo_render_core::design_tokens`; `Pixels` cannot cross that boundary, so
/// the conversions are an extension trait here. Bring it into scope alongside
/// `Radii` to keep writing `Radii::md()` at call sites.
pub trait RadiiPixels {
    fn xs() -> Pixels;
    fn sm() -> Pixels;
    fn md() -> Pixels;
    fn lg() -> Pixels;
    fn xl() -> Pixels;
    fn xxl() -> Pixels;
    fn full() -> Pixels;
}

impl RadiiPixels for Radii {
    #[inline]
    fn xs() -> Pixels {
        px(Radii::XS)
    }

    #[inline]
    fn sm() -> Pixels {
        px(Radii::SM)
    }

    #[inline]
    fn md() -> Pixels {
        px(Radii::MD)
    }

    #[inline]
    fn lg() -> Pixels {
        px(Radii::LG)
    }

    #[inline]
    fn xl() -> Pixels {
        px(Radii::XL)
    }

    #[inline]
    fn xxl() -> Pixels {
        px(Radii::XXL)
    }

    #[inline]
    fn full() -> Pixels {
        px(Radii::FULL)
    }
}
