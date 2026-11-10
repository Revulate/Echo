//! Pointer, keyboard, hit-test, selection, and context-menu handling for CanvasMessageList.

use super::*;
use echo_chat_render::hit_testing;

/// Squared pointer travel, in pixels, under which a press and release count as a click.
const CLICK_DRAG_TOLERANCE_SQUARED: f32 = 3.0 * 3.0;

/// Distance from the chat's top or bottom edge, in pixels, inside which a
/// selection drag scrolls through history.
const SELECTION_EDGE_SCROLL_BAND: f32 = 32.0;
const SELECTION_EDGE_SCROLL_TICK: std::time::Duration = std::time::Duration::from_millis(16);

impl CanvasMessageList {
    /// Convert window position to element-local position
    pub(super) fn to_local_pos(&self, window_pos: Point<Pixels>) -> Point<Pixels> {
        if let Some(bounds) = &self.interaction_state.element_bounds {
            point(
                window_pos.x - bounds.origin.x,
                window_pos.y - bounds.origin.y,
            )
        } else {
            window_pos
        }
    }

    #[inline]
    pub(super) fn line_char_index_for_x(line_data: &ShapedLineData, rel_x: f32) -> (usize, f32) {
        echo_chat_render::hit_mapping::line_char_index_for_x(line_data, rel_x)
    }

    /// Resolve a char index + x-distance for an emote/image segment, mirroring
    /// `line_char_index_for_x`'s (char_index, x_dist) shape so both segment kinds
    /// can compete fairly in the same "best candidate" hit-test loop.
    ///
    /// Images aren't individually shaped text, so there's no per-glyph boundary
    /// to binary-search: a single left/right-half split is enough (click on the
    /// left half of the emote selects before its code, right half selects after).
    /// `x_dist` is the distance from `rel_x` to the nearest edge of the image's
    /// own bounds (0 when `rel_x` is inside it), matching how text's `x_dist`
    /// is 0 for an exact hit and grows with distance from the nearest glyph.
    #[inline]
    fn image_char_index_for_x(
        rel_x: f32,
        seg_x: f32,
        seg_width: f32,
        source_start_char: usize,
        source_len_chars: usize,
    ) -> (usize, f32) {
        let x_dist = if rel_x < seg_x {
            seg_x - rel_x
        } else if rel_x > seg_x + seg_width {
            rel_x - (seg_x + seg_width)
        } else {
            0.0
        };
        let char_index = if rel_x < seg_x + seg_width / 2.0 {
            source_start_char
        } else {
            source_start_char + source_len_chars
        };
        (char_index, x_dist)
    }

    /// Find message and character at local position (accounting for scroll)
    /// Uses HeightTree for O(log n) message lookup
    /// For multi-message selection, always returns a valid point (clamped to bounds)
    /// Returns SharedString instead of String to avoid allocation on every mouse move
    pub(super) fn hit_test_local(
        &self,
        local_pos: Point<Pixels>,
    ) -> Option<(SharedString, usize, usize)> {
        if self.message_model.message_order.is_empty() {
            return None;
        }

        let local_x: f32 = local_pos.x.into();
        let local_y: f32 = local_pos.y.into();

        let content_y = local_y + self.scroll_controller.offset;

        let mut idx = self.message_model.height_tree.index_at_y(content_y);

        if idx >= self.message_model.message_order.len() {
            idx = self.message_model.message_order.len() - 1;
        }

        let msg_id = &self.message_model.message_order[idx];
        let cache_key = self.layout_cache_key_from_shared(msg_id.clone());
        let layout = self
            .peek_cached_layout(&cache_key)
            .or_else(|| self.find_any_layout_for_message(msg_id))?;
        let msg_y = self.message_model.height_tree.y_position(idx);
        let msg_char_len = layout.source_char_len();

        let char_index = if content_y < msg_y {
            0
        } else if content_y >= msg_y + layout.height {
            msg_char_len
        } else {
            let rel_x = local_x;
            let rel_y = content_y - msg_y;
            let text_h = text_height();
            let line_band = |seg_y: f32, seg_height: f32| -> (f32, f32) {
                let top = seg_y - (seg_height - text_h).max(0.0);
                (top, top + seg_height)
            };

            let mut best: Option<(usize, f32)> = None;

            for segment in layout.segments.iter() {
                match segment {
                    LayoutSegment::Text {
                        line_idx,
                        y,
                        height,
                        ..
                    } => {
                        let Some(line_data) = layout.shaped_lines.get(*line_idx) else {
                            continue;
                        };
                        if line_data.source_len_chars == 0 || line_data.char_positions.is_empty() {
                            continue;
                        }

                        let (line_top, line_bottom) = line_band(*y, *height);
                        if rel_y < line_top || rel_y >= line_bottom {
                            continue;
                        }

                        let (idx_in_seg, x_dist) = Self::line_char_index_for_x(line_data, rel_x);
                        let char_index = line_data.source_start_char.saturating_add(idx_in_seg);

                        match best {
                            None => best = Some((char_index, x_dist)),
                            Some((_, best_dist)) if x_dist < best_dist => {
                                best = Some((char_index, x_dist))
                            }
                            _ => {}
                        }
                    }
                    LayoutSegment::LayeredImage {
                        x: seg_x,
                        y,
                        width,
                        height,
                        source_start_char,
                        source_len_chars,
                        ..
                    }
                    | LayoutSegment::Image {
                        x: seg_x,
                        y,
                        width,
                        height,
                        source_range: Some((source_start_char, source_len_chars)),
                        ..
                    } => {
                        if *height <= 0.0 || rel_y < *y || rel_y >= *y + *height {
                            continue;
                        }
                        let (char_index, x_dist) = Self::image_char_index_for_x(
                            rel_x,
                            *seg_x,
                            *width,
                            *source_start_char,
                            *source_len_chars,
                        );
                        match best {
                            None => best = Some((char_index, x_dist)),
                            Some((_, best_dist)) if x_dist < best_dist => {
                                best = Some((char_index, x_dist))
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }

            if let Some((char_index, _)) = best {
                char_index.min(msg_char_len)
            } else {
                let mut best: Option<(usize, f32)> = None;
                for segment in layout.segments.iter() {
                    let LayoutSegment::Text {
                        line_idx,
                        y,
                        height,
                        ..
                    } = segment
                    else {
                        continue;
                    };
                    let Some(line_data) = layout.shaped_lines.get(*line_idx) else {
                        continue;
                    };
                    if line_data.source_len_chars == 0 || line_data.char_positions.is_empty() {
                        continue;
                    }

                    let (line_top, line_bottom) = line_band(*y, *height);
                    let vdist = if rel_y < line_top {
                        line_top - rel_y
                    } else if rel_y > line_bottom {
                        rel_y - line_bottom
                    } else {
                        0.0
                    };

                    let (idx_in_seg, _) = Self::line_char_index_for_x(line_data, rel_x);
                    let char_index = line_data.source_start_char.saturating_add(idx_in_seg);

                    match best {
                        None => best = Some((char_index, vdist)),
                        Some((_, best_dist)) if vdist < best_dist => {
                            best = Some((char_index, vdist))
                        }
                        _ => {}
                    }
                }

                best.map(|(c, _)| c.min(msg_char_len)).unwrap_or(0)
            }
        };

        Some((msg_id.clone(), idx, char_index))
    }

    /// Find hitbox at local position
    fn hit_test_hitbox_local(
        &self,
        local_pos: Point<Pixels>,
    ) -> Option<&hit_testing::ManagedHitbox> {
        self.interaction_state.hitbox_manager.hit_test(local_pos)
    }

    /// Handle mouse down for selection and context menu
    pub(super) fn on_mouse_down(
        &mut self,
        window_pos: Point<Pixels>,
        button: MouseButton,
        click_count: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if button == MouseButton::Left && !self.is_active_split {
            let local_pos = self.to_local_pos(window_pos);
            self.interaction_state.pending_text_click = if click_count < 2 {
                self.pending_text_click_at(local_pos, window_pos)
            } else {
                None
            };
            return;
        }

        self.focus_handle.focus(window, cx);

        let local_pos = self.to_local_pos(window_pos);
        self.interaction_state.pending_text_click = None;

        if button == MouseButton::Left {
            self.interaction_state.pending_text_click = if click_count < 2 {
                self.pending_text_click_at(local_pos, window_pos)
            } else {
                None
            };

            if let Some((msg_id, msg_idx, char_idx)) = self.hit_test_local(local_pos) {
                let granularity = SelectionGranularity::from_click_count(click_count);
                let unit = self.selection_unit_range(&msg_id, char_idx, granularity);
                self.interaction_state.selection_granularity = granularity;
                self.interaction_state.granular_anchor = (granularity
                    != SelectionGranularity::Char)
                    .then(|| (msg_id.clone(), unit.clone()));
                self.interaction_state
                    .selection
                    .start_selection(SelectionPoint {
                        message_id: msg_id.clone(),
                        message_index: msg_idx,
                        char_index: unit.start,
                    });
                if unit.end != unit.start {
                    self.interaction_state
                        .selection
                        .extend_selection(SelectionPoint {
                            message_id: msg_id,
                            message_index: msg_idx,
                            char_index: unit.end,
                        });
                }
                self.message_state.pending_scroll_to_bottom = false;
                self.ensure_selection_edge_scroll(window.window_handle(), cx);
                crate::backend::metrics::record_ui_notify_source("canvas.selection_start");
                cx.notify();
            } else {
                self.interaction_state.selection.clear();
                crate::backend::metrics::record_ui_notify_source("canvas.selection_clear");
                cx.notify();
            }
        } else if button == MouseButton::Right {
            if let Some(hitbox) = self.hit_test_hitbox_local(local_pos) {
                match &hitbox.kind {
                    HitboxKind::Username { login, .. } => {
                        let message = self.get_message(&hitbox.message_id);
                        let can_reply = message
                            .map(|message| {
                                message.allows_remote_message_actions()
                                    && message.sender.eq_ignore_ascii_case(login)
                            })
                            .unwrap_or(false);
                        let behavior = if can_reply {
                            let modifiers = window.modifiers();
                            let modifier = settings::get_username_right_click_modifier();
                            if modifier.matches(
                                modifiers.control,
                                modifiers.alt,
                                modifiers.shift,
                                modifiers.platform,
                                modifiers.function,
                            ) {
                                settings::get_username_right_click_modifier_behavior()
                            } else {
                                settings::get_username_right_click_behavior()
                            }
                        } else {
                            settings::UsernameRightClickBehavior::Mention
                        };

                        match behavior {
                            settings::UsernameRightClickBehavior::Mention => {
                                if !login.starts_with("id:") {
                                    crate::backend::push_ui_action(
                                        crate::backend::UiAction::InsertMention {
                                            username: login.to_string(),
                                            channel: self.channel_name.clone(),
                                            source_split_id: Some(self.source_split_id),
                                        },
                                    );
                                }
                            }
                            settings::UsernameRightClickBehavior::Reply => {
                                if let Some(message) = message {
                                    crate::backend::push_ui_action(
                                        crate::backend::UiAction::Reply {
                                            channel: self.channel_name.clone(),
                                            source_split_id: Some(self.source_split_id),
                                            message_id: message.id.to_string(),
                                            sender: message.sender.to_string(),
                                            display_name: message.display_name.to_string(),
                                            content: message.content.to_string(),
                                        },
                                    );
                                }
                            }
                            settings::UsernameRightClickBehavior::Ignore => {}
                        }
                        return;
                    }
                    HitboxKind::Mention { username } => {
                        if !username.starts_with("id:") {
                            crate::backend::push_ui_action(
                                crate::backend::UiAction::InsertMention {
                                    username: username.to_string(),
                                    channel: self.channel_name.clone(),
                                    source_split_id: Some(self.source_split_id),
                                },
                            );
                        }
                        return;
                    }
                    _ => {}
                }
            }

            self.show_context_menu(window_pos, local_pos, cx);
        }
    }

    /// Char range a press selects at `char_index`: an insertion point, the
    /// word under it (emotes are one word), or the whole message.
    fn selection_unit_range(
        &self,
        message_id: &SharedString,
        char_index: usize,
        granularity: SelectionGranularity,
    ) -> std::ops::Range<usize> {
        if granularity == SelectionGranularity::Char {
            return char_index..char_index;
        }
        let Some(layout) = self.selection_layout(message_id) else {
            return char_index..char_index;
        };
        if granularity == SelectionGranularity::Word {
            echo_chat_render::selection::word_char_range_at(&layout, char_index)
        } else {
            0..layout.source_char_len()
        }
    }

    /// Endpoints for a drag that follows a double- or triple-click: the
    /// selection always covers the anchor unit and the unit under the pointer.
    fn granular_drag_endpoints(
        &self,
        message_id: &SharedString,
        message_index: usize,
        char_index: usize,
    ) -> Option<(SelectionPoint, SelectionPoint)> {
        let (anchor_id, anchor_range) = self.interaction_state.granular_anchor.as_ref()?;
        let anchor_index = self.get_message_index(anchor_id.as_ref())?;
        let focus_range = self.selection_unit_range(
            message_id,
            char_index,
            self.interaction_state.selection_granularity,
        );
        let forward = (message_index, focus_range.start) >= (anchor_index, anchor_range.start);
        let (anchor_char, focus_char) = if forward {
            (anchor_range.start, focus_range.end)
        } else {
            (anchor_range.end, focus_range.start)
        };
        Some((
            SelectionPoint {
                message_id: anchor_id.clone(),
                message_index: anchor_index,
                char_index: anchor_char,
            },
            SelectionPoint {
                message_id: message_id.clone(),
                message_index,
                char_index: focus_char,
            },
        ))
    }

    /// Stage a text-like hitbox under the press for activation on release.
    fn pending_text_click_at(
        &self,
        local_pos: Point<Pixels>,
        window_pos: Point<Pixels>,
    ) -> Option<PendingTextClick> {
        let Some(hitbox) = self.hit_test_hitbox_local(local_pos) else {
            return self.collapsed_deleted_click_at(local_pos, window_pos);
        };
        match &hitbox.kind {
            HitboxKind::Username { .. }
            | HitboxKind::Mention { .. }
            | HitboxKind::Url { .. }
            | HitboxKind::ReplyIndicator { .. }
            | HitboxKind::EmbedPreview { .. }
            | HitboxKind::MediaInspector { .. }
            | HitboxKind::AutomodAction { .. }
            | HitboxKind::ModAction { .. } => Some(PendingTextClick {
                message_id: hitbox.message_id.clone(),
                kind: hitbox.kind.clone(),
                x: window_pos.x.into(),
                y: window_pos.y.into(),
            }),
            _ => self.collapsed_deleted_click_at(local_pos, window_pos),
        }
    }

    /// The collapsed `<message deleted>` row under the pointer, if any. Clicking
    /// anywhere on that row expands it.
    fn collapsed_deleted_row_at(&self, local_pos: Point<Pixels>) -> Option<SharedString> {
        let content_y = f32::from(local_pos.y) + self.scroll_controller.offset;
        if content_y < 0.0 || content_y >= self.message_model.total_height {
            return None;
        }
        let idx = self.message_model.height_tree.index_at_y(content_y);
        let message_id = self.message_model.message_order.get(idx)?;
        self.is_collapsed_deleted(message_id)
            .then(|| message_id.clone())
    }

    fn collapsed_deleted_click_at(
        &self,
        local_pos: Point<Pixels>,
        window_pos: Point<Pixels>,
    ) -> Option<PendingTextClick> {
        let message_id = self.collapsed_deleted_row_at(local_pos)?;
        Some(PendingTextClick {
            message_id: message_id.to_string(),
            kind: HitboxKind::MessageBackground { message_id },
            x: window_pos.x.into(),
            y: window_pos.y.into(),
        })
    }

    /// Channel an emote's "Only in #channel" rule names: the message's own
    /// channel, which differs from this split's in Activity and Watching views.
    fn emote_rule_channel(&self, message_id: &str) -> String {
        self.get_message(message_id)
            .map(|message| message.channel.to_string())
            .unwrap_or_else(|| self.channel_name.clone())
    }

    /// Show context menu at position (delegates to OverlayManager via UiAction)
    fn show_context_menu(
        &mut self,
        window_pos: Point<Pixels>,
        local_pos: Point<Pixels>,
        _cx: &mut Context<Self>,
    ) {
        use crate::backend;

        let x: f32 = window_pos.x.into();
        let y: f32 = window_pos.y.into();

        let show_menu = |state: ContextMenuState| {
            backend::push_ui_action(backend::UiAction::ShowContextMenu(state));
        };

        let hitbox = self.hit_test_hitbox_local(local_pos);
        let mut object_menu = None;
        if let Some(hitbox) = hitbox {
            match &hitbox.kind {
                HitboxKind::Username {
                    login,
                    display_name,
                    user_id,
                    ..
                } => {
                    show_menu(ContextMenuState::open_chatter(
                        x,
                        y,
                        login.to_string(),
                        display_name.to_string(),
                        user_id.as_ref().map(ToString::to_string),
                        self.channel_name.clone(),
                        false,
                    ));
                    return;
                }
                HitboxKind::Mention { username } => {
                    show_menu(ContextMenuState::open_chatter(
                        x,
                        y,
                        username.to_string(),
                        username.to_string(),
                        None,
                        self.channel_name.clone(),
                        false,
                    ));
                    return;
                }
                HitboxKind::ReplyIndicator { parent_msg_id, .. } => {
                    backend::push_ui_action(backend::UiAction::ViewThread {
                        message_id: parent_msg_id.to_string(),
                        channel: self.channel_name.clone(),
                        x,
                        y,
                    });
                    return;
                }
                HitboxKind::Emote { name, url, meta } => {
                    object_menu = Some(ContextMenuState::open_emote(
                        x,
                        y,
                        name.to_string(),
                        url.to_string(),
                        meta.as_deref().cloned(),
                        self.channel_name.clone(),
                        self.emote_rule_channel(&hitbox.message_id),
                    ));
                }
                HitboxKind::LayeredEmote { emotes } => {
                    object_menu = Some(ContextMenuState::open_layered_emote(
                        x,
                        y,
                        emotes.clone(),
                        self.channel_name.clone(),
                        self.emote_rule_channel(&hitbox.message_id),
                    ));
                }
                HitboxKind::Url { url, embed_id, .. }
                | HitboxKind::EmbedPreview { url, embed_id, .. }
                | HitboxKind::MediaInspector { url, embed_id, .. } => {
                    object_menu = Some(ContextMenuState::open_link(
                        x,
                        y,
                        url.to_string(),
                        hitbox.message_id.clone(),
                        *embed_id,
                    ));
                }
                _ => {}
            }
        }

        let mut sections = Vec::with_capacity(3);
        if self.interaction_state.selection.has_selection() {
            let selected_text = self
                .interaction_state
                .selection
                .get_selected_text_with(&self.message_model.message_order, |message_id| {
                    self.selection_layout(message_id)
                });
            if !selected_text.is_empty() {
                sections.push(ContextMenuState::open_selection(x, y, selected_text));
            }
        }

        let message = match (object_menu.is_some(), hitbox) {
            (true, Some(hitbox)) => self.get_message(&hitbox.message_id),
            _ => self
                .hit_test_local(local_pos)
                .and_then(|(msg_id, _, _)| self.get_message(&msg_id)),
        };
        sections.extend(object_menu);
        if let Some(msg) = message {
            let is_whisper = matches!(
                msg.flags.highlight_type.as_ref(),
                Some(crate::backend::HighlightType::Whisper)
            );
            // A collapsed row stands in for the original, which copy must use.
            let content = self.original_message(&msg.id).map_or_else(
                || msg.content.to_string(),
                |original| original.content.to_string(),
            );
            sections.push(ContextMenuState::open_message(
                x,
                y,
                crate::components::overlays::MessageMenuTarget {
                    message_id: msg.id.to_string(),
                    content,
                    sender: msg.sender.to_string(),
                    display_name: msg.display_name.to_string(),
                    timestamp: msg.timestamp,
                    channel: self.channel_name.clone(),
                    source_split_id: Some(self.source_split_id),
                    allow_remote_actions: msg.allows_remote_message_actions() && !is_whisper,
                    is_whisper,
                    highlight_type: msg.flags.highlight_type.clone(),
                },
            ));
        }

        if !sections.is_empty() {
            show_menu(ContextMenuState::compose(sections));
        }
    }

    /// Handle mouse move for selection and hover
    pub(super) fn on_mouse_move(
        &mut self,
        window_pos: Point<Pixels>,
        left_button_down: bool,
        owner_window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        let local_pos = self.to_local_pos(window_pos);
        let mut needs_update = false;
        if self.interaction_state.selection.is_dragging && !left_button_down {
            self.interaction_state.pending_text_click = None;
            needs_update |= self.interaction_state.selection.end_drag();
        }

        if self
            .interaction_state
            .pending_text_click
            .as_ref()
            .is_some_and(|pending| {
                let x: f32 = window_pos.x.into();
                let y: f32 = window_pos.y.into();
                let delta_x = x - pending.x;
                let delta_y = y - pending.y;
                delta_x.mul_add(delta_x, delta_y * delta_y) > CLICK_DRAG_TOLERANCE_SQUARED
            })
        {
            self.interaction_state.pending_text_click = None;
        }

        let (new_cursor, hitbox_kind) = if self.interaction_state.selection.is_dragging {
            (CursorStyle::IBeam, None)
        } else {
            let hitbox = self.hit_test_hitbox_local(local_pos);
            let mut cursor = cursor_for_hitbox(hitbox);
            if cursor == CursorStyle::IBeam && self.collapsed_deleted_row_at(local_pos).is_some() {
                cursor = CursorStyle::PointingHand;
            }
            let kind = hitbox.map(|h| h.kind.clone());
            (cursor, kind)
        };

        if self.interaction_state.cursor_style != new_cursor {
            self.interaction_state.cursor_style = new_cursor;
            needs_update = true;
        }

        if self.interaction_state.selection.is_dragging {
            needs_update |= self.extend_drag_selection_to(local_pos);
        }

        let tooltip_changed =
            self.interaction_state
                .tooltip
                .update(hitbox_kind, owner_window, window_pos);

        if tooltip_changed {
            self.wake_state.bump_hover_generation();
            self.schedule_hover_link_metadata(cx);
            needs_update = true;
        }

        if self.sync_global_tooltip_overlay_from_state() {
            needs_update = true;
        }

        if needs_update {
            const MOUSE_MOVE_NOTIFY_INTERVAL: std::time::Duration =
                std::time::Duration::from_millis(16);
            let since_notify = self.interaction_state.last_mouse_move_notify.elapsed();
            if !tooltip_changed && since_notify < MOUSE_MOVE_NOTIFY_INTERVAL {
                // Trailing edge: the last throttled move must still reach the
                // screen when the pointer stops inside the window.
                if !self.interaction_state.mouse_move_trailing_notify_pending {
                    self.interaction_state.mouse_move_trailing_notify_pending = true;
                    let delay = MOUSE_MOVE_NOTIFY_INTERVAL - since_notify;
                    cx.spawn(async move |this, cx| {
                        cx.background_executor().timer(delay).await;
                        let _ = this.update(cx, |this, cx| {
                            this.interaction_state.mouse_move_trailing_notify_pending = false;
                            this.interaction_state.last_mouse_move_notify =
                                std::time::Instant::now();
                            crate::backend::metrics::record_ui_notify_source(
                                "canvas.mouse_move_trailing",
                            );
                            cx.notify();
                        });
                    })
                    .detach();
                }
                return;
            }
            self.interaction_state.last_mouse_move_notify = std::time::Instant::now();
            crate::backend::metrics::record_ui_notify_source("canvas.mouse_move");
            cx.notify();
        }
    }

    fn extend_drag_selection_to(&mut self, local_pos: Point<Pixels>) -> bool {
        let Some((msg_id, msg_idx, char_idx)) = self.hit_test_local(local_pos) else {
            return false;
        };
        match self.granular_drag_endpoints(&msg_id, msg_idx, char_idx) {
            Some((anchor, focus)) => self
                .interaction_state
                .selection
                .set_endpoints(anchor, focus),
            None => self
                .interaction_state
                .selection
                .extend_selection(SelectionPoint {
                    message_id: msg_id,
                    message_index: msg_idx,
                    char_index: char_idx,
                }),
        }
    }

    /// Keep a selection drag moving through history while the pointer rests
    /// near or beyond the top/bottom edge. Moves outside the canvas never
    /// reach its listeners, so the loop samples the window's pointer position
    /// itself until the drag ends.
    fn ensure_selection_edge_scroll(
        &mut self,
        owner_window: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) {
        if self.wake_state.selection_edge_scroll_running {
            return;
        }
        self.wake_state.selection_edge_scroll_running = true;
        self.wake_state.selection_edge_scroll_generation = self
            .wake_state
            .selection_edge_scroll_generation
            .wrapping_add(1);
        let generation = self.wake_state.selection_edge_scroll_generation;
        cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(SELECTION_EDGE_SCROLL_TICK)
                    .await;
                let pointer = cx
                    .update_window(owner_window, |_, window, _| window.mouse_position())
                    .ok();
                let keep_running = this
                    .update(cx, |this, cx| {
                        if this.wake_state.selection_edge_scroll_generation != generation {
                            return false;
                        }
                        let Some(pointer) =
                            pointer.filter(|_| this.interaction_state.selection.is_dragging)
                        else {
                            this.wake_state.selection_edge_scroll_running = false;
                            return false;
                        };
                        if this.step_selection_edge_scroll(pointer) {
                            crate::backend::metrics::record_ui_notify_source(
                                "canvas.selection_edge_scroll",
                            );
                            cx.notify();
                        }
                        true
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        })
        .detach();
    }

    fn step_selection_edge_scroll(&mut self, window_pos: Point<Pixels>) -> bool {
        let local_pos = self.to_local_pos(window_pos);
        let local_y: f32 = local_pos.y.into();
        let height = self.viewport_height;
        if height <= SELECTION_EDGE_SCROLL_BAND * 2.0 {
            return false;
        }
        let overshoot = if local_y < SELECTION_EDGE_SCROLL_BAND {
            local_y - SELECTION_EDGE_SCROLL_BAND
        } else if local_y > height - SELECTION_EDGE_SCROLL_BAND {
            local_y - (height - SELECTION_EDGE_SCROLL_BAND)
        } else {
            return false;
        };
        // Speed grows with how far past the band's inner edge the pointer is.
        let delta = (overshoot * 0.4).clamp(-48.0, 48.0);
        let delta = if delta.abs() < 2.0 {
            2.0_f32.copysign(delta)
        } else {
            delta
        };
        let offset = self.scroll_controller.offset;
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);
        let target = (offset + delta).clamp(0.0, max_scroll);
        if (target - offset).abs() < 0.5 {
            return false;
        }
        self.scroll_to_offset(target, false);
        let edge_pos = point(local_pos.x, px(local_y.clamp(0.0, height - 1.0)));
        self.extend_drag_selection_to(edge_pos);
        true
    }

    /// Pointer left the chat viewport (including leaving the application
    /// window). No further `on_mouse_move` arrives to clear the tooltip, so
    /// hide it here instead of leaving the global overlay latched.
    pub(super) fn on_pointer_left_viewport(&mut self) {
        self.interaction_state.tooltip.hide();
        self.wake_state.bump_hover_generation();
        self.sync_global_tooltip_overlay_from_state();
    }

    /// Re-hit-test a still pointer after the content under it moved. The
    /// cursor follows the new target, but a tooltip is only hidden when its
    /// target moved away; a new one waits for a real mouse move so scrolling
    /// and tail-follow never strobe tooltips. Returns true when a visible
    /// tooltip was hidden and the overlay needs another frame.
    pub(super) fn refresh_hover_after_content_move(&mut self, window: &Window) -> bool {
        if !self.scroll_controller.state.mouse_hovering_viewport
            || self.interaction_state.selection.is_dragging
            || (self.interaction_state.tooltip.hovered.is_none()
                && self.interaction_state.cursor_style == CursorStyle::IBeam)
        {
            return false;
        }

        let local_pos = self.to_local_pos(window.mouse_position());
        let hitbox = self.hit_test_hitbox_local(local_pos);
        let cursor = cursor_for_hitbox(hitbox);
        let tooltip = &self.interaction_state.tooltip;
        let target_kept =
            tooltip.hovered.is_none() || tooltip.is_hovering(hitbox.map(|hitbox| &hitbox.kind));
        self.interaction_state.cursor_style = cursor;
        if target_kept {
            return false;
        }
        let hid_tooltip = self.interaction_state.tooltip.hide();
        self.wake_state.bump_hover_generation();
        self.sync_global_tooltip_overlay_from_state();
        hid_tooltip
    }

    /// Handle mouse up
    pub(super) fn on_mouse_up(
        &mut self,
        _window_pos: Point<Pixels>,
        button: MouseButton,
        cx: &mut Context<Self>,
    ) {
        if button == MouseButton::Left {
            let press_started_selection = self.interaction_state.selection.end_drag();
            let mut needs_notify = press_started_selection;
            if let Some(pending) = self.interaction_state.pending_text_click.take() {
                if press_started_selection {
                    needs_notify |= self.interaction_state.selection.has_selection();
                    self.interaction_state.selection.clear();
                }
                self.activate_pending_text_click(pending, cx);
            }
            if needs_notify {
                crate::backend::metrics::record_ui_notify_source("canvas.mouse_up");
                cx.notify();
            }
        }
    }

    /// Middle press on a link or username stages the same completed-click
    /// activation as a left click.
    pub(super) fn on_middle_mouse_down(&mut self, window_pos: Point<Pixels>) {
        let local_pos = self.to_local_pos(window_pos);
        self.interaction_state.pending_middle_click = self
            .pending_text_click_at(local_pos, window_pos)
            .filter(|pending| {
                matches!(
                    pending.kind,
                    HitboxKind::Url { .. }
                        | HitboxKind::EmbedPreview { .. }
                        | HitboxKind::Username { .. }
                        | HitboxKind::Mention { .. }
                )
            });
    }

    pub(super) fn on_middle_mouse_up(&mut self, window_pos: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(pending) = self.interaction_state.pending_middle_click.take() else {
            return;
        };
        let x: f32 = window_pos.x.into();
        let y: f32 = window_pos.y.into();
        let (delta_x, delta_y) = (x - pending.x, y - pending.y);
        if delta_x.mul_add(delta_x, delta_y * delta_y) <= CLICK_DRAG_TOLERANCE_SQUARED {
            self.activate_pending_text_click(pending, cx);
        }
    }

    /// Cancel a selection that started here but released outside this canvas.
    ///
    /// An outside release must not activate a staged text-like click: the
    /// pointer may have finished over another surface or beyond the viewport.
    pub(super) fn on_mouse_up_out(&mut self, button: MouseButton, cx: &mut Context<Self>) {
        if button == MouseButton::Middle {
            self.interaction_state.pending_middle_click = None;
        }
        if button == MouseButton::Left {
            self.interaction_state.pending_text_click = None;
            let ended_drag = self.interaction_state.selection.end_drag();
            if ended_drag {
                crate::backend::metrics::record_ui_notify_source("canvas.mouse_up_out");
                cx.notify();
            }
        }
    }

    fn activate_pending_text_click(&mut self, pending: PendingTextClick, cx: &mut Context<Self>) {
        match pending.kind {
            HitboxKind::Username {
                login,
                display_name,
                user_id,
                ..
            } => {
                let (color_hex, badges) = self
                    .get_message(&pending.message_id)
                    .map(|message| (message.color.clone(), message.badges.clone()))
                    .unwrap_or((None, SmallVec::new()));
                crate::backend::push_ui_action(crate::backend::UiAction::ShowUserCard {
                    username: login.to_string(),
                    display_name: display_name.to_string(),
                    user_id: user_id.as_ref().map(ToString::to_string),
                    channel: self.channel_name.clone(),
                    color: color_hex.map(|color| color.to_string()),
                    badges: badges.iter().map(ToString::to_string).collect(),
                    x: pending.x,
                    y: pending.y,
                });
            }
            HitboxKind::Mention { username } => {
                crate::backend::push_ui_action(crate::backend::UiAction::ShowUserCard {
                    username: username.to_string(),
                    display_name: username.to_string(),
                    user_id: None,
                    channel: self.channel_name.clone(),
                    color: None,
                    badges: vec![],
                    x: pending.x,
                    y: pending.y,
                });
            }
            HitboxKind::Url { url, .. } | HitboxKind::EmbedPreview { url, .. } => {
                queue_confirm_open_link(url.to_string());
            }
            HitboxKind::MediaInspector { source_url, .. } => {
                crate::backend::push_ui_action(crate::backend::UiAction::OpenMediaInspector {
                    source_url: source_url.to_string(),
                });
            }
            HitboxKind::ReplyIndicator { parent_msg_id, .. } => {
                if !self.scroll_to_message(parent_msg_id.as_ref(), cx) {
                    crate::backend::push_ui_action(crate::backend::UiAction::ViewThread {
                        message_id: parent_msg_id.to_string(),
                        channel: self.channel_name.clone(),
                        x: pending.x,
                        y: pending.y,
                    });
                }
            }
            HitboxKind::AutomodAction { allow } => {
                let held_id = crate::backend::irc::automod_held_message_id(&pending.message_id);
                crate::backend::manage_held_automod_message_async(held_id.to_string(), allow, 0);
            }
            HitboxKind::ModAction { label, command } => {
                self.run_mod_button(&pending.message_id, &label, &command);
            }
            HitboxKind::MessageBackground { message_id } => {
                self.expand_collapsed_row(&message_id, cx);
            }
            _ => {}
        }
    }

    fn expand_collapsed_row(&mut self, message_id: &str, cx: &mut Context<Self>) {
        if self.reveal_deleted_message(message_id) {
            crate::backend::metrics::record_ui_notify_source("canvas.reveal_deleted");
            cx.notify();
        }
    }

    /// Run an inline moderation button against the row it sits on, after the
    /// same permission recheck the context-menu moderation items use.
    fn run_mod_button(&self, message_id: &str, label: &str, command: &str) {
        let Some(msg) = self.get_message(message_id) else {
            return;
        };
        let channel = match msg.channel.trim_start_matches('#') {
            "" => self.channel_name.clone(),
            channel => channel.to_ascii_lowercase(),
        };
        let username = msg.sender.to_string();
        if !crate::components::common::moderation_policy::recheck_moderation_target(
            &channel, &username,
        ) {
            return;
        }
        let button = crate::backend::mod_buttons::ModButtonConfig {
            label: label.to_string(),
            command: command.to_string(),
            icon: None,
            is_destructive: false,
        };
        let resolved = button.resolve_with_context(crate::backend::mod_buttons::ModButtonContext {
            username: &username,
            user_display_name: &msg.display_name,
            user_id: msg.user_id.as_deref().unwrap_or_default(),
            message_id: &msg.id,
            message_text: &msg.content,
            channel: &channel,
        });
        crate::backend::send_message_or_command_async(channel, resolved);
    }

    /// Handle key down for copy
    pub(super) fn on_key_down(&mut self, event: &KeyDownEvent, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.interaction_state.selection.has_selection() {
            self.interaction_state.selection.clear();
            self.interaction_state.granular_anchor = None;
            crate::backend::metrics::record_ui_notify_source("canvas.selection_escape");
            cx.notify();
            return;
        }

        let is_copy = (event.keystroke.modifiers.platform || event.keystroke.modifiers.control)
            && (event.keystroke.key == "c" || event.keystroke.key == "C");

        if is_copy && self.interaction_state.selection.has_selection() {
            self.interaction_state
                .selection
                .copy_to_clipboard_with(&self.message_model.message_order, |message_id| {
                    self.selection_layout(message_id)
                });
        }
    }

    /// Handle a wheel scroll of `scroll_amount` pixels (positive scrolls up).
    pub(super) fn on_scroll(&mut self, scroll_amount: f32, cx: &mut Context<Self>) {
        frame_rate_controller::record_interaction();

        let old_offset = self.scroll_controller.offset;
        let max_scroll = (self.message_model.total_height - self.viewport_height).max(0.0);

        self.scroll_controller.state.prev_scroll_offset = self.scroll_controller.offset;

        let input_delta = -scroll_amount;
        let pending_delta =
            self.scroll_controller.state.scroll_desired - self.scroll_controller.offset;
        let reverses_pending_motion = self.scroll_controller.state.scroll_animating
            && input_delta.abs() > f32::EPSILON
            && pending_delta.abs() > f32::EPSILON
            && input_delta.signum() != pending_delta.signum();
        let base_position =
            if self.scroll_controller.state.scroll_animating && !reverses_pending_motion {
                self.scroll_controller.state.scroll_desired
            } else {
                self.scroll_controller.offset
            };
        let target = (base_position - scroll_amount).clamp(0.0, max_scroll);

        self.start_scroll_animation(target);
        if *SCROLL_SMOKE_TRACE_ENABLED || *RENDER_TRACE_ENABLED {
            crate::log_perf_render!(
                debug,
                channel_id = %self.channel_id,
                scroll_amount,
                old_offset,
                target,
                max_scroll,
                total_height = self.message_model.total_height,
                viewport_height = self.viewport_height,
                scroll_animating = self.scroll_controller.state.scroll_animating,
                "canvas.scroll_by_pixels"
            );
        }

        self.apply_manual_scroll_target_state(target);

        self.record_user_scroll();

        self.update_acceleration_signal();

        let current_direction = if target < base_position {
            ScrollDirection::Up
        } else if target > base_position {
            ScrollDirection::Down
        } else {
            ScrollDirection::Static
        };
        if matches!(current_direction, ScrollDirection::Up)
            && self.preload_state.last_burst_direction != Some(ScrollDirection::Up)
        {
            self.preload_state.last_burst_direction = Some(ScrollDirection::Up);
            let (start_idx, _end_idx) = self.get_visible_range();
            let burst_start = start_idx.saturating_sub(100);
            let burst_end = start_idx;
            if burst_start < burst_end {
                self.preload_emotes_for_visible_range(burst_start, burst_end);
            }
        } else if !matches!(current_direction, ScrollDirection::Up) {
            self.preload_state.last_burst_direction = None;
        }

        if (self.scroll_controller.offset - old_offset).abs() > 0.5
            || self.scroll_controller.state.scroll_animating
        {
            crate::backend::metrics::record_ui_notify_source("canvas.scroll_wheel");
            cx.notify();
        }
    }
}

/// Pixels per wheel line are `line_height * WHEEL_LINE_SCROLL_MULTIPLIER` times
/// the user's mouse scroll multiplier. The OS wheel-lines setting is already
/// folded into line deltas; precision pixel deltas pass through 1:1.
pub(super) const WHEEL_LINE_SCROLL_MULTIPLIER: f32 = 3.0;

pub(super) fn wheel_scroll_amount(
    delta: ScrollDelta,
    line_height: f32,
    line_multiplier: f32,
) -> f32 {
    match delta {
        ScrollDelta::Lines(lines) => {
            lines.y * line_height * WHEEL_LINE_SCROLL_MULTIPLIER * line_multiplier
        }
        ScrollDelta::Pixels(pixels) => pixels.y.into(),
    }
}

fn cursor_for_hitbox(hitbox: Option<&hit_testing::ManagedHitbox>) -> CursorStyle {
    match hitbox.map(|h| &h.kind) {
        Some(HitboxKind::Url { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::Emote { .. }) => CursorStyle::IBeam,
        Some(HitboxKind::LayeredEmote { .. }) => CursorStyle::IBeam,
        Some(HitboxKind::Badge { .. }) => CursorStyle::Arrow,
        Some(HitboxKind::Username { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::Mention { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::MessageBackground { .. }) => CursorStyle::IBeam,
        Some(HitboxKind::EmbedPreview { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::MediaInspector { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::ReplyIndicator { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::AutomodAction { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::ModAction { .. }) => CursorStyle::PointingHand,
        Some(HitboxKind::Emoji { .. }) => CursorStyle::IBeam,
        None => CursorStyle::IBeam,
    }
}

fn queue_confirm_open_link(url: String) {
    crate::backend::push_ui_action(crate::backend::UiAction::ConfirmOpenLink {
        url,
        mode: crate::backend::ui_actions::LinkOpenMode::Configured,
    });
}
