//! The user's recent messages: rows resolved once per context change, then
//! rendered with inline emotes from the shared image cache.

use super::view::render_streamer_mode_placeholder;
use super::{RecentRowsKey, UserCard};
use crate::backend;
use crate::backend::irc::get_user_color;
use crate::backend::slots::{ResolvedSlot, SlotResolver};
use crate::cache::loader as image_cache;
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use crate::utils::{format_message_timestamp, parse_user_color_opt};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::sync::Arc;

impl UserCard {
    /// Rebuild the derived recent-message rows when the context, emote
    /// resolution, or the settings baked into them have moved. Slot resolution
    /// and string building happen here so a render only assembles elements.
    pub(super) fn ensure_recent_rows(
        &mut self,
        channel: &str,
        hide_user_details: bool,
        window_scale: f32,
    ) {
        let show_timestamps = backend::settings::should_show_message_timestamps(
            backend::bridge::get_cached_channel_live_status(channel),
        );
        let key = RecentRowsKey {
            context_generation: self.user_context_generation,
            emote_resolution_version: crate::state::VersionRegistry::emote_resolution_version(),
            channel_emote_version: crate::state::VersionRegistry::channel_emote_version(channel),
            global_emote_version: crate::state::VersionRegistry::global_emote_version(),
            user_emote_version: crate::state::VersionRegistry::user_emote_version(),
            layout_settings_version: backend::settings::get_layout_settings_version(),
            user_color_generation: backend::irc::user_color_generation(),
            show_timestamps,
            timestamp_format: backend::settings::get_message_timestamp_format(),
            hide_user_details,
            window_scale,
        };
        if self.recent_rows_key == Some(key) {
            return;
        }
        self.recent_rows_key = Some(key);
        self.recent_rows_generation = self.recent_rows_generation.wrapping_add(1);

        if hide_user_details {
            self.recent_rows = Vec::new();
            self.recent_row_animation_urls = Vec::new();
            return;
        }

        let resolver = SlotResolver::new(channel);
        let display_rules = backend::settings::emote_display_rules_snapshot();
        let mut rows = Vec::with_capacity(self.user_context.recent_messages.len());
        let mut animation_urls: Vec<String> = Vec::new();
        let mut seen_animation_urls: rustc_hash::FxHashSet<String> = Default::default();

        for msg in &self.user_context.recent_messages {
            let mut parts = Vec::with_capacity(msg.slots.len());
            for slot in msg.slots.iter() {
                parts.push(match resolver.resolve(slot) {
                    ResolvedSlot::Text(text) => RecentMessagePart::Text(SharedString::from(text)),
                    ResolvedSlot::TwitchGif { alt_text, .. } => {
                        RecentMessagePart::Text(SharedString::from(alt_text))
                    }
                    ResolvedSlot::Emote(emote) => {
                        let flags = display_rules
                            .as_deref()
                            .map(|rules| rules.flags_for(emote.provider, &emote.id, &msg.channel))
                            .unwrap_or_default();
                        if flags.show_as_text {
                            // What was typed, shown as message text.
                            let code = slot.emote_source_text().unwrap_or(&emote.name);
                            RecentMessagePart::Text(SharedString::from(code.to_string()))
                        } else {
                            let url = backend::emotes::chat_display_url_shared_for_scale(
                                &emote,
                                window_scale,
                            )
                            .to_string();
                            if emote.is_animated
                                && !flags.still_image
                                && animation_urls.len() < 64
                                && !seen_animation_urls.contains(url.as_str())
                            {
                                seen_animation_urls.insert(url.clone());
                                animation_urls.push(url.clone());
                            }
                            let image_id =
                                SharedString::from(format!("user-card-inline:{}", emote.name));
                            RecentMessagePart::Emote {
                                url: SharedString::from(url),
                                image_id,
                                emote,
                                still: flags.still_image,
                            }
                        }
                    }
                    ResolvedSlot::Url { url, .. } => {
                        RecentMessagePart::Url(SharedString::from(url))
                    }
                    ResolvedSlot::Mention(mention) => RecentMessagePart::Mention {
                        text: SharedString::from(format!("@{}", mention)),
                        color: get_user_color(&mention)
                            .as_deref()
                            .and_then(parse_user_color_opt),
                    },
                    ResolvedSlot::Emoji { emoji, url } => RecentMessagePart::Emoji {
                        image_id: SharedString::from(format!("user-card-inline:{}", emoji)),
                        url: SharedString::from(url),
                    },
                });
            }

            rows.push(RecentMessageRow {
                timestamp: show_timestamps
                    .then(|| format_message_timestamp(msg.timestamp))
                    .flatten()
                    .map(SharedString::from),
                parts,
            });
        }

        self.recent_rows = rows;
        self.recent_row_animation_urls = animation_urls;
    }

    pub(super) fn render_recent_messages(
        &self,
        hide_user_details: bool,
        _cx: &mut Context<Self>,
    ) -> impl IntoElement + use<> {
        let messages_count = self.user_context.recent_messages.len();

        div()
            .flex_1()
            .min_h_0()
            .when(!self.hosted_in_window, |el| el.max_h(px(200.0)))
            .overflow_hidden()
            .flex()
            .flex_col()
            .child(
                div()
                    .px_3()
                    .py_1p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(
                        div().flex().items_center().gap_1p5().child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .child("Recent Messages"),
                        ),
                    )
                    .when(!hide_user_details, |el| {
                        el.child(
                            div()
                                .px_1p5()
                                .py_0p5()
                                .rounded(Radii::xl())
                                .bg(colors::bg_hover())
                                .text_xs()
                                .text_color(colors::text_secondary())
                                .child(messages_count.to_string()),
                        )
                    }),
            )
            .child(
                div()
                    .id("user-card-messages")
                    .mx_3()
                    .mb_2()
                    .rounded(Radii::sm())
                    .bg(colors::bg_surface())
                    .overflow_y_scroll()
                    .when(self.hosted_in_window, |el| el.flex_1().min_h_0())
                    .when(!self.hosted_in_window, |el| el.max_h(px(150.0)))
                    .when(hide_user_details, |el| {
                        el.child(render_streamer_mode_placeholder(
                            "Recent messages hidden by streamer mode",
                        ))
                    })
                    .when(
                        !hide_user_details && self.user_context.recent_messages.is_empty(),
                        |el| {
                            el.child(
                                div()
                                    .p_3()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .text_center()
                                    .child("No messages found"),
                            )
                        },
                    )
                    .when(!hide_user_details && !self.recent_rows.is_empty(), |el| {
                        el.child(
                            div().children(self.recent_rows.iter().map(render_rich_message_row)),
                        )
                    }),
            )
    }
}

/// Height of inline emote/emoji images in user-card message rows.
const MESSAGE_ROW_EMOTE_HEIGHT: f32 = 20.0;

/// Resolved render inputs for one recent-message row, built once per context
/// change by `UserCard::ensure_recent_rows`.
pub(super) struct RecentMessageRow {
    timestamp: Option<SharedString>,
    parts: Vec<RecentMessagePart>,
}

/// One resolved slot of a recent-message row.
enum RecentMessagePart {
    Text(SharedString),
    Url(SharedString),
    Mention {
        text: SharedString,
        color: Option<Hsla>,
    },
    Emote {
        emote: Arc<crate::backend::emotes::Emote>,
        url: SharedString,
        image_id: SharedString,
        /// A display rule shows the static first frame.
        still: bool,
    },
    Emoji {
        url: SharedString,
        image_id: SharedString,
    },
}

/// Render one recent message. The card header already establishes the user,
/// so rows retain their timestamp and authored content without repeating the
/// same sender name and badges for every buffered message.
fn render_rich_message_row(row: &RecentMessageRow) -> AnyElement {
    let mut content = div()
        .flex_1()
        .min_w_0()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_x(px(Spacing::XS))
        .text_xs();

    for part in &row.parts {
        content = match part {
            RecentMessagePart::Text(text) => content.child(
                div()
                    .min_w_0()
                    .text_color(colors::text_primary())
                    .child(text.clone()),
            ),
            RecentMessagePart::Emote {
                emote,
                url,
                image_id,
                still,
            } => content.child(render_inline_emote(emote, url, image_id, *still)),
            RecentMessagePart::Url(url) => content.child(
                div()
                    .min_w_0()
                    .text_color(colors::accent_text())
                    .child(url.clone()),
            ),
            RecentMessagePart::Mention { text, color } => content.child(
                div()
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(color.unwrap_or_else(colors::accent))
                    .child(text.clone()),
            ),
            RecentMessagePart::Emoji { url, image_id } => content.child(render_inline_image(
                url,
                image_id,
                MESSAGE_ROW_EMOTE_HEIGHT * 0.85,
            )),
        };
    }

    div()
        .px_2()
        .py_1()
        .hover(|s| s.bg(colors::bg_hover()))
        .flex()
        .items_start()
        .gap_2()
        .when_some(row.timestamp.clone(), |element, timestamp| {
            element.child(
                div()
                    .w(px(52.0))
                    .flex_none()
                    .text_xs()
                    .text_color(colors::text_muted())
                    .child(timestamp),
            )
        })
        .child(content)
        .into_any_element()
}

/// Render an emote inline, preferring already-decoded frames from the shared
/// cache (instant; the same images chat just painted) and falling back to
/// GPUI's async image loading for anything not cached yet. A `still` emote uses
/// only Echo's static first frame, never an animated decode or `img(url)`.
fn render_inline_emote(
    emote: &Arc<crate::backend::emotes::Emote>,
    url: &SharedString,
    image_id: &SharedString,
    still: bool,
) -> AnyElement {
    let url_str: &str = url.as_ref();

    if still {
        if let Some(render_image) = image_cache::get_cached_render_image(url_str) {
            return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
        }
        return img(crate::components::ui::still_frame_source(
            url.clone(),
            image_cache::PreloadSource::VisibleChat,
        ))
        .id(ElementId::Name(image_id.clone()))
        .h(px(MESSAGE_ROW_EMOTE_HEIGHT))
        .min_w(px(MESSAGE_ROW_EMOTE_HEIGHT))
        .object_fit(ObjectFit::Contain)
        .into_any_element();
    }

    if emote.is_animated {
        if let Some(render_image) = image_cache::get_current_animated_render_image(url_str) {
            return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
        }
        if matches!(
            image_cache::get_animated_cache_state(url_str),
            image_cache::AnimatedCacheState::NotLoaded
        ) {
            image_cache::trigger_animated_decode(url_str, image_cache::PreloadSource::VisibleChat);
        }
    }
    if let Some(render_image) = image_cache::get_cached_render_image(url_str) {
        return render_image_element_sized(render_image, MESSAGE_ROW_EMOTE_HEIGHT);
    }

    render_inline_image(url, image_id, MESSAGE_ROW_EMOTE_HEIGHT)
}

/// Decoded image scaled to a row height, preserving aspect ratio.
fn render_image_element_sized(render_image: Arc<RenderImage>, height: f32) -> AnyElement {
    let image_size = render_image.size(0);
    let width: f32 = image_size.width.0 as f32;
    let image_height: f32 = image_size.height.0 as f32;
    let aspect = if image_height > 0.0 {
        width / image_height
    } else {
        1.0
    };

    img(ImageSource::Render(render_image))
        .h(px(height))
        .w(px(height * aspect))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}

/// URI-loaded inline image with a stable id (GPUI fetches asynchronously).
fn render_inline_image(url: &SharedString, image_id: &SharedString, height: f32) -> AnyElement {
    img(url.clone())
        .id(ElementId::Name(image_id.clone()))
        .h(px(height))
        .min_w(px(height))
        .object_fit(ObjectFit::Contain)
        .into_any_element()
}
