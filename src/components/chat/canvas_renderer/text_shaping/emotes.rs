//! Emote placement, emote tooltip metadata and emote sizing.

use super::badges::{humanize_channel_display, humanize_compound_label};
use super::builder::PendingLayeredImage;
use super::*;
use echo_emote_core::EmoteScope;

#[inline]
pub(super) fn emote_is_animated(emote: &crate::backend::emotes::Emote, render_url: &str) -> bool {
    if emote.is_animated || is_animated_url(render_url) {
        return true;
    }

    emote.url_4x.as_deref().is_some_and(is_animated_url)
        || emote.url_2x.as_deref().is_some_and(is_animated_url)
        || emote.url_1x.as_deref().is_some_and(is_animated_url)
        || is_animated_url(&emote.url)
}

pub(super) fn build_emote_tooltip_meta(
    emote: &crate::backend::emotes::Emote,
    still_image: bool,
) -> EmoteTooltipMeta {
    use crate::backend::emotes::{EmoteProvider, get_twitch_emote_by_id_any_cached};

    let canonical_twitch = if emote.provider == EmoteProvider::Twitch
        && (emote.twitch_owner_login.is_none() || emote.twitch_emote_type.is_none())
    {
        get_twitch_emote_by_id_any_cached(emote.id.as_ref())
    } else {
        None
    };
    let emote = canonical_twitch.as_deref().unwrap_or(emote);

    let provider_label: SharedString = match emote.provider {
        EmoteProvider::SevenTV => "7TV",
        EmoteProvider::BTTV => "BTTV",
        EmoteProvider::FFZ => "FFZ",
        EmoteProvider::Twitch => "Twitch",
        EmoteProvider::Emoji => "Emoji",
    }
    .into();

    let (source_label, owner_channel): (SharedString, Option<SharedString>) = match emote.provider {
        EmoteProvider::SevenTV => match emote.channel.as_deref() {
            _ if emote.scope() == EmoteScope::Personal => ("7TV Personal".into(), None),
            Some(channel) if !channel.is_empty() => (
                "7TV Channel".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("7TV Global".into(), None),
        },
        EmoteProvider::BTTV => match emote.channel.as_deref() {
            Some(channel) if !channel.is_empty() => (
                "BTTV Channel/Shared".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("BTTV Global".into(), None),
        },
        EmoteProvider::FFZ => match emote.channel.as_deref() {
            Some(channel) if !channel.is_empty() => (
                "FFZ Channel".into(),
                emote.channel.clone().map(SharedString::from),
            ),
            _ => ("FFZ Global".into(), None),
        },
        EmoteProvider::Twitch => {
            if let Some(emote_type) = emote.twitch_emote_type.as_deref() {
                let owner_from_fields = emote
                    .twitch_owner_login
                    .as_ref()
                    .filter(|s| !s.is_empty())
                    .cloned()
                    .map(SharedString::from);
                match emote_type {
                    "subscriptions" => ("Twitch Subscriber Emote".into(), owner_from_fields),
                    "follower" => ("Twitch Follower Emote".into(), owner_from_fields),
                    "bitstier" | "bits" => ("Twitch Bits Emote".into(), owner_from_fields),
                    "prime" => ("Twitch Prime Emote".into(), None),
                    "limitedtime" | "limited" => ("Twitch Limited Emote".into(), None),
                    "hypetrain" => ("Twitch Hype Train Emote".into(), None),
                    "rewards" => ("Twitch Rewards Emote".into(), None),
                    "turbo" => ("Twitch Turbo Emote".into(), None),
                    "globals" | "smilies" | "twofactor" => ("Twitch Global Emote".into(), None),
                    other => {
                        let pretty = humanize_compound_label(other);
                        (
                            SharedString::from(format!("Twitch {} Emote", pretty)),
                            owner_from_fields,
                        )
                    }
                }
            } else if let EmoteScope::Subscription(owner) = emote.scope() {
                let owner = (!owner.is_empty()).then(|| owner.to_string().into());
                ("Twitch Subscriber Emote".into(), owner)
            } else {
                match emote.channel.as_deref() {
                    Some(channel) if channel.starts_with("twitch:") => {
                        let category = channel.trim_start_matches("twitch:");
                        let pretty = match category {
                            "subscriptions" => "Subscriber".into(),
                            "follower" => "Follower".into(),
                            "bits" => "Bits".into(),
                            "channel_points" => "Channel Points".into(),
                            "prime" => "Prime".into(),
                            "turbo" => "Turbo".into(),
                            "rewards" => "Rewards".into(),
                            other => humanize_compound_label(other),
                        };
                        (SharedString::from(format!("Twitch {} Emote", pretty)), None)
                    }

                    Some(channel) if !channel.is_empty() => (
                        SharedString::from(format!(
                            "Twitch Emote • {}",
                            humanize_channel_display(channel)
                        )),
                        emote.channel.clone().map(SharedString::from),
                    ),
                    _ => ("Twitch Emote".into(), None),
                }
            }
        }
        EmoteProvider::Emoji => ("Emoji".into(), None),
    };

    let creator_name = emote.creator_name.clone().map(SharedString::from);
    let original_name = emote.original_name.clone().map(SharedString::from);
    let tooltip_preview_url = emote
        .url_4x
        .clone()
        .or_else(|| emote.url_2x.clone())
        .or_else(|| emote.url_1x.clone())
        .map(SharedString::from)
        .or_else(|| Some(SharedString::from(emote.url.clone())));

    EmoteTooltipMeta {
        provider_label,
        source_label,
        owner_channel,
        creator_name,
        original_name,
        tooltip_preview_url,
        native_width: emote.width,
        native_height: emote.height,
        is_animated: emote_is_animated(emote, &emote.url),
        is_zero_width: emote.zero_width,
        emote_id: SharedString::from(emote.id.clone()),
        provider_key: echo_core::emote_display_rules::provider_key(emote.provider),
        still_image,
    }
}

/// Lay out one base emote with its zero-width overlays.
///
/// `still[i]` forces the static first frame for the base (`i == 0`) or overlay
/// `i - 1`; missing entries animate as usual.
pub(super) fn add_emote_render_unit(
    builder: &mut LayoutBuilder<'_>,
    base: &crate::backend::emotes::Emote,
    overlays: &[std::sync::Arc<crate::backend::emotes::Emote>],
    still: &[bool],
    remove_emote_spacing: bool,
) {
    let render_url = chat_display_url_shared_for_scale(
        base,
        crate::components::chat::canvas_renderer::layout_scale::current(),
    );

    let base_still = still.first().copied().unwrap_or(false);
    let is_animated = !base_still && emote_is_animated(base, &render_url);
    if crate::log::emote_trace_enabled() {
        crate::log_emotes!(
            debug,
            "[LAYOUT] Emote '{}': is_animated={} (flag={}, url_heuristic={}), provider={:?}, url={}, overlays={}",
            base.name,
            is_animated,
            base.is_animated,
            is_animated_url(&render_url),
            base.provider,
            render_url,
            overlays.len()
        );
    }

    let had_text = builder.flush_text_segment();

    builder.flush_layered_emote();

    if had_text {
        builder.last_token_type = LastTokenType::Text;
    }

    let ((base_width, base_height), base_dimensions_pending) =
        emote_dimensions_from_decoded_media(base, &render_url);
    if base_dimensions_pending {
        builder.track_pending_native_dimension_url(&render_url);
    }
    advance_before_emote_like(builder, remove_emote_spacing, base_height);

    let base_tooltip_meta = std::sync::Arc::new(build_emote_tooltip_meta(base, base_still));

    let mut layered_emote = LayeredEmote::new(
        SharedString::from(render_url),
        SharedString::from(base.name.clone()),
        base_width,
        base_height,
        is_animated,
        Some(std::sync::Arc::clone(&base_tooltip_meta)),
    );

    for (overlay_index, overlay) in overlays.iter().enumerate() {
        let overlay_url = chat_display_url_shared_for_scale(
            overlay,
            crate::components::chat::canvas_renderer::layout_scale::current(),
        );
        let overlay_still = still.get(overlay_index + 1).copied().unwrap_or(false);
        let overlay_is_animated = !overlay_still && emote_is_animated(overlay, &overlay_url);
        let ((overlay_width, overlay_height), overlay_dimensions_pending) =
            emote_dimensions_from_decoded_media(overlay, &overlay_url);
        if overlay_dimensions_pending {
            builder.track_pending_native_dimension_url(&overlay_url);
        }
        layered_emote.add_overlay(
            SharedString::from(overlay_url),
            SharedString::from(overlay.name.clone()),
            overlay_width,
            overlay_height,
            overlay_is_animated,
            Some(std::sync::Arc::new(build_emote_tooltip_meta(
                overlay,
                overlay_still,
            ))),
        );
    }

    let (bbox_width, _) = layered_emote.bounds();
    builder.wrap_unless_fits(bbox_width);

    let image_type = ImageType::Emote {
        name: SharedString::from(base.name.clone()),
        tooltip_meta: Some(base_tooltip_meta),
    };

    builder.pending_layered_emote = Some(PendingLayeredImage {
        emote: layered_emote,
        x: builder.current_x,
        image_type,
    });

    builder.advance(base_width);

    builder.last_token_type = LastTokenType::Emote;
}

/// The message's slots and a cursor over its emote slots. Render units keep
/// every emote slot in order (overlays included), so the n-th emote laid out is
/// the n-th emote slot.
pub(super) struct EmoteSlotSource<'a> {
    pub(super) slots: &'a [crate::backend::slots::MessageSlot],
    pub(super) cursor: &'a mut usize,
}

impl EmoteSlotSource<'_> {
    pub(super) fn next_emote_slot(&mut self) -> Option<usize> {
        while let Some(slot) = self.slots.get(*self.cursor) {
            let index = *self.cursor;
            *self.cursor += 1;
            if slot.emote_source_text().is_some() {
                return Some(index);
            }
        }
        None
    }
}

/// Lay out an emote unit under the active display rules.
///
/// An emote shown as text renders exactly as its code would as plain message
/// text, so it also ends the zero-width stack: the next image emote starts a new
/// unit as its base, the way a zero-width emote with no base renders.
pub(super) fn add_emote_render_unit_with_rules(
    builder: &mut LayoutBuilder<'_>,
    unit: &EmoteRenderUnit,
    mut source: EmoteSlotSource<'_>,
    rules: &settings::CompiledEmoteDisplayRules,
    channel: &str,
    text_color: Hsla,
    remove_emote_spacing: bool,
) {
    let mut flags: SmallVec<[settings::EmoteDisplayFlags; 5]> =
        SmallVec::with_capacity(unit.overlays.len() + 1);
    let mut slot_indices: SmallVec<[Option<usize>; 5]> =
        SmallVec::with_capacity(unit.overlays.len() + 1);
    unit.for_each_emote(|emote| {
        flags.push(rules.flags_for(emote.provider, &emote.id, channel));
        slot_indices.push(source.next_emote_slot());
    });
    let emote_at = |index: usize| {
        if index == 0 {
            &unit.base
        } else {
            &unit.overlays[index - 1]
        }
    };

    let mut index = 0;
    while index < flags.len() {
        if flags[index].show_as_text {
            let emote = emote_at(index);
            let slot = slot_indices[index];
            let code = slot
                .and_then(|slot| source.slots[slot].emote_source_text())
                .unwrap_or(&emote.name);
            let glued = slot.is_some_and(|slot| {
                echo_core::slot_parser::emote_slot_is_glued(source.slots, slot)
            });
            add_emote_code_as_text(builder, emote, code, glued, text_color);
            index += 1;
            continue;
        }
        let end = (index + 1..flags.len())
            .find(|&next| flags[next].show_as_text)
            .unwrap_or(flags.len());
        let still: SmallVec<[bool; 5]> = flags[index..end]
            .iter()
            .map(|flags| flags.still_image)
            .collect();
        add_emote_render_unit(
            builder,
            emote_at(index),
            &unit.overlays[index..end - 1],
            &still,
            remove_emote_spacing,
        );
        index = end;
    }
}

/// Render an emote's `code` (what was typed) as message text. The code joins
/// the copy/selection source like any word, and an emote hitbox over it keeps
/// the tooltip (a still preview) and the emote context menu with "Show image
/// again" reachable.
pub(super) fn add_emote_code_as_text(
    builder: &mut LayoutBuilder<'_>,
    emote: &crate::backend::emotes::Emote,
    code: &str,
    glued: bool,
    color: Hsla,
) {
    builder.flush_layered_emote();
    // The parser drops the whitespace before an emote and keeps the one after
    // it, so restore a separating space the way the text would have had one,
    // except after punctuation the emote was typed glued to, as in "(KEKW)".
    if matches!(builder.last_token_type, LastTokenType::Emote) {
        builder.ensure_source_separator();
    } else if !glued && !builder.last_text_ended_with_whitespace && !builder.source_text.is_empty()
    {
        push_text_segment(builder, " ", color);
    }
    builder.flush_text_segment();

    push_text_segment(builder, code, color);
    let render_url = chat_display_url_shared_for_scale(
        emote,
        crate::components::chat::canvas_renderer::layout_scale::current(),
    );
    builder.add_hitbox(HitboxKind::Emote {
        name: SharedString::from(emote.name.clone()),
        url: SharedString::from(render_url),
        meta: Some(std::sync::Arc::new(build_emote_tooltip_meta(emote, true))),
    });
    builder.flush_text_segment();
    builder.last_token_type = LastTokenType::Text;
}

/// Return the selected source URL's pixel density for native-size conversion.
///
/// Echo stores Twitch's `3.0` and BTTV's `3x` maximum variants in `url_4x`.
/// Twitch's normal `3.0` response is four times the 1x geometry (112px for a
/// 28px emote), so URL labels are not used as a literal divisor here. Historical
/// Twitch exceptions use their verified normalization factors, including the
/// fractional heart-emote density.
#[inline]
pub(super) fn selected_emote_source_density(
    emote: &crate::backend::emotes::Emote,
    url: &str,
) -> f32 {
    echo_emote_core::model::emote_source_density(emote, url)
}

/// Provider geometry fixes the image box before pixels arrive. When it is
/// absent, resident or remembered sizes from any tier avoid a square fallback.
#[inline]
pub(super) fn emote_dimensions_from_decoded_media(
    emote: &crate::backend::emotes::Emote,
    url: &str,
) -> ((f32, f32), bool) {
    let logical = emote
        .width
        .zip(emote.height)
        .filter(|(width, height)| *width > 0 && *height > 0)
        .or_else(|| {
            (emote.provider == EmoteProvider::Twitch)
                .then(|| echo_emote_core::twitch_base_dimensions(&emote.id))
        })
        .or_else(|| {
            [
                Some(url),
                emote.url_1x.as_deref(),
                emote.url_2x.as_deref(),
                emote.url_4x.as_deref(),
                Some(emote.url.as_ref()),
            ]
            .into_iter()
            .flatten()
            .find_map(|candidate| {
                logical_native_dimensions(
                    echo_chat_render::ports::decoded_image_size(candidate),
                    selected_emote_source_density(emote, candidate),
                    None,
                )
            })
        });
    let (width, height) = calculate_emote_dimensions(
        logical.map(|(width, _)| width),
        logical.map(|(_, height)| height),
        emote_size(),
    );
    ((width, height), logical.is_none())
}
