//! Asset source for Echo GPUI
//!
//! Provides Echo's custom embedded assets (logo SVGs, icons, etc.)

use gpui::{AssetSource, Result, SharedString};
use rust_embed::RustEmbed;
use std::borrow::Cow;

/// Echo's embedded assets (icons, etc.)
#[derive(RustEmbed)]
#[folder = "assets"]
#[include = "icons/*.svg"]
pub struct EchoAssets;

impl AssetSource for EchoAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if path.is_empty() {
            return Ok(None);
        }
        Ok(EchoAssets::get(path).map(|f| f.data))
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        Ok(EchoAssets::iter()
            .filter_map(|p| p.starts_with(path).then(|| p.into()))
            .collect())
    }
}
