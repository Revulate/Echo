//! Badge and 7TV cosmetic images in the card header, their tooltips, and the
//! subscription summary derived from badges.

use crate::backend;
use crate::backend::cosmetics_core::SevenTvBadge;
use crate::cache::loader as image_cache;
use crate::components::overlays::text_tooltip_on_hover;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels};
use echo_core::badge_key::badge_display_name;
use gpui::*;

/// Render a badge image
pub(super) fn render_badge(badge_key: &str, channel: &str) -> impl IntoElement + use<> {
    let url = backend::get_badge_url(badge_key, channel);
    let source_id = format!("user-card-badge-{}", badge_key.replace(['/', ':'], "-"));
    let tooltip_text = if backend::is_using_ffz_authority_badge(badge_key, channel) {
        let badge_name = backend::badges::badge_key_name(badge_key);
        format!("FFZ Channel {} Badge", badge_display_name(badge_name))
    } else {
        user_card_badge_tooltip_text(badge_key)
    };

    if let Some(url) = url {
        div()
            .id(source_id.clone())
            .size(px(18.0))
            .mr_0p5()
            .on_hover(text_tooltip_on_hover(
                source_id.clone(),
                tooltip_text.clone(),
            ))
            .child(img(url).size(px(18.0)).object_fit(ObjectFit::Contain))
            .into_any_element()
    } else {
        div()
            .id(source_id.clone())
            .size(px(18.0))
            .mr_0p5()
            .rounded(Radii::xs())
            .bg(colors::bg_elevated())
            .flex()
            .items_center()
            .justify_center()
            .text_xs()
            .on_hover(text_tooltip_on_hover(
                source_id.clone(),
                tooltip_text.clone(),
            ))
            .child(
                badge_key
                    .chars()
                    .next()
                    .unwrap_or('?')
                    .to_uppercase()
                    .to_string(),
            )
            .into_any_element()
    }
}

fn user_card_badge_tooltip_text(badge_key: &str) -> String {
    let (name, version) = badge_name_and_version(badge_key);

    match (name.as_str(), version) {
        ("subscriber", Some(months)) => format!("Subscriber Badge - {months} months"),
        ("founder", Some(months)) => format!("Founder Badge - {months} months"),
        ("bits", Some(amount)) => format!("Bits Badge - {amount}"),
        ("sub-gifter", Some(count)) => format!("Sub Gifter Badge - {count}"),
        ("ffz", Some(kind)) => format!("FFZ Badge - {}", humanize_badge_label(kind)),
        ("ffz-bot", Some(kind)) => {
            format!("FFZ Bot Badge - {}", humanize_badge_label(kind))
        }
        ("bttv", Some(kind)) => format!("BTTV Badge - {}", humanize_badge_label(kind)),
        ("bttv-pro", Some(kind)) | ("betterttv-pro", Some(kind)) => {
            format!("BTTV Pro Badge - {}", humanize_badge_label(kind))
        }
        _ => {
            if let Some(version) = version {
                format!(
                    "{} Badge - {}",
                    badge_display_name(&name),
                    humanize_badge_label(version)
                )
            } else {
                format!("{} Badge", badge_display_name(&name))
            }
        }
    }
}

fn humanize_badge_label(value: &str) -> String {
    value
        .split(['-', '_'])
        .filter(|part| !part.is_empty())
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                Some(first) => {
                    first.to_uppercase().collect::<String>() + &chars.as_str().to_ascii_lowercase()
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

pub(super) fn render_7tv_badge(badge: &SevenTvBadge) -> AnyElement {
    let source_id = format!("user-card-7tv-badge-{}", badge.id);
    let tooltip_text = seven_tv_badge_tooltip_text(badge);

    div()
        .id(source_id.clone())
        .size(px(18.0))
        .mr_0p5()
        .on_hover(text_tooltip_on_hover(
            source_id.clone(),
            tooltip_text.clone(),
        ))
        .child(render_cosmetic_image(
            &badge.url,
            18.0,
            ObjectFit::Contain,
            badge.is_animated,
        ))
        .into_any_element()
}

fn seven_tv_badge_tooltip_text(badge: &SevenTvBadge) -> String {
    let mut parts = Vec::new();
    if !badge.name.trim().is_empty() {
        parts.push(badge.name.trim().to_string());
    }
    if !badge.tooltip.trim().is_empty() && badge.tooltip.trim() != badge.name.trim() {
        parts.push(badge.tooltip.trim().to_string());
    }
    if badge.is_animated {
        parts.push("Animated 7TV badge".to_string());
    } else {
        parts.push("7TV badge".to_string());
    }
    parts.join(" - ")
}

pub(super) fn render_cosmetic_image(
    url: &str,
    size_px: f32,
    object_fit: ObjectFit,
    is_animated: bool,
) -> AnyElement {
    let size = px(size_px);
    if is_animated {
        if let Some(render_image) = image_cache::get_current_animated_render_image(url) {
            if let Some(metadata) = image_cache::get_animation_metadata(url) {
                crate::emote_renderer::register_visible_metadata(metadata);
            }
            return img(render_image)
                .size(size)
                .object_fit(object_fit)
                .into_any_element();
        }
        if let Some(render_image) = image_cache::get_cached_render_image(url) {
            return img(render_image)
                .size(size)
                .object_fit(object_fit)
                .into_any_element();
        }
        if matches!(
            image_cache::get_animated_cache_state(url),
            image_cache::AnimatedCacheState::NotLoaded
        ) {
            image_cache::trigger_animated_decode(url, image_cache::PreloadSource::VisibleLayout);
        }
    }

    if is_animated {
        return img(SharedString::from(url))
            .id((
                "user-card-cosmetic-animation",
                stable_image_instance_id(url),
            ))
            .size(size)
            .object_fit(object_fit)
            .into_any_element();
    }
    img(SharedString::from(url))
        .size(size)
        .object_fit(object_fit)
        .into_any_element()
}

fn stable_image_instance_id(url: &str) -> u64 {
    use std::hash::{Hash, Hasher};

    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    url.hash(&mut hasher);
    hasher.finish()
}

fn subscription_summary_from_badges(badges: &[String]) -> Option<String> {
    let mut subscriber_months = None;
    let mut has_founder = false;
    let mut has_bttv_pro = false;

    for badge in badges {
        let (name, version) = badge_name_and_version(badge);
        match name.as_str() {
            "subscriber" => {
                subscriber_months = version.and_then(|value| value.parse::<u32>().ok());
            }
            "founder" => {
                has_founder = true;
            }
            "bttv-pro" | "betterttv-pro" if version == Some("subscriber") => {
                has_bttv_pro = true;
            }
            _ => {}
        }
    }

    let mut parts = Vec::new();
    match (subscriber_months, has_founder) {
        (Some(1), true) => parts.push("Founder - 1 month".to_string()),
        (Some(months), true) => parts.push(format!("Founder - {months} months")),
        (Some(1), false) => parts.push("1 month".to_string()),
        (Some(months), false) => parts.push(format!("{months} months")),
        (None, true) => parts.push("Founder".to_string()),
        (None, false) => {}
    }
    if has_bttv_pro {
        parts.push("BTTV Pro Subscriber".to_string());
    }

    (!parts.is_empty()).then(|| parts.join(" - "))
}

pub(super) fn subscription_summary_from_live_and_badges(
    subscription_info: Option<&backend::twitch_api::TwitchUserSubscriptionInfo>,
    badges: &[String],
) -> Option<String> {
    let badge_summary = subscription_summary_from_badges(badges);
    let Some(info) = subscription_info.filter(|info| info.is_subscribed) else {
        return badge_summary;
    };

    let mut parts = Vec::new();
    parts.push(
        format_subscription_tier(info.tier.as_deref())
            .or_else(|| info.plan_name.clone())
            .unwrap_or_else(|| "Subscribed".to_string()),
    );

    if let Some(months) = badge_summary
        && !parts.iter().any(|part| part == &months)
    {
        parts.push(months);
    }

    if info.is_gift {
        let gifter = info
            .gifter_name
            .as_deref()
            .or(info.gifter_login.as_deref())
            .map(str::trim)
            .filter(|name| !name.is_empty());
        match gifter {
            Some(name) => parts.push(format!("gifted by {name}")),
            None => parts.push("gifted".to_string()),
        }
    }

    Some(parts.join(" - "))
}

fn format_subscription_tier(tier: Option<&str>) -> Option<String> {
    match tier.map(str::trim).filter(|tier| !tier.is_empty())? {
        "1000" => Some("Tier 1".to_string()),
        "2000" => Some("Tier 2".to_string()),
        "3000" => Some("Tier 3".to_string()),
        other => Some(other.to_string()),
    }
}

fn badge_name_and_version(badge: &str) -> (String, Option<&str>) {
    let badge = badge.trim();
    if let Some((name, version)) = badge.split_once('/') {
        return (name.to_ascii_lowercase(), Some(version));
    }
    if let Some((name, version)) = badge.split_once(':') {
        return (name.to_ascii_lowercase(), Some(version));
    }
    (badge.to_ascii_lowercase(), None)
}
