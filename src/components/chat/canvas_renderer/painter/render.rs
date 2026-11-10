//! CanvasMessageList GPUI render implementation.

use super::*;

impl Render for CanvasMessageList {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let canvas_cpu_start = std::time::Instant::now();
        let metrics_enabled = crate::backend::metrics::is_enabled();
        let render_log_enabled = crate::log::enabled("perf_render");
        let render_timing_enabled = metrics_enabled || render_log_enabled;
        crate::diagnostics::alloc_profile::reset_frame_counters();
        let mut render_trace = CanvasRenderTraceGuard::enter(
            &self.channel_id,
            self.message_model.messages.len(),
            self.container_width,
            self.viewport_height,
        );

        crate::managers::dynamic_tick_scheduler::sync_animation_cap_from_window(window);
        self.ensure_window_visibility_observer(window, cx);
        let current_window_scale = window.scale_factor();
        crate::components::chat::canvas_renderer::layout_scale::set_current(current_window_scale);
        self.layout_scale_factor = current_window_scale;
        self.sync_deleted_display();
        self.mod_buttons_active =
            self.mod_mode && crate::backend::irc::can_moderate_channel(&self.channel_name);
        crate::components::chat::canvas_renderer::layout_mode::set_mod_buttons(
            self.mod_buttons_active,
        );
        if crate::theme::colors::main_window_frame_active() {
            crate::backend::settings::set_window_scale_factor(current_window_scale);
        }
        self.reset_render_perf_frame_stats();

        if metrics_enabled {
            self.split_render_flush_counter = self.split_render_flush_counter.saturating_add(1);
            if self.split_render_flush_counter >= 30 {
                crate::backend::metrics::record_split_renders(
                    &self.channel_id,
                    self.split_render_flush_counter as u64,
                );
                self.split_render_flush_counter = 0;
            }
        }

        let mut needs_notify = false;

        if self.interaction_state.selection.is_dragging && !window.is_window_active() {
            self.end_lost_selection_drag();
        }

        if self.update_scroll_animation() && !self.wake_state.animation_waker_running {
            crate::backend::metrics::record_ui_notify_source("canvas.scroll_animation");
            if *RENDER_TRACE_ENABLED {
                crate::log_perf_render!(
                    debug,
                    "canvas.render.notify source=scroll_animation channel={}",
                    self.channel_id
                );
            }
            cx.notify();
        }

        self.clamp_scroll_offset_to_content();

        if !self.message_model.messages.is_empty()
            && global_layout_cache::get_invalidation_version()
                == self.layout_store.runtime.last_invalidation_version
            && global_layout_cache::layout_eviction_seq()
                != self.layout_store.runtime.last_layout_eviction_seq
        {
            self.apply_layout_evictions();
        }

        let pre_layout_range = self.get_visible_range();
        let pre_layout_scroll_offset = self.scroll_controller.offset;
        let pre_layout_quantized_width = self.layout_quantized_width_for_cache();
        let cached_visible_render_range_before_frame =
            self.viewport_state.cached_visible_render_range;
        let animation_only_frame_pending =
            std::mem::take(&mut self.wake_state.animation_only_frame_pending);
        let stable_visible_row_fast_path = self.can_reuse_visible_rows_for_stable_frame(
            pre_layout_range.0,
            pre_layout_range.1,
            pre_layout_scroll_offset,
            pre_layout_quantized_width,
        );
        if !stable_visible_row_fast_path && render_timing_enabled {
            self.render_perf_frame_stats.stable_gate_failures = self.stable_frame_gate_failures(
                pre_layout_range.0,
                pre_layout_range.1,
                pre_layout_scroll_offset,
                pre_layout_quantized_width,
            );
        }
        let scroll_visible_row_fast_path = !stable_visible_row_fast_path
            && self.can_translate_visible_rows_for_scroll_frame(
                pre_layout_range.0,
                pre_layout_range.1,
                pre_layout_quantized_width,
            );
        let visible_row_reuse_fast_path =
            stable_visible_row_fast_path || scroll_visible_row_fast_path;
        let viewport_paint_snapshot_reuse_fast_path = stable_visible_row_fast_path
            || (scroll_visible_row_fast_path
                && cached_visible_render_range_before_frame == Some(pre_layout_range));
        let animation_visible_row_fast_path = stable_visible_row_fast_path
            && animation_only_frame_pending
            && self.wake_state.has_visible_animations_last_frame;

        let layout_time = if visible_row_reuse_fast_path {
            if animation_visible_row_fast_path {
                crate::backend::metrics::record_emote_event("canvas.animation_visible_rows_reused");
                self.render_perf_frame_stats.animation_visible_row_reuse += 1;
            } else if scroll_visible_row_fast_path {
                crate::backend::metrics::record_emote_event(
                    "canvas.scroll_visible_rows_translated",
                );
                self.render_perf_frame_stats
                    .viewport_canvas_scroll_snapshot_reuse += 1;
            } else {
                crate::backend::metrics::record_emote_event("canvas.stable_visible_rows_reused");
                self.render_perf_frame_stats.stable_visible_row_reuse += 1;
            }
            std::time::Duration::ZERO
        } else {
            let layout_start = RenderTimer::start(render_timing_enabled);
            self.update_layouts(window);
            let layout_time = layout_start.elapsed();
            crate::backend::metrics::record_emote_stage(
                "canvas.layout",
                layout_time,
                self.message_model.messages.len(),
            );

            layout_time
        };

        crate::diagnostics::fps_profiler::record_phase("layout", layout_time);

        crate::diagnostics::fps_profiler::record_frame_classification(
            if animation_visible_row_fast_path {
                crate::diagnostics::fps_profiler::FrameClass::AnimationOnly
            } else if scroll_visible_row_fast_path {
                crate::diagnostics::fps_profiler::FrameClass::ScrollTranslate
            } else if stable_visible_row_fast_path {
                crate::diagnostics::fps_profiler::FrameClass::StableReuse
            } else {
                crate::diagnostics::fps_profiler::FrameClass::FullRebuild
            },
        );

        let animation_census_owner = cx.entity_id().as_u64();
        self.wake_state.animation_census_owner = Some(animation_census_owner);
        emote_renderer::begin_split_frame(animation_census_owner);

        if !visible_row_reuse_fast_path {
            self.clamp_scroll_offset_to_content();
        }

        let (start_idx, end_idx) = if visible_row_reuse_fast_path {
            pre_layout_range
        } else {
            self.get_visible_range()
        };

        let elem_build_start = RenderTimer::start(render_timing_enabled);

        if !visible_row_reuse_fast_path {
            let embed_preload_start = RenderTimer::start(metrics_enabled);
            self.fetch_embeds_for_visible_range(start_idx, end_idx);
            crate::backend::metrics::record_emote_stage(
                "canvas.embed_scan",
                embed_preload_start.elapsed(),
                end_idx.saturating_sub(start_idx),
            );

            let emote_preload_start = RenderTimer::start(metrics_enabled);
            self.preload_emotes_for_visible_range(start_idx, end_idx);
            crate::backend::metrics::record_emote_stage(
                "canvas.emote_scan",
                emote_preload_start.elapsed(),
                end_idx.saturating_sub(start_idx),
            );
        }

        static RENDER_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        if crate::log::enabled("ui")
            && RENDER_COUNTER
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .is_multiple_of(300)
        {
            crate::log_ui!(trace,
                channel_id = %self.channel_id,
                msg_count = self.message_model.messages.len(),
                visible_range = ?format!("{}..{}", start_idx, end_idx),
                "Render frame"
            );
        }

        let container_width = self.container_width;
        let scroll_offset = self.scroll_controller.offset;

        let list_id = self.cached_element_id.clone();

        let visible_scan_start = RenderTimer::start(crate::diagnostics::fps_profiler::is_enabled());
        let visible_rows_result = self.update_visible_rows_for_render_frame(
            start_idx,
            end_idx,
            scroll_offset,
            stable_visible_row_fast_path,
            scroll_visible_row_fast_path,
            window,
        );
        crate::diagnostics::fps_profiler::record_phase(
            "visible_scan",
            visible_scan_start.elapsed(),
        );
        needs_notify |= visible_rows_result.needs_notify;

        let hitbox_start = RenderTimer::start(metrics_enabled);
        let hitboxes_moved = !stable_visible_row_fast_path
            && !self.interaction_state.hitbox_manager.is_cache_valid(
                start_idx,
                end_idx,
                scroll_offset,
            );
        if hitboxes_moved {
            if self
                .interaction_state
                .hitbox_manager
                .translate_cached_range_to_scroll(start_idx, end_idx, scroll_offset)
            {
                crate::backend::metrics::record_emote_stage(
                    "canvas.hitbox_translate",
                    hitbox_start.elapsed(),
                    self.interaction_state.hitbox_manager.hitboxes.len(),
                );
            } else {
                self.interaction_state.hitbox_manager.clear();
                for row in self.viewport_state.visible_render_rows.iter() {
                    for region in row.layout.hitbox_regions.iter() {
                        self.interaction_state.hitbox_manager.add(
                            region,
                            row.layout.message_id_shared.as_ref(),
                            row.viewport_y,
                        );
                    }
                }
                self.interaction_state.hitbox_manager.validate_cache(
                    start_idx,
                    end_idx,
                    scroll_offset,
                );
                crate::backend::metrics::record_emote_stage(
                    "canvas.hitbox_rebuild",
                    hitbox_start.elapsed(),
                    self.interaction_state.hitbox_manager.hitboxes.len(),
                );
            }
        }

        if hitboxes_moved {
            needs_notify |= self.refresh_hover_after_content_move(window);
        }

        self.viewport_state.render_element_buffer.clear();
        let visible_row_count = self.viewport_state.visible_render_rows.len();
        if self.viewport_state.render_element_buffer.capacity() < visible_row_count {
            self.viewport_state
                .render_element_buffer
                .reserve(visible_row_count - self.viewport_state.render_element_buffer.capacity());
        }

        let paint_prep_start = RenderTimer::start(render_timing_enabled);
        let frame_alternate_bg = crate::backend::settings::get_alternate_message_backgrounds()
            .then(colors::bg_alternate_row);
        let mut fallback_rows_animate = false;
        let animation_slot_owner = cx.entity_id().as_u64();
        {
            // This render's elements repaint every slot; cached reuse keeps them.
            let mut slots = self.viewport_state.painted_animation_slots.borrow_mut();
            slots.slots.clear();
            slots.unslotted = 0;
        }
        let viewport_canvas_element = if visible_row_count > 0 {
            let selection_or_search_active = self.interaction_state.selection.has_selection()
                || self.interaction_state.selection.is_dragging
                || self.search_focus_message_id.is_some();
            let paint_rows_scale_matches =
                (self.viewport_state.cached_viewport_paint_rows_scale - current_window_scale).abs()
                    < 1e-4;
            let cached_paint_rows = (viewport_paint_snapshot_reuse_fast_path
                && !selection_or_search_active
                && paint_rows_scale_matches)
                .then(|| {
                    self.viewport_state
                        .cached_viewport_paint_rows
                        .as_ref()
                        .filter(|rows| !rows.is_empty())
                        .map(Arc::clone)
                })
                .flatten();

            let mut snapshot_row_paint_paths = RowPaintPathTally::default();
            let snapshot_reuse_matches = if let Some(cached_rows) = cached_paint_rows.as_ref() {
                let mut membership =
                    echo_render_core::snapshot_membership::OrderedSnapshotMembership::new(
                        cached_rows.iter().map(|cached| {
                            (cached.message_index, Arc::as_ptr(&cached.layout) as usize)
                        }),
                    );
                for row in self.viewport_state.visible_render_rows.iter() {
                    let element_render_plan = ElementRenderPlan::for_layout(&row.layout);
                    snapshot_row_paint_paths
                        .record(element_render_plan.fallback_reasons(row.is_deleted));
                    if can_paint_visible_row_in_viewport_canvas(
                        &element_render_plan,
                        row.is_deleted,
                    ) {
                        membership.observe_direct_row(
                            row.message_index,
                            Arc::as_ptr(&row.layout) as usize,
                        );
                    } else {
                        fallback_rows_animate |= element_render_plan.has_loading_animation();
                        self.viewport_state.render_element_buffer.push(
                            render_message_at_position(
                                &FallbackRowFrame {
                                    selection: &self.interaction_state.selection,
                                    container_width,
                                    alternate_bg: frame_alternate_bg,
                                    animation_slots: &self.viewport_state.painted_animation_slots,
                                    slot_owner: animation_slot_owner,
                                },
                                row,
                                &element_render_plan,
                                false,
                            )
                            .into_any_element(),
                        );
                    }
                }
                membership.finish()
            } else {
                false
            };

            if snapshot_reuse_matches {
                self.render_perf_frame_stats.row_paint_paths = snapshot_row_paint_paths;
            } else {
                self.viewport_state.render_element_buffer.clear();
            }

            if snapshot_reuse_matches {
                self.render_perf_frame_stats.viewport_canvas_fast_path += 1;
                self.render_perf_frame_stats
                    .viewport_canvas_prepared_snapshot_reuse += 1;
                let paint_scroll_delta =
                    self.viewport_state.cached_viewport_paint_scroll_offset - scroll_offset;
                cached_paint_rows.map(|rows| {
                    render_visible_rows_viewport_element(
                        rows,
                        container_width,
                        paint_scroll_delta,
                        Rc::clone(&self.viewport_state.painted_animation_slots),
                        animation_slot_owner,
                    )
                    .into_any_element()
                })
            } else {
                drop(cached_paint_rows);
                let search_focus_message_id = self.search_focus_message_id.as_ref();
                let mut paint_rows =
                    std::mem::take(&mut self.viewport_state.spare_viewport_paint_rows);
                paint_rows.clear();
                if paint_rows.capacity() < visible_row_count {
                    paint_rows.reserve(visible_row_count - paint_rows.capacity());
                }
                let mut reused_paint_rows = 0u32;

                {
                    let reusable_viewport_paint_rows =
                        if selection_or_search_active || !paint_rows_scale_matches {
                            None
                        } else {
                            self.viewport_state.cached_viewport_paint_rows.as_deref()
                        };

                    for row in self.viewport_state.visible_render_rows.iter() {
                        let element_render_plan = ElementRenderPlan::for_layout(&row.layout);
                        self.render_perf_frame_stats
                            .row_paint_paths
                            .record(element_render_plan.fallback_reasons(row.is_deleted));
                        let is_search_focus = search_focus_message_id
                            .map(|id| id.as_ref() == row.layout.message_id_shared.as_ref())
                            .unwrap_or(false);

                        if can_paint_visible_row_in_viewport_canvas(
                            &element_render_plan,
                            row.is_deleted,
                        ) {
                            let mut reused_current_paint_row = false;
                            let paint_row =
                                match reusable_viewport_paint_rows.and_then(|cached_rows| {
                                    reuse_prepared_viewport_paint_row(cached_rows, row)
                                }) {
                                    Some(paint_row) => {
                                        reused_paint_rows = reused_paint_rows.saturating_add(1);
                                        reused_current_paint_row = true;
                                        paint_row
                                    }
                                    _ => prepare_viewport_paint_row(
                                        row,
                                        self.interaction_state.selection.get_ranges_for_message(
                                            row.message_index,
                                            row.layout.source_char_len(),
                                        ),
                                        is_search_focus,
                                        window,
                                    ),
                                };
                            if !reused_current_paint_row {
                                self.render_perf_frame_stats
                                    .viewport_canvas_prepared_text_lines +=
                                    paint_row.text_commands.len() as u32;
                                self.render_perf_frame_stats
                                    .viewport_canvas_prepared_static_images += paint_row
                                    .image_commands
                                    .iter()
                                    .filter(|cmd| cmd.static_render_image.is_some())
                                    .count()
                                    as u32;
                            }
                            paint_rows.push(paint_row);
                        } else {
                            fallback_rows_animate |= element_render_plan.has_loading_animation();
                            self.viewport_state.render_element_buffer.push(
                                render_message_at_position(
                                    &FallbackRowFrame {
                                        selection: &self.interaction_state.selection,
                                        container_width,
                                        alternate_bg: frame_alternate_bg,
                                        animation_slots: &self
                                            .viewport_state
                                            .painted_animation_slots,
                                        slot_owner: animation_slot_owner,
                                    },
                                    row,
                                    &element_render_plan,
                                    is_search_focus,
                                )
                                .into_any_element(),
                            );
                        }
                    }
                }

                if let Some(previous_rows) = self.viewport_state.cached_viewport_paint_rows.take()
                    && let Ok(mut previous_rows) = Arc::try_unwrap(previous_rows)
                {
                    previous_rows.clear();
                    if previous_rows.capacity()
                        > self.viewport_state.spare_viewport_paint_rows.capacity()
                    {
                        self.viewport_state.spare_viewport_paint_rows = previous_rows;
                    }
                }

                if paint_rows.is_empty() {
                    self.viewport_state.spare_viewport_paint_rows = paint_rows;
                    None
                } else {
                    self.render_perf_frame_stats
                        .viewport_canvas_prepared_row_reuse += reused_paint_rows;
                    self.render_perf_frame_stats.viewport_canvas_fast_path += 1;
                    #[allow(clippy::arc_with_non_send_sync)]
                    let paint_rows = Arc::new(paint_rows);
                    if !selection_or_search_active {
                        self.viewport_state.cached_viewport_paint_rows =
                            Some(Arc::clone(&paint_rows));
                        self.viewport_state.cached_viewport_paint_scroll_offset = scroll_offset;
                        self.viewport_state.cached_viewport_paint_rows_scale = current_window_scale;
                    }
                    Some(
                        render_visible_rows_viewport_element(
                            paint_rows,
                            container_width,
                            0.0,
                            Rc::clone(&self.viewport_state.painted_animation_slots),
                            animation_slot_owner,
                        )
                        .into_any_element(),
                    )
                }
            }
        } else {
            self.viewport_state.cached_viewport_paint_rows = None;
            None
        };
        self.viewport_state.fallback_rows_animate = fallback_rows_animate;
        let paint_prep_time = paint_prep_start.elapsed();
        crate::diagnostics::fps_profiler::record_phase("paint_prep", paint_prep_time);

        let direct_animation_metadata_start = RenderTimer::start(metrics_enabled);
        let want_signature = !animation_visible_row_fast_path;
        let (direct_animation_metadata_count, merged_animation_frame) =
            self.register_and_signature_visible_direct_paint_animations(want_signature);
        if direct_animation_metadata_count > 0 {
            crate::backend::metrics::record_emote_stage(
                "canvas.direct_anim_metadata",
                direct_animation_metadata_start.elapsed(),
                direct_animation_metadata_count,
            );
        }
        let image_misses = self.visible_image_load_misses();
        self.wake_state.has_visible_image_loads_last_frame = image_misses.pending;
        if let Some(deadline) = image_misses
            .next_failed_retry
            .filter(|deadline| *deadline > std::time::Instant::now())
        {
            self.schedule_failed_image_retry_wake(deadline, cx);
        }

        let visible_count = emote_renderer::owner_visible_count(animation_census_owner);
        emote_renderer::end_split_frame();
        let has_visible_emote_animations = visible_count > 0;
        self.wake_state.has_visible_animations_last_frame = has_visible_emote_animations;
        self.wake_state.last_visible_animation_frame_signature =
            if self.wake_state.has_visible_animations_last_frame {
                if animation_visible_row_fast_path {
                    self.wake_state.last_visible_animation_frame_signature
                } else {
                    merged_animation_frame.map(|frame| frame.signature)
                }
            } else {
                None
            };
        let entity = cx.entity().clone();
        let hover_entity = entity.clone();
        let cursor = self.interaction_state.cursor_style;
        let mut container = div()
            .id(list_id)
            .role(Role::List)
            .aria_label(format!("Chat messages in #{}", self.channel_name))
            .flex_1()
            .w_full()
            .h_full()
            .min_h_0()
            .min_w_0()
            .overflow_hidden()
            .bg(colors::bg_void())
            .relative()
            .cursor(cursor)
            .track_focus(&self.focus_handle)
            .on_hover(move |hovered, window, cx| {
                let hovering = *hovered;
                let modifiers = window.modifiers();
                hover_entity.update(cx, |this, cx| {
                    if this.scroll_controller.state.mouse_hovering_viewport == hovering {
                        return;
                    }

                    this.scroll_controller.state.mouse_hovering_viewport = hovering;
                    let _ = this.set_chat_pause_modifiers(modifiers);
                    if hovering {
                        this.refresh_chat_hover_pause(cx);
                    } else {
                        let _ = this.clear_chat_hover_pause();
                        this.on_pointer_left_viewport();
                    }
                    crate::backend::metrics::record_ui_notify_source("canvas.chat_hover_state");
                    cx.notify();
                });
            })
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(
                        event.position,
                        MouseButton::Left,
                        event.click_count,
                        window,
                        cx,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Right,
                cx.listener(|this, event: &MouseDownEvent, window, cx| {
                    this.on_mouse_down(
                        event.position,
                        MouseButton::Right,
                        event.click_count,
                        window,
                        cx,
                    );
                }),
            )
            .on_mouse_down(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseDownEvent, _window, _cx| {
                    this.on_middle_mouse_down(event.position);
                }),
            )
            .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                let modifiers = window.modifiers();
                let modifier_changed = this.set_chat_pause_modifiers(modifiers);
                this.refresh_chat_hover_pause(cx);
                this.on_mouse_move(event.position, event.dragging(), window.window_handle(), cx);
                if modifier_changed {
                    crate::backend::metrics::record_ui_notify_source(
                        "canvas.chat_pause.mouse_modifier",
                    );
                    cx.notify();
                }
            }))
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up(event.position, MouseButton::Left, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up_out(MouseButton::Left, cx);
                }),
            )
            .on_mouse_up(
                MouseButton::Middle,
                cx.listener(|this, event: &MouseUpEvent, _window, cx| {
                    this.on_middle_mouse_up(event.position, cx);
                }),
            )
            .on_mouse_up_out(
                MouseButton::Middle,
                cx.listener(|this, _event: &MouseUpEvent, _window, cx| {
                    this.on_mouse_up_out(MouseButton::Middle, cx);
                }),
            )
            .on_scroll(cx.listener(|this, event: &ScrollEvent, _window, cx| {
                if event.modifiers.control {
                    const FONT_SCALE_STEP: f32 = 0.05;
                    let delta_y: f32 = event.delta.pixel_delta(px(line_height())).y.into();
                    if delta_y != 0.0 {
                        let step = if delta_y > 0.0 {
                            FONT_SCALE_STEP
                        } else {
                            -FONT_SCALE_STEP
                        };
                        settings::set_font_scale(settings::get_font_scale() + step);
                        cx.notify();
                    }
                    return;
                }
                let scroll_amount = interaction::wheel_scroll_amount(
                    event.delta,
                    line_height(),
                    crate::backend::settings::get_mouse_scroll_multiplier(),
                );
                if *SCROLL_SMOKE_TRACE_ENABLED || *RENDER_TRACE_ENABLED {
                    let precise = matches!(event.delta, ScrollDelta::Pixels(_));
                    crate::log_perf_render!(
                        debug,
                        channel_id = %this.channel_id,
                        scroll_amount,
                        precise,
                        "canvas.scroll_wheel_event"
                    );
                }
                this.on_scroll(scroll_amount, cx);
            }))
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _window, cx| {
                this.on_key_down(event, cx);
            }));

        if let Some(elem) = viewport_canvas_element {
            container = container.child(elem);
        }
        for elem in self.viewport_state.render_element_buffer.drain(..) {
            container = container.child(elem);
        }

        if let Some(boundary) = self.render_new_message_boundary(scroll_offset) {
            container = container.child(boundary);
        }
        if let Some(line) = self.render_last_read_boundary(scroll_offset) {
            container = container.child(line);
        }

        if settings::get_show_reply_button() {
            for row in &self.viewport_state.visible_render_rows {
                if let Some(reply_btn) = self.render_reply_button(
                    row.layout.message_id_shared.as_ref(),
                    scroll_offset,
                    cx,
                ) {
                    container = container.child(reply_btn);
                }
            }
        }

        let has_scrollback = self.message_model.total_height > self.viewport_height;
        if has_scrollback
            && self.should_show_pause_indicator()
            && settings::get_show_pause_indicator()
        {
            container = container.child(render_pause_indicator());
        } else if has_scrollback && !self.is_at_bottom() {
            container = container.child(self.render_jump_to_live(cx));
        }

        container = container.child(
            canvas(
                move |bounds, _window, cx| {
                    entity.update(cx, |this, _| {
                        let bounds_changed = this
                            .interaction_state
                            .element_bounds
                            .map(|old| {
                                (old.origin.x - bounds.origin.x).abs() > px(0.5)
                                    || (old.origin.y - bounds.origin.y).abs() > px(0.5)
                                    || (old.size.width - bounds.size.width).abs() > px(0.5)
                                    || (old.size.height - bounds.size.height).abs() > px(0.5)
                            })
                            .unwrap_or(true);
                        if bounds_changed {
                            this.interaction_state.element_bounds = Some(bounds);
                        }
                    });
                },
                |_bounds, _: (), _window, _cx| {},
            )
            .absolute()
            .inset_0(),
        );

        let elem_build_time = elem_build_start.elapsed();
        crate::backend::metrics::record_emote_stage(
            "canvas.elem_build",
            elem_build_time,
            end_idx.saturating_sub(start_idx),
        );

        if *RENDER_TRACE_ENABLED || crate::diagnostics::fps_profiler::is_enabled() {
            let visible_row_count = self.viewport_state.visible_render_rows.len();
            if visible_row_count > 0 {
                const PER_MESSAGE_PAINT_BUDGET_MS: f64 = 0.1;
                const PER_MESSAGE_LAYOUT_BUDGET_MS: f64 = 2.0;
                let per_message_elem_ms =
                    (paint_prep_time.as_secs_f64() * 1000.0) / visible_row_count as f64;
                let did_layout_this_frame = !visible_row_reuse_fast_path;
                let budget_ms = if did_layout_this_frame {
                    PER_MESSAGE_PAINT_BUDGET_MS + PER_MESSAGE_LAYOUT_BUDGET_MS
                } else {
                    PER_MESSAGE_PAINT_BUDGET_MS
                };
                if per_message_elem_ms > budget_ms {
                    crate::log_perf_render!(
                        warn,
                        channel = %self.channel_id,
                        per_msg_ms = per_message_elem_ms,
                        budget_ms,
                        visible = visible_row_count,
                        layout_frame = did_layout_this_frame,
                        "canvas per-message budget exceeded"
                    );
                }
            }
        }
        static CANVAS_TIMING_FRAME: std::sync::atomic::AtomicU64 =
            std::sync::atomic::AtomicU64::new(0);
        if render_log_enabled
            && CANVAS_TIMING_FRAME
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                .is_multiple_of(120)
        {
            crate::log_perf_render!(
                trace,
                channel = %self.channel_id,
                layout_ms = layout_time.as_millis(),
                elem_ms = elem_build_time.as_millis(),
                msg_count = self.message_model.messages.len(),
                visible = end_idx - start_idx,
                "canvas render timing"
            );
        }

        self.flush_render_perf_aggregate_if_due(
            layout_time,
            elem_build_time,
            self.viewport_state.visible_render_rows.len(),
            end_idx.saturating_sub(start_idx),
        );

        if self.wake_state.has_visible_animations_last_frame {
            let next_change_ms = merged_animation_frame.and_then(|frame| frame.next_change_ms);
            self.reschedule_animation_waker_if_late(next_change_ms);
            self.ensure_animation_waker(next_change_ms, window, cx);
        } else {
            self.release_animation_waker();
        }

        self.wake_state.has_async_layout_in_flight_last_frame =
            self.async_layout_manager.has_in_flight();
        if self.wake_state.has_async_layout_in_flight_last_frame {
            self.ensure_async_layout_waker(cx);
        } else {
            self.release_async_layout_waker();
        }

        if self.wake_state.has_visible_image_loads_last_frame {
            self.ensure_image_load_waker(cx);
        } else {
            self.release_image_load_waker();
        }

        if self
            .layout_store
            .runtime
            .pending_layout_quantized_width
            .is_some()
        {
            self.ensure_width_stabilization_waker(cx);
        } else {
            self.release_width_stabilization_waker();
        }

        needs_notify |= self.resume_stranded_bottom_follow(cx);

        if needs_notify
            && !self.wake_state.animation_waker_running
            && !self.wake_state.async_layout_waker_running
            && !self.wake_state.image_load_waker_running
            && !self.wake_state.width_stabilization_waker_running
        {
            crate::backend::metrics::record_ui_notify_source("canvas.render_state_settled");
            cx.notify();
        }

        render_trace.finish();

        crate::diagnostics::alloc_profile::record_frame_snapshot();

        crate::diagnostics::fps_profiler::record_canvas_cpu_nanos(
            canvas_cpu_start.elapsed().as_nanos() as u64,
        );

        crate::diagnostics::fps_profiler::disable_secondary_window_gpu_timing(window);

        accessibility::AccessibleChat::new(container, cx.entity(), stable_visible_row_fast_path)
    }
}

impl CanvasMessageList {
    /// Overlay the unread boundary without inserting a synthetic message row.
    /// That keeps hitboxes, message storage, and the virtualized render path
    /// unchanged while still placing the divider immediately before the first
    /// newly arrived message whenever it enters the viewport.
    fn render_new_message_boundary(&self, scroll_offset: f32) -> Option<AnyElement> {
        let boundary_id = self.message_state.new_message_boundary_id.as_ref()?;
        let index = self.get_message_index(boundary_id.as_ref())?;
        self.render_boundary_divider(index, scroll_offset, "New messages")
    }

    /// The "last read" line sits below the newest message seen before the tab
    /// was hidden or the window lost focus. It yields to the "New messages"
    /// divider when both mark the same boundary.
    fn render_last_read_boundary(&self, scroll_offset: f32) -> Option<AnyElement> {
        if !settings::get_show_last_read_indicator() {
            return None;
        }
        let last_read_id = self.message_state.last_read_id.as_ref()?;
        let index = self.get_message_index(last_read_id.as_ref())? + 1;
        if index > self.message_model.height_tree.len() {
            return None;
        }
        let new_messages_index = self
            .message_state
            .new_message_boundary_id
            .as_ref()
            .and_then(|id| self.get_message_index(id.as_ref()));
        if new_messages_index == Some(index) {
            return None;
        }
        self.render_boundary_divider(index, scroll_offset, "Last read")
    }

    fn render_boundary_divider(
        &self,
        index: usize,
        scroll_offset: f32,
        label: &'static str,
    ) -> Option<AnyElement> {
        let viewport_y = self.message_model.height_tree.y_position(index) - scroll_offset;
        if !(viewport_y >= -12.0 && viewport_y <= self.viewport_height + 12.0) {
            return None;
        }

        Some(
            div()
                .absolute()
                .left(px(12.0))
                .right(px(12.0))
                .top(px((viewport_y - 9.0).max(0.0)))
                .flex()
                .items_center()
                .text_xs()
                .text_color(colors::accent_text())
                .child(div().h(px(1.0)).flex_1().bg(colors::accent().opacity(0.72)))
                .child(
                    div()
                        .px(px(6.0))
                        .rounded(px(8.0))
                        .bg(colors::bg_void())
                        .border_1()
                        .border_color(colors::accent().opacity(0.72))
                        .child(label),
                )
                .child(div().h(px(1.0)).flex_1().bg(colors::accent().opacity(0.72)))
                .into_any_element(),
        )
    }
}
