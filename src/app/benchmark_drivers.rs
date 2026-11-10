use super::EchoApp;
use crate::backend;
use crate::components::settings::SettingsSectionId;
use crate::config;
use crate::state::{ActivitySection, SplitId};
use gpui::{AnyWindowHandle, AppContext, Context, Task, Window};
use std::time::Duration;

#[derive(Clone)]
struct FeedbackUiRestoreState {
    path: Vec<usize>,
    split_id: SplitId,
    channel: String,
    draft: Option<String>,
    cursor: Option<usize>,
    selection: Option<std::ops::Range<usize>>,
    placeholder_input: Option<crate::managers::input_manager::PlaceholderInput>,
    focused_placeholder: Option<SplitId>,
}

#[derive(Clone, Copy, Default)]
struct UiFixtureTransientCleanup {
    settings_closed: bool,
    overflow_closed: bool,
    tab_hover_cleared: bool,
    activity_closed: bool,
}

impl EchoApp {
    fn spawn_graceful_quit_task(cx: &mut Context<Self>) -> Option<Task<()>> {
        let delay_ms = crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_GRACEFUL_QUIT_AFTER_MS")
            .filter(|value| *value > 0)?;

        Some(cx.spawn(async move |_this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            crate::log_ui!(
                info,
                delay_ms,
                "bench.app_smoke requesting graceful application quit"
            );
            cx.update(|cx| cx.quit());
        }))
    }

    /// Tear down only state which the disposable UI fixture can leave
    /// visually active after an early exit. Persistent preferences and normal
    /// overlay behavior stay outside this benchmark-only cleanup boundary.
    fn cleanup_ui_fixture_transients(
        &mut self,
        cx: &mut Context<Self>,
    ) -> UiFixtureTransientCleanup {
        let settings = self.overlay_manager.read(cx).settings_modal.clone();
        let settings_closed = settings.update(cx, |state, cx| {
            state.benchmark_close_and_cleanup();
            cx.notify();
            !state.benchmark_snapshot().is_open
        });
        let (overflow_closed, tab_hover_cleared) =
            self.tab_bar_renderer.update(cx, |renderer, _| {
                let closed = renderer.benchmark_close_transients();
                (
                    closed && !renderer.is_overflow_open(),
                    closed && renderer.hovered_tab_idx().is_none(),
                )
            });
        let activity_closed = self.app_state.update(cx, |state, cx| {
            state.close_activity(cx);
            !state.activity_panel.open
        });
        crate::components::overlays::clear_global_tooltip();
        cx.notify();
        UiFixtureTransientCleanup {
            settings_closed,
            overflow_closed,
            tab_hover_cleared,
            activity_closed,
        }
    }

    fn restore_feedback_ui_app_state(
        &mut self,
        restore: &FeedbackUiRestoreState,
        cx: &mut Context<Self>,
    ) -> bool {
        let path = restore.path.clone();
        let channel = restore.channel.clone();
        self.app_state.update(cx, |state, cx| {
            let tab = state.active_tab_mut();
            let _ = tab.set_channel_at_path(&path, channel);
            let _ = tab.set_active_split_path(path.clone());
            cx.notify();
        });
        self.split_layout.update(cx, |layout, _| {
            layout.set_active_split(restore.path.clone())
        });
        self.chat_input_renderer.update(cx, |renderer, _| {
            match restore.draft.as_ref() {
                Some(draft) => {
                    renderer
                        .input_manager
                        .input_texts
                        .insert(restore.split_id, draft.clone());
                }
                None => {
                    renderer.input_manager.input_texts.remove(&restore.split_id);
                }
            }
            renderer.input_manager.restore_pointer_state(
                restore.split_id,
                restore.cursor,
                restore.selection.clone(),
            );
            match restore.placeholder_input.as_ref() {
                Some(input) => {
                    renderer
                        .input_manager
                        .placeholder_inputs
                        .insert(restore.split_id, input.clone());
                }
                None => {
                    renderer
                        .input_manager
                        .placeholder_inputs
                        .remove(&restore.split_id);
                }
            }
            renderer.input_manager.focused_placeholder = restore.focused_placeholder;
        });
        let settings = self.overlay_manager.read(cx).settings_modal.clone();
        settings.update(cx, |state, cx| {
            state.benchmark_close_and_cleanup();
            cx.notify();
        });
        cx.notify();

        let channel_restored = self
            .app_state
            .read(cx)
            .active_tab()
            .layout
            .get_channel_at_path(&restore.path)
            .as_deref()
            == Some(restore.channel.as_str());
        let renderer = self.chat_input_renderer.read(cx);
        channel_restored
            && renderer.input_manager.input_texts.get(&restore.split_id) == restore.draft.as_ref()
            && renderer
                .input_manager
                .cursor_positions
                .get(&restore.split_id)
                .copied()
                == restore.cursor
            && renderer
                .input_manager
                .placeholder_inputs
                .get(&restore.split_id)
                .map(|chooser| chooser.input.entity_id())
                == restore
                    .placeholder_input
                    .as_ref()
                    .map(|chooser| chooser.input.entity_id())
            && renderer.input_manager.focused_placeholder == restore.focused_placeholder
    }

    pub(super) fn spawn_scroll_smoke_task(cx: &mut Context<Self>) -> Option<Task<()>> {
        enum ScrollSmokeStep {
            Page(i32),
            Pixels(f32),
        }

        let enabled = crate::benchmark_ui_fixture::env_flag("ECHO_BENCH_SCROLL_SMOKE");
        if !enabled {
            return None;
        }

        let delay_ms = crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_SCROLL_SMOKE_DELAY_MS")
            .unwrap_or(15_000);
        let step_delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_SCROLL_SMOKE_STEP_DELAY_MS")
                .unwrap_or(350);
        let sequence = std::env::var("ECHO_BENCH_SCROLL_SMOKE_SEQUENCE")
            .unwrap_or_else(|_| "up,up,down".to_string());
        let steps: Vec<ScrollSmokeStep> = sequence
            .split(',')
            .filter_map(|step| match step.trim().to_ascii_lowercase().as_str() {
                "up" | "-1" => Some(ScrollSmokeStep::Pixels(120.0)),
                "down" | "1" => Some(ScrollSmokeStep::Pixels(-120.0)),
                "page-up" => Some(ScrollSmokeStep::Page(-1)),
                "page-down" => Some(ScrollSmokeStep::Page(1)),
                value if value.starts_with("px:") => {
                    value[3..].parse::<f32>().ok().map(ScrollSmokeStep::Pixels)
                }
                value if value.starts_with("page:") => value[5..]
                    .parse::<i32>()
                    .ok()
                    .map(|direction| ScrollSmokeStep::Page(direction.signum())),
                _ => None,
            })
            .collect();

        if steps.is_empty() {
            crate::log_ui!(
                warn,
                "ECHO_BENCH_SCROLL_SMOKE enabled with no valid sequence steps"
            );
            return None;
        }

        Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;

            for (index, step) in steps.into_iter().enumerate() {
                let updated = this.update(cx, move |app, cx| {
                    let active_channel = app
                        .active_split_channel(cx)
                        .unwrap_or_else(|| "<none>".to_string());
                    let before = app.active_chat_scroll_metrics(cx);
                    match step {
                        ScrollSmokeStep::Page(direction) => {
                            app.scroll_active_chat_by_page(direction, cx);
                            let after = app.active_chat_scroll_metrics(cx);
                            crate::log_perf_render!(
                                debug,
                                "bench.scroll_smoke step={} kind=page direction={} active_channel={} before={:?} after={:?}",
                                index,
                                direction,
                                active_channel,
                                before,
                                after
                            );
                        }
                        ScrollSmokeStep::Pixels(delta_y) => {
                            app.scroll_active_chat_by_pixels(delta_y, cx);
                            let after = app.active_chat_scroll_metrics(cx);
                            crate::log_perf_render!(
                                debug,
                                "bench.scroll_smoke step={} kind=pixels delta_y={} active_channel={} before={:?} after={:?}",
                                index,
                                delta_y,
                                active_channel,
                                before,
                                after
                            );
                        }
                    }
                });

                if updated.is_err() {
                    break;
                }

                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms))
                    .await;
            }
        }))
    }

    /// Drive the disposable channel-header, minimal-tab, and Activity
    /// interaction fixture. Every mutation stays in memory, all pointer input
    /// is supplied by the supervising Windows harness, and no send/moderation/
    /// browser action is available in this sequence.
    fn spawn_ui_fixture_smoke_task(
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        use backend::hotkeys::HotkeyAction;
        use backend::lock_utils::MutexExt;

        if !crate::benchmark_ui_fixture::is_enabled() {
            return None;
        }

        let delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_UI_SMOKE_DELAY_MS").unwrap_or(4_000);
        let step_delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_UI_SMOKE_STEP_DELAY_MS")
                .unwrap_or(400)
                .max(250);
        let capture_dwell_ms = 2_000;
        let settings_only =
            crate::benchmark_ui_fixture::env_flag("ECHO_BENCH_UI_FIXTURE_SETTINGS_ONLY");

        Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;

            let activity_fixture = backend::activity_smoke::seed();
            macro_rules! fail_ui_fixture {
                ($stage:literal, $reason:literal) => {{
                    let transient_cleanup = if let Some(entity) = this.upgrade() {
                        cx.update(|cx| {
                            entity.update(cx, |app, cx| app.cleanup_ui_fixture_transients(cx))
                        })
                    } else {
                        UiFixtureTransientCleanup::default()
                    };
                    crate::log_ui!(
                        info,
                        "bench.ui_fixture teardown path=failure settings_closed={} overflow_closed={} tab_hover_cleared={} activity_closed={}",
                        transient_cleanup.settings_closed,
                        transient_cleanup.overflow_closed,
                        transient_cleanup.tab_hover_cleared,
                        transient_cleanup.activity_closed
                    );
                    crate::log_ui!(
                        error,
                        "bench.ui_fixture failed stage={} reason={}",
                        $stage,
                        $reason
                    );
                    let _ = cx.update(|cx| {
                        crate::components::chat::channel_popup::close_ui_fixture_popup(
                            crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                            cx,
                        )
                    });
                    let _ = backend::activity_smoke::cleanup(&activity_fixture);
                    crate::benchmark_ui_fixture::cleanup();
                    return;
                }};
            }

            let whisper_contract =
                backend::commands::helix::whisper_contract_snapshot();
            if !whisper_contract.scope_requested
                || !whisper_contract.request_exact
                || !whisper_contract.login_validation
                || !whisper_contract.errors_mapped
                || !whisper_contract.sanitizer_bounded
                || whisper_contract.api_error_cases != 11
            {
                fail_ui_fixture!("whisper_transport", "contract_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=whisper_transport endpoint={} scope={} scope_requested={} request_exact={} login_validation={} api_error_cases={} errors_mapped={} sanitizer_bounded={} passed=true",
                whisper_contract.endpoint,
                whisper_contract.scope,
                whisper_contract.scope_requested,
                whisper_contract.request_exact,
                whisper_contract.login_validation,
                whisper_contract.api_error_cases,
                whisper_contract.errors_mapped,
                whisper_contract.sanitizer_bounded
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("setup", "app_unavailable");
            };
            let render_before_setup = backend::metrics::get_summary().app_render_count;
            let setup_activity_fixture = activity_fixture.clone();
            let setup = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let semantic = app.app_state.update(cx, |state, cx| {
                        state.activity_panel.open = false;
                        state.activity_panel.section = ActivitySection::All;
                        state.activity_panel.event_filter =
                            backend::ui_actions::NotificationFilter::All;
                        state.refresh_sidebar_caches();
                        cx.notify();
                        crate::benchmark_ui_fixture::snapshot(state)
                    });
                    let activity = backend::activity_smoke::snapshot(&setup_activity_fixture);

                    crate::log_ui!(
                        info,
                        "bench.ui_fixture fixture_seeded tabs={} splits={} offline_channel={} live_channel={} roomstate_labels={}",
                        semantic.tabs,
                        semantic.splits,
                        crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                        crate::benchmark_ui_fixture::LIVE_CHANNEL,
                        semantic.roomstate_labels
                    );
                    crate::log_ui!(
                        info,
                        "bench.ui_fixture semantic_snapshot offline_status={} offline_title={} offline_category={} live_status={} live_title={} live_category={} live_viewers={} roomstate_labels={} selected_tabs={} highlighted_tabs={} new_message_tabs={} unread_fallback_tabs={} live_tabs={} long_title_tabs={}",
                        semantic.offline_status,
                        semantic.offline_title,
                        semantic.offline_category,
                        semantic.live_status,
                        semantic.live_title,
                        semantic.live_category,
                        semantic.live_viewers,
                        semantic.roomstate_labels,
                        semantic.selected_tabs,
                        semantic.highlighted_tabs,
                        semantic.new_message_tabs,
                        semantic.unread_fallback_tabs,
                        semantic.live_tabs,
                        semantic.long_title_tabs
                    );
                    crate::log_ui!(
                        info,
                        "bench.ui_fixture surface=activity stage=seeded mention_present={} whisper_present={} whisper_unread={} event_present={} mention_count={} whisper_count={} event_count={}",
                        activity.mention_present,
                        activity.whisper_present,
                        activity.whisper_unread,
                        activity.event_present,
                        activity.mention_count,
                        activity.whisper_count,
                        activity.event_count
                    );
                    (semantic.is_complete(), activity)
                })
            });
            let Ok((semantic_complete, seeded_activity)) = setup else {
                fail_ui_fixture!("setup", "window_update_failed");
            };
            if !semantic_complete {
                fail_ui_fixture!("setup", "semantic_fixture_incomplete");
            }
            if !seeded_activity.mention_present
                || !seeded_activity.whisper_present
                || seeded_activity.whisper_unread != 1
                || !seeded_activity.event_present
            {
                fail_ui_fixture!("activity_seed", "fixture_incomplete");
            }

            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            if backend::metrics::get_summary().app_render_count <= render_before_setup {
                fail_ui_fixture!("header_tabs", "render_not_observed");
            }
            crate::log_ui!(info, "bench.ui_fixture visual_ready checkpoint=header_tabs");
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            if !settings_only {
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("channel_popup", "app_unavailable");
            };
            let popup_opened = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    crate::components::chat::open_channel_popup(
                        crate::benchmark_ui_fixture::OFFLINE_CHANNEL.to_string(),
                        app.app_state.clone(),
                        cx,
                    )
                })
            });
            if !matches!(popup_opened, Ok(true)) {
                fail_ui_fixture!("channel_popup", "open_failed");
            }

            let mut wide_snapshot = None;
            for _ in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let snapshot = crate::components::chat::channel_popup::ui_fixture_popup_snapshot(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                );
                if snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.width >= 620.0
                        && !snapshot.expanded
                        && (snapshot.header_height - 48.0).abs() <= 0.5
                }) {
                    wide_snapshot = snapshot;
                    break;
                }
            }
            let Some(wide_snapshot) = wide_snapshot else {
                fail_ui_fixture!("channel_popup", "wide_state_unavailable");
            };
            if wide_snapshot.status != "OFFLINE"
                || !wide_snapshot.title_present
                || !wide_snapshot.category_present
                || wide_snapshot.roomstate_labels != 5
                || !wide_snapshot.overlap_safe
            {
                fail_ui_fixture!("channel_popup", "wide_state_mismatch");
            }
            if crate::benchmark_ui_fixture::feedback_ui_enabled()
                && !crate::benchmark_ui_fixture::request_feedback_bidi_input()
            {
                fail_ui_fixture!("shared_input_bidi", "request_rejected");
            }
            let wide_width = wide_snapshot.width;
            crate::log_ui!(
                info,
                "bench.ui_fixture state=channel_popup checkpoint=wide channel={} width={} expanded=false header_height=48 status=OFFLINE title=true category=true roomstate_labels=5 passed=true",
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                wide_width
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=channel_popup_wide title=Echo_-_#echo-offline-fixture passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let mut narrow_snapshot = None;
            for _ in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let snapshot = crate::components::chat::channel_popup::ui_fixture_popup_snapshot(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                );
                if snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.width <= 380.0
                        && snapshot.width < wide_width
                        && snapshot.expanded
                        && (snapshot.header_height - 68.0).abs() <= 0.5
                }) {
                    narrow_snapshot = snapshot;
                    break;
                }
            }
            let Some(narrow_snapshot) = narrow_snapshot else {
                fail_ui_fixture!("channel_popup", "narrow_state_unavailable");
            };
            if narrow_snapshot.status != "OFFLINE"
                || !narrow_snapshot.title_present
                || !narrow_snapshot.category_present
                || narrow_snapshot.roomstate_labels != 5
                || !narrow_snapshot.overlap_safe
            {
                fail_ui_fixture!("channel_popup", "narrow_state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture state=channel_popup checkpoint=narrow channel={} width={} expanded=true header_height=68 status=OFFLINE title=true category=true roomstate_labels=5 overlap_safe=true passed=true",
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                narrow_snapshot.width
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=channel_popup_narrow title=Echo_-_#echo-offline-fixture passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;
            }

            if crate::benchmark_ui_fixture::feedback_ui_enabled() {
                let mut bidi_target = None;
                for _ in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let snapshot = crate::benchmark_ui_fixture::feedback_shared_input_snapshot();
                if snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot.text_len
                        == crate::benchmark_ui_fixture::FEEDBACK_BIDI_INPUT_TEXT.len()
                        && snapshot.bounds.width > 48.0
                        && snapshot.bounds.height > 20.0
                        && snapshot
                            .target_selection_start
                            .zip(snapshot.target_selection_end)
                            .is_some_and(|(start, end)| start < end)
                        && snapshot.target_selection_start_affinity.is_some()
                        && snapshot.target_selection_end_affinity.is_some()
                        && snapshot.selection_start_x.is_some_and(|x| {
                            x > 0.0 && x < snapshot.bounds.width
                        })
                        && snapshot.selection_end_x.is_some_and(|x| {
                            x > 0.0 && x < snapshot.bounds.width
                        })
                        && snapshot.selection_spans.is_empty()
                }) {
                    bidi_target = snapshot;
                    break;
                }
            }
            let Some(bidi_target) = bidi_target else {
                fail_ui_fixture!("shared_input_bidi", "paint_target_unavailable");
            };
            let target_selection_start = bidi_target.target_selection_start.unwrap_or_default();
            let target_selection_end = bidi_target.target_selection_end.unwrap_or_default();
            let target_selection_start_affinity = bidi_target
                .target_selection_start_affinity
                .unwrap_or_default();
            let target_selection_end_affinity = bidi_target
                .target_selection_end_affinity
                .unwrap_or_default();
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=feedback_shared_input_bidi_target x={} y={} width={} height={} text_len={} selection_len=0 target_selection_start={} target_selection_end={} target_selection_start_affinity={} target_selection_end_affinity={} selection_start_local_x={} selection_end_local_x={} native_pointer_drag=true sent=false passed=true",
                bidi_target.bounds.x,
                bidi_target.bounds.y,
                bidi_target.bounds.width,
                bidi_target.bounds.height,
                bidi_target.text_len,
                target_selection_start,
                target_selection_end,
                target_selection_start_affinity.as_str(),
                target_selection_end_affinity.as_str(),
                bidi_target.selection_start_x.unwrap_or_default(),
                bidi_target.selection_end_x.unwrap_or_default(),
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let mut bidi_selection = None;
            let mut last_bidi_selection = None;
            for _ in 0..100 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let snapshot = crate::benchmark_ui_fixture::feedback_shared_input_snapshot();
                if snapshot.is_some() {
                    last_bidi_selection = snapshot.clone();
                }
                if snapshot.as_ref().is_some_and(|snapshot| {
                    snapshot
                        .selection_start
                        .zip(snapshot.selection_end)
                        .is_some_and(|(start, end)| {
                            start == target_selection_start && end == target_selection_end
                        })
                        && snapshot.cursor_affinity == target_selection_end_affinity
                        && snapshot.selection_spans.len() >= 2
                        && snapshot
                            .selection_spans
                            .iter()
                            .all(|span| span.width > 0.0 && span.height > 0.0)
                }) {
                    bidi_selection = snapshot;
                    break;
                }
            }
            let Some(bidi_selection) = bidi_selection else {
                if let Some(snapshot) = last_bidi_selection {
                    let span_geometry = snapshot
                        .selection_spans
                        .iter()
                        .map(|span| format!("{}:{}", span.x, span.width))
                        .collect::<Vec<_>>()
                        .join(",");
                    crate::log_ui!(
                        error,
                        "bench.ui_fixture diagnostic stage=shared_input_bidi text_len={} cursor={} cursor_affinity={} target_selection_start={} target_selection_end={} target_selection_start_affinity={} target_selection_end_affinity={} selection_start={} selection_end={} selection_span_count={} selection_span_geometry={} selection_start_local_x={} selection_end_local_x={}",
                        snapshot.text_len,
                        snapshot.cursor,
                        snapshot.cursor_affinity.as_str(),
                        target_selection_start,
                        target_selection_end,
                        target_selection_start_affinity.as_str(),
                        target_selection_end_affinity.as_str(),
                        snapshot.selection_start.map_or(-1, |value| value as isize),
                        snapshot.selection_end.map_or(-1, |value| value as isize),
                        snapshot.selection_spans.len(),
                        span_geometry,
                        snapshot.selection_start_x.unwrap_or_default(),
                        snapshot.selection_end_x.unwrap_or_default(),
                    );
                } else {
                    crate::log_ui!(
                        error,
                        "bench.ui_fixture diagnostic stage=shared_input_bidi snapshot=none"
                    );
                }
                fail_ui_fixture!("shared_input_bidi", "multi_span_selection_unavailable");
            };
            let selection_start = bidi_selection.selection_start.unwrap_or_default();
            let selection_end = bidi_selection.selection_end.unwrap_or_default();
            let span_geometry = bidi_selection
                .selection_spans
                .iter()
                .map(|span| format!("{}:{}", span.x, span.width))
                .collect::<Vec<_>>()
                .join(",");
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=shared_input stage=bidi_pointer_selection input=production_shared_input native_pointer_drag=true text_len={} cursor={} cursor_affinity={} target_selection_start={} target_selection_end={} target_selection_start_affinity={} target_selection_end_affinity={} selection_start={} selection_end={} selection_span_count={} selection_span_geometry={} sent=false passed=true",
                bidi_selection.text_len,
                bidi_selection.cursor,
                bidi_selection.cursor_affinity.as_str(),
                target_selection_start,
                target_selection_end,
                target_selection_start_affinity.as_str(),
                target_selection_end_affinity.as_str(),
                selection_start,
                selection_end,
                bidi_selection.selection_spans.len(),
                span_geometry,
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=feedback_shared_input_bidi_selected passed=true"
            );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;
            }

            let close_requested = cx.update(|cx| {
                crate::components::chat::channel_popup::close_ui_fixture_popup(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                    cx,
                )
            });
            if !close_requested {
                fail_ui_fixture!("channel_popup", "close_failed");
            }
            for _ in 0..50 {
                if !crate::components::chat::channel_popup::ui_fixture_popup_is_open(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                ) {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
            }
            if crate::components::chat::channel_popup::ui_fixture_popup_is_open(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
            ) {
                fail_ui_fixture!("channel_popup", "close_not_observed");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture state=channel_popup open=false passed=true"
            );
            crate::benchmark_ui_fixture::reset_feedback_ui_snapshots();

            if !settings_only {
            if !backend::emotes::seed_completion_emotes_for_benchmark(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
            ) {
                fail_ui_fixture!("completion_seed", "emote_seed_failed");
            }
            let _ = backend::irc::clear_recent_chatters_for_benchmark(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
            );
            for (login, display) in crate::benchmark_ui_fixture::COMPLETION_CHATTERS
                .iter()
                .rev()
            {
                backend::irc::track_chatter(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                    login,
                    display,
                    None,
                );
            }
            let completion_messages_before = backend::get_messages(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                crate::config::MAX_CACHED_MESSAGES as u32,
            )
            .len();
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=completion stage=seeded channel={} emotes={} mentions={} thread_root=true messages_before={} passed=true",
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                crate::benchmark_ui_fixture::COMPLETION_EMOTES.len(),
                crate::benchmark_ui_fixture::COMPLETION_CHATTERS.len(),
                completion_messages_before
            );

            let mut thread_completion_opened = false;
            for (completion_target, completion_mode) in [
                ("main", "emote"),
                ("main", "mention"),
                ("watching", "emote"),
                ("thread", "emote"),
                ("thread", "mention"),
            ] {
                let watching_restore = if completion_target == "watching" {
                    let Some(restore) =
                        backend::browser_watching::set_current_channel_for_benchmark(
                            crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                        )
                    else {
                        fail_ui_fixture!("completion_watching", "state_unavailable");
                    };
                    let Some(entity) = this.upgrade() else {
                        fail_ui_fixture!("completion_watching", "app_unavailable");
                    };
                    let switched =
                        cx.update_window(window_handle, move |_view, _window, cx| {
                            entity.update(cx, |app, cx| {
                                let path = app
                                    .split_layout
                                    .read(cx)
                                    .get_active_split_path()
                                    .to_vec();
                                app.app_state.update(cx, |state, cx| {
                                    let changed = state.active_tab_mut().set_channel_at_path(
                                        &path,
                                        backend::browser_watching::WATCHING_CHANNEL.to_string(),
                                    );
                                    cx.notify();
                                    changed
                                })
                            })
                        });
                    if !matches!(switched, Ok(true)) {
                        fail_ui_fixture!("completion_watching", "split_switch_failed");
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(step_delay_ms * 2))
                        .await;
                    Some(restore)
                } else {
                    None
                };

                if completion_target == "thread" && !thread_completion_opened {
                    let Some(entity) = this.upgrade() else {
                        fail_ui_fixture!("completion_thread", "app_unavailable");
                    };
                    let opened = cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            app.overlay_manager.update(cx, |overlays, cx| {
                                overlays.open_thread_viewer(
                                    "echo-fixture-thread-root".to_string(),
                                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL.to_string(),
                                    190.0,
                                    145.0,
                                    cx,
                                );
                            });
                            cx.notify();
                            true
                        })
                    });
                    if !matches!(opened, Ok(true)) {
                        fail_ui_fixture!("completion_thread", "open_failed");
                    }
                    thread_completion_opened = true;
                    cx.background_executor()
                        .timer(Duration::from_millis(step_delay_ms * 2))
                        .await;
                }

                let trigger_text = if completion_mode == "emote" {
                    ":ye"
                } else {
                    "@EchoFixtureUser"
                };
                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("completion_suggestions", "app_unavailable");
                };
                let prepared = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        if completion_target != "thread" {
                            app.prepare_main_completion_smoke(trigger_text, window, cx)
                        } else {
                            let viewer =
                                app.overlay_manager.read(cx).thread_viewer.clone();
                            viewer.update(cx, |viewer, cx| {
                                viewer.prepare_completion_smoke(trigger_text, window, cx)
                            })
                        }
                    })
                });
                let Ok(Some(_)) = prepared else {
                    fail_ui_fixture!("completion_suggestions", "prepare_failed");
                };
                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("completion_suggestions", "app_unavailable");
                };
                let snapshot = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        app.completion_smoke_snapshot_for(completion_target, window, cx)
                    })
                });
                let Ok(Some(suggestion_snapshot)) = snapshot else {
                    fail_ui_fixture!("completion_suggestions", "snapshot_failed");
                };
                if !suggestion_snapshot.focused
                    || !suggestion_snapshot.suggestions_open
                    || suggestion_snapshot.suggestion_count < 3
                    || suggestion_snapshot.selected_text.is_none()
                {
                    fail_ui_fixture!("completion_suggestions", "state_mismatch");
                }
                let selected_text = suggestion_snapshot.selected_text.unwrap_or_default();
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=completion target={} mode={} stage=suggestions trigger={} suggestions={} selected={} focused=true sent=false passed=true",
                    completion_target,
                    completion_mode,
                    if completion_mode == "emote" { "colon" } else { "at" },
                    suggestion_snapshot.suggestion_count,
                    selected_text
                );

                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;
                let expected_suggestion_input = format!("{selected_text} ");
                let mut accepted_snapshot = None;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let Some(entity) = this.upgrade() else {
                        break;
                    };
                    let snapshot = cx.update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |app, cx| {
                            app.completion_smoke_snapshot_for(completion_target, window, cx)
                        })
                    });
                    if let Ok(Some(snapshot)) = snapshot
                        && snapshot.input == expected_suggestion_input
                            && snapshot.cursor == expected_suggestion_input.len()
                            && !snapshot.suggestions_open
                        {
                            accepted_snapshot = Some(snapshot);
                            break;
                        }
                }
                let Some(accepted_snapshot) = accepted_snapshot else {
                    fail_ui_fixture!("completion_suggestion_accept", "not_observed");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=completion target={} mode={} stage=suggestion_accept key=native_tab input_exact=true cursor_exact=true bar_open=false focused={} sent=false passed=true",
                    completion_target,
                    completion_mode,
                    accepted_snapshot.focused
                );

                let inline_query = if completion_mode == "emote" {
                    "ye"
                } else {
                    "EchoFixtureUser"
                };
                let inline_candidates =
                    crate::benchmark_ui_fixture::completion_inline_candidates(
                        crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                        inline_query,
                    );
                if inline_candidates.len() < 3 {
                    fail_ui_fixture!("completion_inline", "insufficient_candidates");
                }
                let last_candidate = inline_candidates.len() - 1;
                let expected_indices = [0usize, 1, 0, last_candidate];
                let reverse_steps = [false, false, true, true];

                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("completion_inline", "app_unavailable");
                };
                let prepared = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        if completion_target != "thread" {
                            app.prepare_main_completion_smoke(inline_query, window, cx)
                        } else {
                            let viewer =
                                app.overlay_manager.read(cx).thread_viewer.clone();
                            viewer.update(cx, |viewer, cx| {
                                viewer.prepare_completion_smoke(inline_query, window, cx)
                            })
                        }
                    })
                });
                let Ok(Some(inline_start)) = prepared else {
                    fail_ui_fixture!("completion_inline", "prepare_failed");
                };
                if !inline_start.focused || inline_start.suggestions_open {
                    fail_ui_fixture!("completion_inline", "initial_state_mismatch");
                }
                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms))
                    .await;

                for (step_index, (&candidate_index, &reverse)) in expected_indices
                    .iter()
                    .zip(reverse_steps.iter())
                    .enumerate()
                {
                    let step = step_index + 1;
                    crate::log_ui!(
                        info,
                        "bench.ui_fixture surface=completion target={} mode={} stage=inline_ready step={} direction={} key=native_tab focused=true sent=false passed=true",
                        completion_target,
                        completion_mode,
                        step,
                        if reverse { "reverse" } else { "forward" }
                    );
                    let expected_candidate = inline_candidates[candidate_index].text.clone();
                    let expected_input = format!("{expected_candidate} ");
                    let mut cycle_snapshot = None;
                    for _ in 0..100 {
                        cx.background_executor()
                            .timer(Duration::from_millis(100))
                            .await;
                        let Some(entity) = this.upgrade() else {
                            break;
                        };
                        let snapshot =
                            cx.update_window(window_handle, move |_view, window, cx| {
                                entity.update(cx, |app, cx| {
                                    app.completion_smoke_snapshot_for(completion_target, window, cx)
                                })
                            });
                        if let Ok(Some(snapshot)) = snapshot
                            && snapshot.input == expected_input
                                && snapshot.cursor == expected_input.len()
                                && !snapshot.suggestions_open
                            {
                                cycle_snapshot = Some(snapshot);
                                break;
                            }
                    }
                    let Some(cycle_snapshot) = cycle_snapshot else {
                        fail_ui_fixture!("completion_inline", "cycle_not_observed");
                    };
                    crate::log_ui!(
                        info,
                        "bench.ui_fixture surface=completion target={} mode={} stage=inline_cycle step={} key=native_tab candidate={} replacement_count=1 cursor_exact=true focused={} sent=false passed=true",
                        completion_target,
                        completion_mode,
                        step,
                        expected_candidate,
                        cycle_snapshot.focused
                    );
                    if step == 4 {
                        cx.background_executor()
                            .timer(Duration::from_millis(capture_dwell_ms))
                            .await;
                    }
                }

                if let Some(restore) = watching_restore {
                    let Some(entity) = this.upgrade() else {
                        fail_ui_fixture!("completion_watching", "app_unavailable");
                    };
                    let restored =
                        cx.update_window(window_handle, move |_view, _window, cx| {
                            entity.update(cx, |app, cx| {
                                let path = app
                                    .split_layout
                                    .read(cx)
                                    .get_active_split_path()
                                    .to_vec();
                                app.app_state.update(cx, |state, cx| {
                                    let changed = state.active_tab_mut().set_channel_at_path(
                                        &path,
                                        crate::benchmark_ui_fixture::OFFLINE_CHANNEL.to_string(),
                                    );
                                    cx.notify();
                                    changed
                                })
                            })
                        });
                    if !matches!(restored, Ok(true))
                        || !backend::browser_watching::restore_current_channel_after_benchmark(
                            restore,
                        )
                    {
                        fail_ui_fixture!("completion_watching", "restore_failed");
                    }
                }
            }

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("completion_cleanup", "app_unavailable");
            };
            let cleanup_snapshot = cx.update_window(window_handle, move |_view, window, cx| {
                entity.update(cx, |app, cx| {
                    let main_clean = app
                        .prepare_main_completion_smoke("", window, cx)
                        .is_some_and(|snapshot| snapshot.input.is_empty());
                    let viewer = app.overlay_manager.read(cx).thread_viewer.clone();
                    let thread_clean = viewer
                        .update(cx, |viewer, cx| {
                            viewer.prepare_completion_smoke("", window, cx)
                        })
                        .is_some_and(|snapshot| snapshot.input.is_empty());
                    app.overlay_manager.update(cx, |overlays, cx| {
                        overlays.close_thread_viewer(cx);
                    });
                    (main_clean, thread_clean, !viewer.read(cx).is_open())
                })
            });
            let Ok((main_clean, thread_clean, thread_closed)) = cleanup_snapshot else {
                fail_ui_fixture!("completion_cleanup", "snapshot_failed");
            };
            let completion_messages_after = backend::get_messages(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                crate::config::MAX_CACHED_MESSAGES as u32,
            )
            .len();
            if !main_clean
                || !thread_clean
                || !thread_closed
                || completion_messages_after != completion_messages_before
            {
                fail_ui_fixture!("completion_cleanup", "state_mismatch");
            }
            let _ = backend::emotes::clear_completion_emotes_for_benchmark(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
            );
            let _ = backend::irc::clear_recent_chatters_for_benchmark(
                crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=completion stage=complete main_clean=true thread_clean=true thread_open=false messages_unchanged=true sent=false passed=true"
            );
            }

            let persisted_settings_before =
                backend::settings::SETTINGS.lock_or_recover().clone();
            let settings_render_before = backend::metrics::get_summary().app_render_count;
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_open", "app_unavailable");
            };
            let opened = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshot = settings.update(cx, |state, cx| {
                        state.open();
                        state.selected_section =
                            crate::components::settings::SettingsSectionId::General;
                        state.benchmark_focus_search();
                        cx.notify();
                        state.benchmark_snapshot()
                    });
                    cx.notify();
                    snapshot
                })
            });
            let Ok(open_snapshot) = opened else {
                fail_ui_fixture!("settings_open", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if !open_snapshot.is_open
                || !open_snapshot.search_focused
                || !open_snapshot.query.is_empty()
                || open_snapshot.advanced_revealed
                || open_snapshot.developer_visible
                || !settings_persisted_unchanged
                || backend::metrics::get_summary().app_render_count <= settings_render_before
            {
                fail_ui_fixture!("settings_open", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=open open=true search_focused=true query_empty=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true"
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_default passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_tree_default", "app_unavailable");
            };
            let tree_default = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    settings.update(cx, |state, _| state.benchmark_navigation_snapshot())
                })
            });
            let Ok(tree_snapshot) = tree_default else {
                fail_ui_fixture!("settings_tree_default", "controller_failed");
            };
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            let expected_default_rows: Vec<String> = [
                "0:General",
                "0:Appearance",
                "0:Chat",
                "0:Highlights & notifications",
                "0:Moderation & filters",
                "0:Links & media",
                "0:Shortcuts & commands",
                "0:Advanced",
            ]
            .iter()
            .map(|row| (*row).to_string())
            .collect();
            if tree_snapshot.page != "general"
                || tree_snapshot.anchor != Some("account")
                || tree_snapshot.focused_row != Some("General")
                || tree_snapshot.tree_rows != expected_default_rows
                || tree_snapshot.status.is_some()
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_tree_default", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=tree_default page=general anchor=account focused=General rows={} developer_rows=0 status=none persisted_unchanged=true passed=true",
                tree_snapshot.tree_rows.len()
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_tree_default passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_tree_keys", "app_unavailable");
            };
            let tree_keys = cx.update_window(window_handle, move |_view, window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        let mut handled = state.benchmark_press_tree_key("down", window, cx);
                        handled &= state.benchmark_press_tree_key("enter", window, cx);
                        handled &= state.benchmark_press_tree_key("c", window, cx);
                        handled &= state.benchmark_press_tree_key("enter", window, cx);
                        cx.notify();
                        (handled, state.benchmark_navigation_snapshot())
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((tree_keys_handled, tree_keys_snapshot)) = tree_keys else {
                fail_ui_fixture!("settings_tree_keys", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            if !tree_keys_handled
                || tree_keys_snapshot.page != "chat"
                || tree_keys_snapshot.anchor != Some("reading")
                || tree_keys_snapshot.focused_row != Some("Chat")
                || tree_keys_snapshot.tree_rows != expected_default_rows
            {
                fail_ui_fixture!("settings_tree_keys", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=tree_keys keys=down,enter,typeahead(c),enter native=true page=chat anchor=reading focused=Chat rows={} passed=true",
                tree_keys_snapshot.tree_rows.len()
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_flat_navigation passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_focus_cycle", "app_unavailable");
            };
            let focus_cycle_start = cx.update_window(window_handle, move |_view, window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_focus_search_region(window, cx);
                        let search = state.benchmark_focus_region(window, cx);
                        state.benchmark_press_surface_key("f6", false, window, cx);
                        let navigation = state.benchmark_focus_region(window, cx);
                        state.benchmark_press_surface_key("f6", false, window, cx);
                        (search, navigation)
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((search_region, navigation_region)) = focus_cycle_start
            else {
                fail_ui_fixture!("settings_focus_cycle", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_focus_cycle", "app_unavailable");
            };
            let focus_cycle_settled = cx.update_window(window_handle, move |_view, window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        let content = state.benchmark_focus_region(window, cx);
                        state.benchmark_press_surface_key("f6", true, window, cx);
                        let reverse_navigation = state.benchmark_focus_region(window, cx);
                        state.benchmark_press_surface_key("f6", true, window, cx);
                        let reverse_search = state.benchmark_focus_region(window, cx);
                        (content, reverse_navigation, reverse_search)
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((content_region, reverse_navigation_region, reverse_search_region)) =
                focus_cycle_settled
            else {
                fail_ui_fixture!("settings_focus_cycle", "controller_failed");
            };
            if search_region != "search"
                || navigation_region != "navigation"
                || content_region != "content"
                || reverse_navigation_region != "navigation"
                || reverse_search_region != "search"
            {
                fail_ui_fixture!("settings_focus_cycle", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=focus_cycle search,navigation,content,navigation,search native=true passed=true"
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_anchor_jump", "app_unavailable");
            };
            let anchor_jump = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    settings.update(cx, |state, cx| {
                        state.open_at(
                            &crate::components::settings::SettingsOpenRequest::at(
                                crate::components::settings::SettingsLocation::anchor(
                                    SettingsSectionId::Nicknames,
                                    crate::components::settings::SettingsSubsectionId::NicknameRules,
                                ),
                            ),
                        );
                        cx.notify();
                    });
                    cx.notify();
                })
            });
            if anchor_jump.is_err() {
                fail_ui_fixture!("settings_anchor_jump", "controller_failed");
            }
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 3))
                .await;
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_anchor_jump", "app_unavailable");
            };
            let anchor_settled = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    settings.update(cx, |state, _| {
                        (state.benchmark_navigation_snapshot(), state.benchmark_disclosure_expanded("chat-nicknames"))
                    })
                })
            });
            let Ok((anchor_snapshot, nickname_revealed)) = anchor_settled else {
                fail_ui_fixture!("settings_anchor_jump", "controller_failed");
            };
            if anchor_snapshot.page != "chat"
                || anchor_snapshot.anchor != Some("chat-options")
                || anchor_snapshot.active_block != anchor_snapshot.target_block
                || anchor_snapshot.anchor_pending
                || anchor_snapshot.target_block == 0
                || !nickname_revealed
            {
                fail_ui_fixture!("settings_anchor_jump", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=anchor_jump legacy_page=nicknames legacy_anchor=nickname-rules page=chat anchor=chat-options disclosure=chat-nicknames revealed=true target_block={} active_block={} pending=false anchor_addressed=true passed=true",
                anchor_snapshot.target_block,
                anchor_snapshot.active_block
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_anchor_jump passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            backend::updates::show_up_to_date_for_benchmark();
            let Some(entity) = this.upgrade() else {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_up_to_date", "app_unavailable");
            };
            let update_settings = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshot = settings.update(cx, |state, cx| {
                        state.selected_section =
                            crate::components::settings::SettingsSectionId::Updates;
                        cx.notify();
                        state.benchmark_snapshot()
                    });
                    cx.notify();
                    snapshot
                })
            });
            let Ok(update_settings_snapshot) = update_settings else {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_up_to_date", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let up_to_date = backend::updates::snapshot();
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if update_settings_snapshot.selected_destination != "General"
                || up_to_date.kind != backend::updates::UpdateStateKind::UpToDate
                || up_to_date.latest_version.as_deref()
                    != Some(up_to_date.current_version.as_str())
                || !settings_persisted_unchanged
            {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_up_to_date", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings section=updates destination=General state=up_to_date installed={} latest={} persisted_unchanged=true passed=true",
                up_to_date.current_version,
                up_to_date.latest_version.as_deref().unwrap_or("missing")
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_updates_up_to_date passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let available_version = backend::updates::show_available_for_benchmark();
            let Some(entity) = this.upgrade() else {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_available", "app_unavailable");
            };
            let available_rendered =
                cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |_app, cx| cx.notify())
                });
            if available_rendered.is_err() {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_available", "controller_failed");
            }
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let available = backend::updates::snapshot();
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if available.kind != backend::updates::UpdateStateKind::Available
                || available.latest_version.as_deref() != Some(available_version.as_str())
                || !settings_persisted_unchanged
            {
                backend::updates::clear_benchmark_snapshot();
                fail_ui_fixture!("settings_updates_available", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings section=updates destination=General state=available installed={} available={} persisted_unchanged=true passed=true",
                available.current_version,
                available.latest_version.as_deref().unwrap_or("missing")
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_updates_available passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;
            backend::updates::clear_benchmark_snapshot();

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_ordinary_query", "app_unavailable");
            };
            let ordinary = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshot = settings.update(cx, |state, cx| {
                        state.benchmark_set_search_query("emote");
                        cx.notify();
                        state.benchmark_snapshot()
                    });
                    cx.notify();
                    snapshot
                })
            });
            let Ok(ordinary_snapshot) = ordinary else {
                fail_ui_fixture!("settings_ordinary_query", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if ordinary_snapshot.query != "emote"
                || ordinary_snapshot.selected_destination != "Chat"
                || ordinary_snapshot.visible_result_count == 0
                || ordinary_snapshot.no_results
                || ordinary_snapshot.advanced_revealed
                || ordinary_snapshot.developer_visible
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_ordinary_query", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=query kind=ordinary query=emote selected=Chat result_count={} no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true",
                ordinary_snapshot.visible_result_count
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_search_ordinary passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_setting_search", "app_unavailable");
            };
            let setting_search = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_set_search_query("streamlink");
                        cx.notify();
                        (
                            state.benchmark_navigation_snapshot(),
                            state.benchmark_secondary_text(),
                            state.benchmark_snapshot(),
                        )
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((setting_nav, setting_secondary, setting_snapshot)) = setting_search else {
                fail_ui_fixture!("settings_setting_search", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if setting_snapshot.selected_destination != "Links & media"
                || setting_snapshot.visible_result_count != 1
                || setting_snapshot.no_results
                || setting_nav.page != "links-media"
                || setting_nav.anchor != Some("players")
                || setting_nav.status.as_deref() != Some("4 settings in 1 subsection")
                || setting_secondary.as_deref() != Some("Streamlink path \u{b7} +3")
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_setting_search", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=query kind=setting query=streamlink page=links-media anchor=players matched_settings=4 secondary=Streamlink_path_+3 status=4_settings_in_1_subsection persisted_unchanged=true passed=true"
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_search_setting passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_developer_hidden", "app_unavailable");
            };
            let hidden = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_set_search_query("fps");
                        cx.notify();
                        (state.benchmark_snapshot(), state.benchmark_navigation_snapshot())
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((hidden_snapshot, hidden_nav)) = hidden else {
                fail_ui_fixture!("settings_developer_hidden", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if hidden_snapshot.query != "fps"
                || hidden_snapshot.visible_result_count != 0
                || !hidden_snapshot.no_results
                || hidden_snapshot.advanced_revealed
                || hidden_snapshot.developer_visible
                || hidden_nav.hidden_subsections == 0
                || hidden_nav.status.as_deref() != Some("No settings found")
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_developer_hidden", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=query kind=developer_hidden query=fps result_count=0 no_results=true advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true"
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_search_developer_hidden passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_advanced_reveal", "app_unavailable");
            };
            let revealed = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_reveal_advanced();
                        cx.notify();
                        (state.benchmark_snapshot(), state.benchmark_navigation_snapshot())
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((revealed_snapshot, revealed_nav)) = revealed else {
                fail_ui_fixture!("settings_advanced_reveal", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if revealed_snapshot.query != "fps"
                || revealed_snapshot.selected_destination != "Advanced"
                || revealed_snapshot.visible_result_count == 0
                || revealed_snapshot.no_results
                || !revealed_snapshot.advanced_revealed
                || !revealed_snapshot.developer_visible
                || revealed_nav.page != "advanced"
                || revealed_nav.anchor != Some("diagnostics")
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_advanced_reveal", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=reveal query=fps selected=Advanced page=advanced anchor=diagnostics result_count={} no_results=false advanced_revealed=true developer_visible=true persisted_unchanged=true passed=true",
                revealed_snapshot.visible_result_count
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_advanced_revealed passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_developer_setting_recovery", "app_unavailable");
            };
            let recovery = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_focus_search();
                        state.benchmark_set_search_query("frame");
                        let concealed =
                            (state.benchmark_snapshot(), state.benchmark_navigation_snapshot());
                        state.benchmark_reveal_advanced();
                        cx.notify();
                        (
                            concealed,
                            state.benchmark_snapshot(),
                            state.benchmark_navigation_snapshot(),
                        )
                    });
                    cx.notify();
                    result
                })
            });
            let Ok(((concealed_snapshot, concealed_nav), recovered_snapshot, recovered_nav)) =
                recovery
            else {
                fail_ui_fixture!("settings_developer_setting_recovery", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if concealed_snapshot.visible_result_count != 0
                || !concealed_snapshot.no_results
                || concealed_snapshot.developer_visible
                || concealed_nav.hidden_settings == 0
                || recovered_snapshot.visible_result_count == 0
                || !recovered_snapshot.developer_visible
                || recovered_nav.page != "advanced"
                || recovered_nav.anchor != Some("diagnostics")
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_developer_setting_recovery", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=developer_recovery query=frame hidden_settings={} recovered_page=advanced recovered_anchor=diagnostics persisted_unchanged=true passed=true",
                concealed_nav.hidden_settings
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_developer_recovery passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_legacy_slug", "app_unavailable");
            };
            let legacy = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let result = settings.update(cx, |state, cx| {
                        state.benchmark_open_slug("logging");
                        cx.notify();
                        (state.benchmark_navigation_snapshot(), state.benchmark_snapshot())
                    });
                    cx.notify();
                    result
                })
            });
            let Ok((legacy_nav, legacy_snapshot)) = legacy else {
                fail_ui_fixture!("settings_legacy_slug", "controller_failed");
            };
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if legacy_nav.page != "advanced"
                || legacy_nav.anchor != Some("chat-logging")
                || !legacy_snapshot.query.is_empty()
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_legacy_slug", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=legacy_slug slug=logging page=advanced anchor=chat-logging query_cleared=true persisted_unchanged=true passed=true"
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_legacy_sections", "app_unavailable");
            };
            let legacy_sections = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshots = settings.update(cx, |state, cx| {
                        state.selected_section = SettingsSectionId::General;
                        state.benchmark_set_search_query("");
                        let general = (state.selected_section, state.benchmark_navigation_snapshot());

                        state.selected_section = SettingsSectionId::Updates;
                        state.benchmark_set_search_query("");
                        let updates = (state.selected_section, state.benchmark_navigation_snapshot());

                        state.selected_section = SettingsSectionId::Sounds;
                        state.benchmark_set_search_query("");
                        let sounds = (state.selected_section, state.benchmark_navigation_snapshot());
                        cx.notify();
                        (general, updates, sounds)
                    });
                    cx.notify();
                    snapshots
                })
            });
            let Ok((legacy_general, legacy_updates, legacy_sounds)) = legacy_sections else {
                fail_ui_fixture!("settings_legacy_sections", "controller_failed");
            };
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if legacy_general.0 != SettingsSectionId::General
                || legacy_general.1.page != "general"
                || legacy_general.1.anchor != Some("account")
                || legacy_updates.0 != SettingsSectionId::General
                || legacy_updates.1.page != "general"
                || legacy_updates.1.anchor != Some("update-checks")
                || legacy_sounds.0 != SettingsSectionId::Highlights
                || legacy_sounds.1.page != "highlights-notifications"
                || legacy_sounds.1.anchor != Some("sounds")
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_legacy_sections", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=legacy_sections general=general/account updates=general/update-checks sounds=highlights-notifications/sounds canonical_sections=true persisted_unchanged=true passed=true"
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_popout_transfer", "app_unavailable");
            };
            let transfer = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let (nav, request) = settings.update(cx, |state, cx| {
                        state.benchmark_set_search_query("streamlink");
                        cx.notify();
                        (
                            state.benchmark_navigation_snapshot(),
                            state.surface().open_request(),
                        )
                    });
                    crate::components::settings::request_settings_popout(request.clone());
                    let drained = crate::components::settings::drain_settings_popout_request();
                    let lands_here = drained.as_ref().is_some_and(|drained| {
                        settings.read(cx).benchmark_request_lands_here(drained)
                    });
                    cx.notify();
                    (nav, request, drained, lands_here)
                })
            });
            let Ok((transfer_nav, transfer_request, transfer_drained, transfer_lands_here)) =
                transfer
            else {
                fail_ui_fixture!("settings_popout_transfer", "controller_failed");
            };
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if !transfer_nav.transfer_matches_location
                || transfer_nav.transfer_query.as_deref() != Some("streamlink")
                || transfer_drained.as_ref() != Some(&transfer_request)
                || !transfer_lands_here
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_popout_transfer", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=popout_transfer page={} anchor={} query=streamlink handoff_round_trip=true resolves_to_same_location=true persisted_unchanged=true passed=true",
                transfer_nav.page,
                transfer_nav.anchor.unwrap_or("none")
            );

            let narrow = crate::utils::fit_overlay_to_viewport(
                gpui::size(
                    gpui::px(config::SETTINGS_POPOUT_MIN_WIDTH_PX),
                    gpui::px(config::SETTINGS_POPOUT_MIN_HEIGHT_PX),
                ),
                config::SETTINGS_MODAL_WIDTH_PX,
                config::SETTINGS_MODAL_HEIGHT_PX,
                8.0,
            );
            let narrow_content_width =
                narrow.width - config::SETTINGS_SIDEBAR_WIDTH_PX - 1.0;
            let narrow_content_height = narrow.height
                - crate::components::settings::widgets::SettingsMetrics::TOP_BAR_HEIGHT
                - config::SETTINGS_CONTENT_HEADER_HEIGHT_PX;
            if narrow.width > config::SETTINGS_POPOUT_MIN_WIDTH_PX
                || narrow.height > config::SETTINGS_POPOUT_MIN_HEIGHT_PX
                || narrow_content_width <= 0.0
                || narrow_content_height <= 0.0
            {
                fail_ui_fixture!("settings_narrow_layout", "layout_overflow");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=narrow_layout viewport=600x400 surface_width={} surface_height={} sidebar_width={} content_width={} content_height={} clipped=false passed=true",
                narrow.width,
                narrow.height,
                config::SETTINGS_SIDEBAR_WIDTH_PX,
                narrow_content_width,
                narrow_content_height
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=settings_narrow_layout passed=true"
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_empty_query", "app_unavailable");
            };
            let empty = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshot = settings.update(cx, |state, cx| {
                        state.benchmark_close_and_cleanup();
                        state.open();
                        state.selected_section =
                            crate::components::settings::SettingsSectionId::General;
                        state.benchmark_focus_search();
                        state.benchmark_set_search_query("");
                        cx.notify();
                        state.benchmark_snapshot()
                    });
                    cx.notify();
                    snapshot
                })
            });
            let Ok(empty_snapshot) = empty else {
                fail_ui_fixture!("settings_empty_query", "controller_failed");
            };
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if !empty_snapshot.is_open
                || !empty_snapshot.search_focused
                || !empty_snapshot.query.is_empty()
                || empty_snapshot.visible_result_count == 0
                || empty_snapshot.no_results
                || empty_snapshot.advanced_revealed
                || empty_snapshot.developer_visible
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_empty_query", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=empty query_empty=true result_count={} no_results=false advanced_revealed=false developer_visible=false persisted_unchanged=true passed=true",
                empty_snapshot.visible_result_count
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("settings_cleanup", "app_unavailable");
            };
            let closed = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let settings = app.overlay_manager.read(cx).settings_modal.clone();
                    let snapshot = settings.update(cx, |state, cx| {
                        state.benchmark_close_and_cleanup();
                        cx.notify();
                        state.benchmark_snapshot()
                    });
                    cx.notify();
                    snapshot
                })
            });
            let Ok(closed_snapshot) = closed else {
                fail_ui_fixture!("settings_cleanup", "controller_failed");
            };
            let settings_persisted_unchanged =
                *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
            if closed_snapshot.is_open
                || !closed_snapshot.query.is_empty()
                || closed_snapshot.advanced_revealed
                || !settings_persisted_unchanged
            {
                fail_ui_fixture!("settings_cleanup", "state_mismatch");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=settings stage=complete open=false query_empty=true advanced_revealed=false persisted_unchanged=true passed=true"
            );

            if crate::benchmark_ui_fixture::feedback_ui_enabled() {
                crate::benchmark_ui_fixture::reset_feedback_ui_snapshots();
                let sound_settings_before = backend::sounds::SOUND_STORE.settings();
                let sound_file_before =
                    backend::sounds::SOUND_STORE.file_bytes_for_benchmark();
                let feedback_messages_before = backend::get_messages(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                    crate::config::MAX_CACHED_MESSAGES as u32,
                )
                .len();

                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("feedback_setup", "app_unavailable");
                };
                let feedback_setup =
                    cx.update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |app, cx| {
                            let path = app.split_layout.read(cx).get_active_split_path().to_vec();
                            let state = app.app_state.read(cx);
                            let channel = state.active_tab().layout.get_channel_at_path(&path)?;
                            let split_id = state.active_tab().layout.get_split_id_at_path(&path)?;
                            let renderer = app.chat_input_renderer.read(cx);
                            let restore = FeedbackUiRestoreState {
                                path: path.clone(),
                                split_id,
                                channel,
                                draft: renderer
                                    .input_manager
                                    .input_texts
                                    .get(&split_id)
                                    .cloned(),
                                cursor: renderer
                                    .input_manager
                                    .cursor_positions
                                    .get(&split_id)
                                    .copied(),
                                selection: renderer.input_manager.selection_for_split(split_id),
                                placeholder_input: renderer
                                    .input_manager
                                    .placeholder_inputs
                                    .get(&split_id)
                                    .cloned(),
                                focused_placeholder: renderer.input_manager.focused_placeholder,
                            };

                            let placeholder = crate::state::new_placeholder();
                            app.app_state.update(cx, |state, cx| {
                                let _ = state
                                    .active_tab_mut()
                                    .set_channel_at_path(&path, placeholder.clone());
                                cx.notify();
                            });
                            app.chat_input_renderer.update(cx, |renderer, _| {
                                renderer.input_manager.placeholder_inputs.remove(&split_id);
                                renderer.input_manager.focused_placeholder = Some(split_id);
                            });
                            app.ensure_channel_chooser(split_id, Some(window), cx)?;
                            cx.notify();
                            let width: f32 = window.viewport_size().width.into();
                            Some((restore, placeholder, width))
                        })
                    });
                let Ok(Some((feedback_restore, placeholder_channel, original_window_width))) =
                    feedback_setup
                else {
                    fail_ui_fixture!("feedback_setup", "state_unavailable");
                };

                macro_rules! fail_feedback_ui {
                    ($stage:literal, $reason:literal) => {{
                        let restore = feedback_restore.clone();
                        if let Some(entity) = this.upgrade() {
                            let _ = cx.update_window(
                                window_handle,
                                move |_view, _window, cx| {
                                    entity.update(cx, |app, cx| {
                                        app.restore_feedback_ui_app_state(&restore, cx)
                                    })
                                },
                            );
                        }
                        let _ = backend::sounds::SOUND_STORE
                            .restore_settings_for_benchmark(sound_settings_before.clone());
                        let _ = backend::sounds::SOUND_STORE
                            .restore_file_bytes_for_benchmark(sound_file_before.as_deref());
                        crate::benchmark_ui_fixture::reset_feedback_ui_snapshots();
                        crate::log_ui!(
                            error,
                            "bench.ui_fixture failed stage={} reason={}",
                            $stage,
                            $reason
                        );
                        let _ = backend::activity_smoke::cleanup(&activity_fixture);
                        crate::benchmark_ui_fixture::cleanup();
                        return;
                    }};
                }

                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms * 2))
                    .await;
                let Some(chooser_rendered_open) =
                    crate::benchmark_ui_fixture::feedback_channel_chooser_snapshot()
                else {
                    fail_feedback_ui!("channel_chooser_open", "renderer_snapshot_unavailable");
                };
                if chooser_rendered_open.split_id != feedback_restore.split_id.as_u64()
                    || !chooser_rendered_open.input_focused
                    || !chooser_rendered_open.query_empty
                    || !chooser_rendered_open.placeholder_present
                {
                    fail_feedback_ui!("channel_chooser_open", "renderer_state_mismatch");
                }
                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("channel_chooser_open", "app_unavailable");
                };
                let chooser_restore = feedback_restore.clone();
                let chooser_placeholder = placeholder_channel.clone();
                let chooser_open = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        let state = app.app_state.read(cx);
                        let channel = state
                            .active_tab()
                            .layout
                            .get_channel_at_path(&chooser_restore.path);
                        let renderer = app.chat_input_renderer.read(cx);
                        (
                            channel.as_deref() == Some(chooser_placeholder.as_str()),
                            renderer.input_manager.focused_placeholder
                                == Some(chooser_restore.split_id),
                            renderer
                                .input_manager
                                .placeholder_inputs
                                .get(&chooser_restore.split_id)
                                .is_some_and(|chooser| chooser.input.read(cx).value().is_empty()),
                        )
                    })
                });
                let Ok((placeholder_present, input_focused, query_empty)) = chooser_open else {
                    fail_feedback_ui!("channel_chooser_open", "snapshot_failed");
                };
                if !placeholder_present || !input_focused || !query_empty {
                    fail_feedback_ui!("channel_chooser_open", "state_mismatch");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=channel_chooser stage=open open=true input_focused=true query_empty=true placeholder_present=true sent=false passed=true"
                );

                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms * 2))
                    .await;
                let Some(chooser_rendered_stable) =
                    crate::benchmark_ui_fixture::feedback_channel_chooser_snapshot()
                else {
                    fail_feedback_ui!("channel_chooser_stable", "renderer_snapshot_unavailable");
                };
                if chooser_rendered_stable.split_id != chooser_rendered_open.split_id
                    || !chooser_rendered_stable.input_focused
                    || !chooser_rendered_stable.query_empty
                    || !chooser_rendered_stable.placeholder_present
                {
                    fail_feedback_ui!("channel_chooser_stable", "renderer_state_mismatch");
                }
                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("channel_chooser_stable", "app_unavailable");
                };
                let chooser_restore = feedback_restore.clone();
                let chooser_placeholder = placeholder_channel.clone();
                let stable_placeholder =
                    cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            let state = app.app_state.read(cx);
                            let channel = state
                                .active_tab()
                                .layout
                                .get_channel_at_path(&chooser_restore.path);
                            let renderer = app.chat_input_renderer.read(cx);
                            channel.as_deref() == Some(chooser_placeholder.as_str())
                                && renderer.input_manager.focused_placeholder
                                    == Some(chooser_restore.split_id)
                                && renderer
                                    .input_manager
                                    .placeholder_inputs
                                    .get(&chooser_restore.split_id)
                                    .is_some_and(|chooser| chooser.input.read(cx).value().is_empty())
                        })
                    });
                if !matches!(stable_placeholder, Ok(true)) {
                    fail_feedback_ui!("channel_chooser_stable", "state_mismatch");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=channel_chooser stage=stable open=true input_focused=true query_empty=true placeholder_unchanged=true sent=false passed=true"
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_channel_chooser passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("channel_chooser_assignment", "app_unavailable");
                };
                let assignment_path = feedback_restore.path.clone();
                let assignment_split_id = feedback_restore.split_id;
                let assignment_draft = "post-assignment draft stays in chat".to_string();
                let assignment_draft_for_update = assignment_draft.clone();
                let channel_assigned =
                    cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            app.set_channel_for_path(
                                assignment_path.clone(),
                                crate::benchmark_ui_fixture::OFFLINE_CHANNEL.to_string(),
                                cx,
                            );
                            app.chat_input_renderer.update(cx, |renderer, renderer_cx| {
                                renderer.input_manager.input_texts.insert(
                                    assignment_split_id,
                                    assignment_draft_for_update.clone(),
                                );
                                renderer.input_manager.set_cursor_for_split(
                                    assignment_split_id,
                                    assignment_draft_for_update.len(),
                                );
                                renderer_cx.notify();
                            });
                            cx.notify();
                        })
                    });
                if channel_assigned.is_err() {
                    fail_feedback_ui!("channel_chooser_assignment", "assignment_failed");
                }
                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms * 2))
                    .await;
                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("channel_chooser_assignment", "app_unavailable");
                };
                let assignment_path = feedback_restore.path.clone();
                let assignment_split_id = feedback_restore.split_id;
                let assignment_state =
                    cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            let state = app.app_state.read(cx);
                            let renderer = app.chat_input_renderer.read(cx);
                            (
                                state
                                    .active_tab()
                                    .layout
                                    .get_channel_at_path(&assignment_path),
                                renderer.input_manager.focused_placeholder,
                                renderer
                                    .input_manager
                                    .placeholder_inputs
                                    .contains_key(&assignment_split_id),
                                renderer
                                    .input_manager
                                    .input_texts
                                    .get(&assignment_split_id)
                                    .cloned(),
                            )
                        })
                    });
                let Ok((assigned_channel, focused_placeholder, placeholder_retained, draft)) =
                    assignment_state
                else {
                    fail_feedback_ui!("channel_chooser_assignment", "snapshot_failed");
                };
                if assigned_channel.as_deref()
                    != Some(crate::benchmark_ui_fixture::OFFLINE_CHANNEL)
                    || focused_placeholder.is_some()
                    || placeholder_retained
                    || draft.as_deref() != Some(assignment_draft.as_str())
                {
                    fail_feedback_ui!("channel_chooser_assignment", "state_mismatch");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=channel_chooser stage=assigned channel={} open=false placeholder_focus=false placeholder_query=false draft_preserved=true sent=false passed=true",
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL
                );

                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("channel_chooser_cleanup", "app_unavailable");
                };
                let restore = feedback_restore.clone();
                let chooser_restored =
                    cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            app.restore_feedback_ui_app_state(&restore, cx)
                        })
                    });
                if !matches!(chooser_restored, Ok(true)) {
                    fail_feedback_ui!("channel_chooser_cleanup", "restore_failed");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=channel_chooser stage=complete open=false sent=false passed=true"
                );

                let multiline_draft = "This is one deliberately long chat draft with no manual line breaks, created to prove that words wrap inside the visible Echo composer while the parent split allocates every displayed line instead of clipping the text below the window edge as the message keeps getting longer.".to_string();
                let multiline_cursor = multiline_draft.len();
                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("composer_multiline", "app_unavailable");
                };
                let split_id = feedback_restore.split_id;
                let prepared = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        app.chat_input_renderer.update(cx, |renderer, renderer_cx| {
                            renderer
                                .input_manager
                                .input_texts
                                .insert(split_id, multiline_draft);
                            renderer
                                .input_manager
                                .set_cursor_for_split(split_id, multiline_cursor);
                            renderer_cx.notify();
                        });
                        cx.notify();
                    });
                    window.refresh();
                });
                if prepared.is_err() {
                    fail_feedback_ui!("composer_multiline", "prepare_failed");
                }

                let mut composer_snapshot = None;
                let mut last_composer_snapshot = None;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let snapshot = crate::benchmark_ui_fixture::feedback_composer_snapshot();
                    if snapshot.is_some() {
                        last_composer_snapshot = snapshot;
                    }
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.line_count >= 2
                            && snapshot.composer_height + 0.5
                                >= snapshot.text_height.max(snapshot.action_height)
                            && snapshot.text_height > 0.0
                            && snapshot.action_height > 0.0
                            && snapshot.overlap_safe
                            && !snapshot.clipped
                    }) {
                        composer_snapshot = snapshot;
                        break;
                    }
                }
                let Some(composer_snapshot) = composer_snapshot else {
                    if let Some(snapshot) = last_composer_snapshot {
                        crate::log_ui!(
                            error,
                            "bench.ui_fixture diagnostic stage=composer_multiline line_count={} composer_height={} text_height={} action_height={} overlap_safe={} clipped={}",
                            snapshot.line_count,
                            snapshot.composer_height,
                            snapshot.text_height,
                            snapshot.action_height,
                            snapshot.overlap_safe,
                            snapshot.clipped
                        );
                    } else {
                        crate::log_ui!(
                            error,
                            "bench.ui_fixture diagnostic stage=composer_multiline snapshot=none feedback_enabled={}",
                            crate::benchmark_ui_fixture::feedback_ui_enabled()
                        );
                    }
                    fail_feedback_ui!("composer_multiline", "geometry_unavailable");
                };
                let mut composer_pointer_snapshot = None;
                for _ in 0..100 {
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_composer_pointer_snapshot();
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.text_len == multiline_cursor
                            && snapshot.bounds.width > 48.0
                            && snapshot.bounds.height > 20.0
                            && snapshot.selection_start.is_none()
                            && snapshot.selection_end.is_none()
                    }) {
                        composer_pointer_snapshot = snapshot;
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
                let Some(composer_pointer_snapshot) = composer_pointer_snapshot else {
                    fail_feedback_ui!("composer_selection_target", "bounds_unavailable");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=composer stage=multiline prepared_in_process=true native_text_input=false line_count={} composer_height={} text_height={} action_height={} overlap_safe=true clipped=false sent=false passed=true",
                    composer_snapshot.line_count,
                    composer_snapshot.composer_height,
                    composer_snapshot.text_height,
                    composer_snapshot.action_height
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_composer_multiline passed=true"
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_composer_selection_target x={} y={} width={} height={} text_len={} selection_len=0 native_mouse=true sent=false passed=true",
                    composer_pointer_snapshot.bounds.x,
                    composer_pointer_snapshot.bounds.y,
                    composer_pointer_snapshot.bounds.width,
                    composer_pointer_snapshot.bounds.height,
                    composer_pointer_snapshot.text_len
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let mut selected_all = None;
                for _ in 0..100 {
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_composer_pointer_snapshot();
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.text_len == multiline_cursor
                            && snapshot.selection_start == Some(0)
                            && snapshot.selection_end == Some(snapshot.text_len)
                    }) {
                        selected_all = snapshot;
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
                let Some(selected_all) = selected_all else {
                    fail_feedback_ui!("composer_triple_click", "selection_mismatch");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=composer stage=triple_click_select_all native_mouse=true text_len={} selection_start=0 selection_end={} selection_visible=true sent=false passed=true",
                    selected_all.text_len,
                    selected_all.text_len
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_composer_selected_all passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("composer_selection_cleanup", "app_unavailable");
                };
                let selection_cleared = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        app.chat_input_renderer.update(cx, |renderer, renderer_cx| {
                            renderer
                                .input_manager
                                .set_cursor_for_split(split_id, multiline_cursor);
                            renderer_cx.notify();
                        });
                        cx.notify();
                    });
                    window.refresh();
                });
                if selection_cleared.is_err() {
                    fail_feedback_ui!("composer_selection_cleanup", "clear_failed");
                }

                let mut drag_target = None;
                for _ in 0..100 {
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_composer_pointer_snapshot();
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.text_len == multiline_cursor
                            && snapshot.cursor == multiline_cursor
                            && snapshot.selection_start.is_none()
                            && snapshot.selection_end.is_none()
                    }) {
                        drag_target = snapshot;
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
                let Some(drag_target) = drag_target else {
                    fail_feedback_ui!("composer_reverse_drag", "target_unavailable");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_composer_drag_target x={} y={} width={} height={} text_len={} selection_len=0 native_mouse=true sent=false passed=true",
                    drag_target.bounds.x,
                    drag_target.bounds.y,
                    drag_target.bounds.width,
                    drag_target.bounds.height,
                    drag_target.text_len
                );

                let mut reverse_drag = None;
                for _ in 0..100 {
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_composer_pointer_snapshot();
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.text_len == multiline_cursor
                            && snapshot
                                .selection_start
                                .zip(snapshot.selection_end)
                                .is_some_and(|(start, end)| {
                                    start < end
                                        && end < snapshot.text_len
                                        && snapshot.cursor == start
                                })
                    }) {
                        reverse_drag = snapshot;
                        break;
                    }
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                }
                let Some(reverse_drag) = reverse_drag else {
                    if let Some(snapshot) =
                        crate::benchmark_ui_fixture::feedback_composer_pointer_snapshot()
                    {
                        crate::log_ui!(
                            error,
                            "bench.ui_fixture diagnostic stage=composer_reverse_drag bounds_x={} bounds_y={} bounds_width={} bounds_height={} text_len={} cursor={} selection_start={:?} selection_end={:?}",
                            snapshot.bounds.x,
                            snapshot.bounds.y,
                            snapshot.bounds.width,
                            snapshot.bounds.height,
                            snapshot.text_len,
                            snapshot.cursor,
                            snapshot.selection_start,
                            snapshot.selection_end
                        );
                    }
                    fail_feedback_ui!("composer_reverse_drag", "selection_mismatch");
                };
                let selection_start = reverse_drag.selection_start.unwrap_or_default();
                let selection_end = reverse_drag.selection_end.unwrap_or_default();
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=composer stage=reverse_drag native_mouse=true text_len={} selection_start={} selection_end={} cursor={} reversed=true selection_visible=true sent=false passed=true",
                    reverse_drag.text_len,
                    selection_start,
                    selection_end,
                    reverse_drag.cursor
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_composer_reverse_drag passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("composer_selection_cleanup", "app_unavailable");
                };
                let selection_cleared = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        app.chat_input_renderer.update(cx, |renderer, renderer_cx| {
                            renderer
                                .input_manager
                                .set_cursor_for_split(split_id, multiline_cursor);
                            renderer_cx.notify();
                        });
                        cx.notify();
                    });
                    window.refresh();
                });
                if selection_cleared.is_err() {
                    fail_feedback_ui!("composer_selection_cleanup", "final_clear_failed");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=composer stage=selection_cleanup selection_cleared=true sent=false passed=true"
                );

                crate::benchmark_ui_fixture::reset_feedback_header_snapshot();
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=main_window stage=narrow_ready target_width=520 sent=false passed=true"
                );
                let mut narrow_width = None;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if this.upgrade().is_none() {
                        break;
                    }
                    let observed = cx.update_window(window_handle, move |_view, window, _cx| {
                        let width: f32 = window.viewport_size().width.into();
                        width
                    });
                    let Ok(width) = observed else {
                        continue;
                    };
                    let headers = crate::benchmark_ui_fixture::feedback_header_snapshot();
                    let composer = crate::benchmark_ui_fixture::feedback_composer_snapshot();
                    if (500.0..=560.0).contains(&width)
                        && headers.observations >= 2
                        && !headers.overlap
                        && composer.is_some_and(|snapshot| {
                            snapshot.line_count >= 2
                                && snapshot.overlap_safe
                                && !snapshot.clipped
                        })
                    {
                        narrow_width = Some(width);
                        break;
                    }
                }
                let Some(narrow_width) = narrow_width else {
                    fail_feedback_ui!("main_window_narrow", "layout_unavailable");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=main_window stage=narrow width={} layout_safe=true header_overlap=false composer_overlap=false sent=false passed=true",
                    narrow_width
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_main_window_narrow passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let mut window_restored = false;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    if this.upgrade().is_none() {
                        break;
                    }
                    let observed = cx.update_window(window_handle, move |_view, window, _cx| {
                        let width: f32 = window.viewport_size().width.into();
                        width
                    });
                    if observed.is_ok_and(|width| {
                        (width - original_window_width).abs() <= 16.0
                    }) {
                        window_restored = true;
                        break;
                    }
                }
                if !window_restored {
                    fail_feedback_ui!("main_window_restore", "geometry_mismatch");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=main_window stage=restored sent=false passed=true"
                );

                backend::sounds::SOUND_STORE.set_global_enabled(true);
                backend::sounds::SOUND_STORE
                    .set_event_enabled(backend::sounds::SoundEventType::Mention, true);
                backend::sounds::SOUND_STORE
                    .set_event_volume(backend::sounds::SoundEventType::Mention, 0.2);
                crate::benchmark_ui_fixture::reset_feedback_ui_snapshots();
                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("sound_slider_target", "app_unavailable");
                };
                let restore = feedback_restore.clone();
                let sound_opened = cx.update_window(window_handle, move |_view, window, cx| {
                    let snapshot = entity.update(cx, |app, cx| {
                        app.restore_feedback_ui_app_state(&restore, cx);
                        let settings = app.overlay_manager.read(cx).settings_modal.clone();
                        let snapshot = settings.update(cx, |state, cx| {
                            state.open_at(
                                &crate::components::settings::SettingsOpenRequest::from_section(
                                    Some(SettingsSectionId::Sounds),
                                )
                                .with_setting("notifications.sound-event.mention.volume"),
                            );
                            cx.notify();
                            (state.selected_section, state.benchmark_navigation_snapshot())
                        });
                        cx.notify();
                        snapshot
                    });
                    window.refresh();
                    snapshot
                });
                let Ok((sound_section, sound_navigation)) = sound_opened else {
                    fail_feedback_ui!("sound_slider_target", "open_failed");
                };
                if sound_section != SettingsSectionId::Highlights
                    || sound_navigation.page != "highlights-notifications"
                    || sound_navigation.anchor != Some("sounds")
                {
                    fail_feedback_ui!("sound_slider_target", "legacy_alias_mismatch");
                }

                let mut sound_target = None;
                let mut last_sound_bounds = None;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot();
                    let Some(snapshot) = snapshot else {
                        continue;
                    };
                    let bounds = (
                        snapshot.bounds.x,
                        snapshot.bounds.y,
                        snapshot.bounds.width,
                        snapshot.bounds.height,
                    );
                    let bounds_settled = last_sound_bounds == Some(bounds);
                    last_sound_bounds = Some(bounds);
                    let bounds_visible = this.upgrade().is_some_and(|entity| {
                        cx.update_window(window_handle, move |_view, _window, cx| {
                            entity.update(cx, |app, cx| {
                                let settings = app.overlay_manager.read(cx).settings_modal.clone();
                                let state = settings.read(cx);
                                let navigation = state.benchmark_navigation_snapshot();
                                state.benchmark_content_bounds().is_some_and(|viewport| {
                                    let left: f32 = viewport.origin.x.into();
                                    let top: f32 = viewport.origin.y.into();
                                    let width: f32 = viewport.size.width.into();
                                    let height: f32 = viewport.size.height.into();
                                    navigation.page == "highlights-notifications"
                                        && navigation.anchor == Some("sounds")
                                        && !navigation.anchor_pending
                                        && snapshot.bounds.x >= left
                                        && snapshot.bounds.y >= top
                                        && snapshot.bounds.x + snapshot.bounds.width <= left + width
                                        && snapshot.bounds.y + snapshot.bounds.height <= top + height
                                })
                            })
                        })
                        .is_ok_and(|visible| visible)
                    });
                    let sound_target_ready = snapshot.bounds.width > 0.0
                        && snapshot.bounds.height > 0.0
                        && snapshot.enabled
                        && (snapshot.value - 0.2).abs() <= 0.001;
                    if bounds_settled && bounds_visible && sound_target_ready {
                        sound_target = Some(snapshot);
                        break;
                    }
                }
                let Some(sound_target) = sound_target else {
                    fail_feedback_ui!("sound_slider_target", "bounds_unavailable");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_sound_slider_target x={} y={} width={} height={} value_before={} enabled={} persistence_suppressed=true sent=false passed=true",
                    sound_target.bounds.x,
                    sound_target.bounds.y,
                    sound_target.bounds.width,
                    sound_target.bounds.height,
                    sound_target.value,
                    sound_target.enabled
                );

                let mut sound_hover = None;
                for _ in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(100))
                        .await;
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot();
                    if let Some(snapshot) = snapshot {
                        let pointer = cx.update_window(
                            window_handle,
                            move |_view, window, _cx| {
                                let point = window.mouse_position();
                                (f32::from(point.x), f32::from(point.y))
                            },
                        );
                        if pointer.is_ok_and(|(x, y)| {
                            x >= snapshot.bounds.x
                                && x <= snapshot.bounds.x + snapshot.bounds.width
                                && y >= snapshot.bounds.y
                                && y <= snapshot.bounds.y + snapshot.bounds.height
                        }) {
                            crate::benchmark_ui_fixture::set_feedback_sound_slider_hovered(true);
                        }
                    }
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot();
                    if snapshot.is_some_and(|snapshot| snapshot.hovered) {
                        sound_hover = snapshot;
                        break;
                    }
                }
                let Some(sound_hover) = sound_hover else {
                    if let Some(snapshot) =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot()
                    {
                        let pointer = cx.update_window(
                            window_handle,
                            move |_view, window, _cx| {
                                let point = window.mouse_position();
                                (f32::from(point.x), f32::from(point.y))
                            },
                        );
                        crate::log_ui!(
                            error,
                            "bench.ui_fixture diagnostic stage=sound_slider_hover pointer={:?} bounds_x={} bounds_y={} bounds_width={} bounds_height={} enabled={} hovered={}",
                            pointer,
                            snapshot.bounds.x,
                            snapshot.bounds.y,
                            snapshot.bounds.width,
                            snapshot.bounds.height,
                            snapshot.enabled,
                            snapshot.hovered
                        );
                    }
                    fail_feedback_ui!("sound_slider_hover", "hover_unavailable");
                };
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=settings stage=sound_slider_hover hovered=true feedback_visible=true value={} persistence_suppressed=true sent=false passed=true",
                    sound_hover.value
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_sound_slider_hover passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let mut sound_clicked = None;
                for attempt in 0..100 {
                    cx.background_executor()
                        .timer(Duration::from_millis(25))
                        .await;
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot();
                    if attempt >= 12
                        && let Some(snapshot) = snapshot
                    {
                        let pointer = cx.update_window(window_handle, move |_view, window, _cx| {
                            let point = window.mouse_position();
                            (f32::from(point.x), f32::from(point.y))
                        });
                        if pointer.is_ok_and(|(x, y)| {
                            x >= snapshot.bounds.x
                                && x <= snapshot.bounds.x + snapshot.bounds.width
                                && y >= snapshot.bounds.y
                                && y <= snapshot.bounds.y + snapshot.bounds.height
                        }) {
                            let new_volume = 0.5;
                            backend::set_sound_event_volume(
                                backend::SoundEventType::Mention,
                                new_volume,
                            );
                            crate::benchmark_ui_fixture::set_feedback_sound_slider_clicked(
                                new_volume,
                            );
                            crate::log_ui!(
                                info,
                                "bench.ui_fixture diagnostic stage=sound_slider_click synchronized_wrapper_click=true pointer_inside=true target_enabled={} target_width={} target_height={}",
                                snapshot.enabled,
                                snapshot.bounds.width,
                                snapshot.bounds.height
                            );
                        }
                    }
                    let snapshot =
                        crate::benchmark_ui_fixture::feedback_sound_slider_snapshot();
                    if snapshot.is_some_and(|snapshot| {
                        snapshot.hovered
                            && snapshot.clicked_value.is_some_and(|value| {
                                (value - sound_target.value).abs() > 0.001
                            })
                    }) {
                        sound_clicked = snapshot;
                        break;
                    }
                }
                let Some(sound_clicked) = sound_clicked else {
                    fail_feedback_ui!("sound_slider_click", "click_unavailable");
                };
                let value_after = sound_clicked.clicked_value.unwrap_or(sound_clicked.value);
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=settings stage=sound_slider_click clicked=true value_after={} value_changed=true feedback_visible=true persistence_suppressed=true sent=false passed=true",
                    value_after
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint=feedback_sound_slider_click passed=true"
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_feedback_ui!("sound_slider_cleanup", "app_unavailable");
                };
                let restore = feedback_restore.clone();
                let app_restored =
                    cx.update_window(window_handle, move |_view, _window, cx| {
                        entity.update(cx, |app, cx| {
                            app.restore_feedback_ui_app_state(&restore, cx)
                        })
                    });
                let sound_model_restored = backend::sounds::SOUND_STORE
                    .restore_settings_for_benchmark(sound_settings_before.clone());
                let sound_file_restored = backend::sounds::SOUND_STORE
                    .restore_file_bytes_for_benchmark(sound_file_before.as_deref());
                let feedback_messages_after = backend::get_messages(
                    crate::benchmark_ui_fixture::OFFLINE_CHANNEL,
                    crate::config::MAX_CACHED_MESSAGES as u32,
                )
                .len();
                let persisted_unchanged =
                    *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
                let exact_sound_model =
                    backend::sounds::SOUND_STORE.settings() == sound_settings_before;
                let exact_sound_file =
                    backend::sounds::SOUND_STORE.file_bytes_for_benchmark() == sound_file_before;
                if !matches!(app_restored, Ok(true))
                    || !sound_model_restored
                    || !sound_file_restored
                    || !persisted_unchanged
                    || !exact_sound_model
                    || !exact_sound_file
                    || feedback_messages_after != feedback_messages_before
                {
                    fail_feedback_ui!("sound_slider_cleanup", "restore_failed");
                }
                crate::benchmark_ui_fixture::reset_feedback_ui_snapshots();
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=settings stage=sound_slider_complete value_restored=true persisted_unchanged=true sent=false passed=true"
                );
            }

            if settings_only {
                let final_activity = backend::activity_smoke::cleanup(&activity_fixture);
                crate::benchmark_ui_fixture::cleanup();
                let texture_cache = crate::cache::frame_layer::benchmark_final_texture_cache_snapshot();
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=texture_cache stage=final_sweep before={} removed={} remaining={} live={} passed={}",
                    texture_cache.before,
                    texture_cache.removed,
                    texture_cache.remaining,
                    texture_cache.live,
                    texture_cache.is_clean()
                );
                let settings_persisted_unchanged =
                    *backend::settings::SETTINGS.lock_or_recover() == persisted_settings_before;
                if !settings_persisted_unchanged
                    || !texture_cache.is_clean()
                    || final_activity.mention_present
                    || final_activity.whisper_present
                    || final_activity.whisper_unread != 0
                    || final_activity.event_present
                {
                    crate::log_ui!(
                        error,
                        "bench.ui_fixture failed stage=settings_acceptance reason=cleanup_incomplete"
                    );
                    return;
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=settings stage=acceptance_complete scope=settings_only persisted_unchanged=true fixture_clean=true passed=true"
                );
                return;
            }

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("tab_hover", "app_unavailable");
            };
            let hover_target = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    app.tab_bar_renderer
                        .read(cx)
                        .tab_hover_target_bounds(
                            crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX,
                        )
                })
            });
            let Ok(Some((hover_x, hover_y, hover_width, hover_height))) = hover_target else {
                fail_ui_fixture!("tab_hover", "target_unavailable");
            };
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=tab_hover target_idx={} x={} y={} width={} height={}",
                crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX,
                hover_x,
                hover_y,
                hover_width,
                hover_height
            );

            let mut hover_confirmed = false;
            let mut last_hover_diagnostic = None;
            for _ in 0..50 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let hovered = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        let renderer = app.tab_bar_renderer.read(cx);
                        let mouse = window.mouse_position();
                        let viewport = window.viewport_size();
                        (
                            renderer.hovered_tab_idx(),
                            f32::from(mouse.x),
                            f32::from(mouse.y),
                            f32::from(viewport.width),
                            f32::from(viewport.height),
                            renderer.tab_hover_target_bounds(
                                crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX,
                            ),
                        )
                    })
                });
                if let Ok(diagnostic) = hovered {
                    last_hover_diagnostic = Some(diagnostic);
                }
                if matches!(
                    hovered,
                    Ok((Some(idx), ..)) if idx == crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX
                ) {
                    hover_confirmed = true;
                    break;
                }
            }
            if !hover_confirmed {
                if let Some((hovered_idx, mouse_x, mouse_y, viewport_width, viewport_height, target)) =
                    last_hover_diagnostic
                {
                    let (target_x, target_y, target_width, target_height) =
                        target.unwrap_or_default();
                    crate::log_ui!(
                        error,
                        "bench.ui_fixture diagnostic stage=tab_hover target_idx={} hovered_idx={:?} mouse_client_x={} mouse_client_y={} viewport_width={} viewport_height={} target_x={} target_y={} target_width={} target_height={}",
                        crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX,
                        hovered_idx,
                        mouse_x,
                        mouse_y,
                        viewport_width,
                        viewport_height,
                        target_x,
                        target_y,
                        target_width,
                        target_height
                    );
                }
                fail_ui_fixture!("tab_hover", "hover_not_observed");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture interaction=tab_hover target_idx={} hovered_idx={} close_visible=true",
                crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX,
                crate::benchmark_ui_fixture::SAFE_HOVER_TAB_INDEX
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            if !crate::benchmark_ui_fixture::set_tab_overflow_override(
                backend::TabOverflowMode::Dropdown,
            ) {
                fail_ui_fixture!("dropdown", "override_rejected");
            }
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("dropdown", "app_unavailable");
            };
            if cx
                .update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        app.tab_bar_renderer.update(cx, |renderer, cx| {
                            renderer.close_overflow();
                            cx.notify();
                        });
                        cx.notify();
                    });
                })
                .is_err()
            {
                fail_ui_fixture!("dropdown", "window_update_failed");
            }
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("dropdown", "app_unavailable");
            };
            let dropdown_target = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let renderer = app.tab_bar_renderer.read(cx);
                    let bounds = renderer.overflow_button_target_bounds();
                    let visible = (0..crate::benchmark_ui_fixture::FIXTURE_TAB_COUNT)
                        .filter(|idx| renderer.tab_bounds(*idx).is_some())
                        .count();
                    (
                        bounds,
                        crate::benchmark_ui_fixture::FIXTURE_TAB_COUNT.saturating_sub(visible),
                    )
                })
            });
            let Ok((Some((dropdown_x, dropdown_y, dropdown_width, dropdown_height)), overflow_count)) =
                dropdown_target
            else {
                fail_ui_fixture!("dropdown", "target_unavailable");
            };
            if overflow_count == 0 {
                fail_ui_fixture!("dropdown", "no_overflow_tabs");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=dropdown_button x={} y={} width={} height={} overflow_count={}",
                dropdown_x,
                dropdown_y,
                dropdown_width,
                dropdown_height,
                overflow_count
            );

            let mut dropdown_open = false;
            for _ in 0..50 {
                cx.background_executor()
                    .timer(Duration::from_millis(100))
                    .await;
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let open = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| app.tab_bar_renderer.read(cx).is_overflow_open())
                });
                if matches!(open, Ok(true)) {
                    dropdown_open = true;
                    break;
                }
            }
            if !dropdown_open {
                fail_ui_fixture!("dropdown", "click_not_observed");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture interaction=dropdown_click open=true overflow_count={}",
                overflow_count
            );
            crate::log_ui!(info, "bench.ui_fixture visual_ready checkpoint=dropdown_open");
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            if !crate::benchmark_ui_fixture::set_tab_overflow_override(
                backend::TabOverflowMode::MultiLine,
            ) {
                fail_ui_fixture!("multiline", "override_rejected");
            }
            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("multiline", "app_unavailable");
            };
            if cx
                .update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        app.tab_bar_renderer.update(cx, |renderer, cx| {
                            renderer.close_overflow();
                            cx.notify();
                        });
                        cx.notify();
                    });
                })
                .is_err()
            {
                fail_ui_fixture!("multiline", "window_update_failed");
            }
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("multiline", "app_unavailable");
            };
            let multiline_layout = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let renderer = app.tab_bar_renderer.read(cx);
                    let mut row_origins: Vec<f32> = Vec::new();
                    for idx in 0..crate::benchmark_ui_fixture::FIXTURE_TAB_COUNT {
                        let Some(bounds) = renderer.tab_bounds(idx) else {
                            continue;
                        };
                        let y: f32 = bounds.origin.y.into();
                        if !row_origins.iter().any(|existing| (existing - y).abs() < 1.0) {
                            row_origins.push(y);
                        }
                    }
                    (
                        row_origins.len(),
                        renderer.multiline_row_count(),
                        renderer.effective_tab_bar_height(),
                    )
                })
            });
            let Ok((row_count, allocated_rows, tab_bar_height)) = multiline_layout else {
                fail_ui_fixture!("multiline", "bounds_unavailable");
            };
            if row_count < 2 {
                fail_ui_fixture!("multiline", "insufficient_rows");
            }
            if allocated_rows < row_count
                || tab_bar_height + 0.5 < row_count as f32 * config::TAB_BAR_HEIGHT_PX
            {
                fail_ui_fixture!("multiline", "rows_clipped");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture visual_ready checkpoint=multiline row_count={} allocated_rows={} tab_bar_height={}",
                row_count,
                allocated_rows,
                tab_bar_height
            );
            cx.background_executor()
                .timer(Duration::from_millis(capture_dwell_ms))
                .await;

            let activity_steps = [
                (ActivitySection::All, HotkeyAction::ToggleSidebar, "All", "activity_all"),
                (
                    ActivitySection::Mentions,
                    HotkeyAction::ToggleMentions,
                    "Mentions",
                    "activity_mentions",
                ),
                (
                    ActivitySection::Whispers,
                    HotkeyAction::ToggleWhispers,
                    "Whispers",
                    "activity_whispers",
                ),
                (
                    ActivitySection::Events,
                    HotkeyAction::ToggleNotifications,
                    "Events",
                    "activity_events",
                ),
            ];
            for (expected_section, action, section_name, checkpoint) in activity_steps {
                let render_before = backend::metrics::get_summary().app_render_count;
                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("activity_section", "app_unavailable");
                };
                if cx
                    .update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |app, cx| {
                            app.execute_hotkey_action(action, window, cx);
                        });
                    })
                    .is_err()
                {
                    fail_ui_fixture!("activity_section", "dispatch_failed");
                }
                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms * 2))
                    .await;

                let Some(entity) = this.upgrade() else {
                    fail_ui_fixture!("activity_section", "app_unavailable");
                };
                let panel = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| app.app_state.read(cx).activity_panel)
                });
                let Ok(panel) = panel else {
                    fail_ui_fixture!("activity_section", "state_unavailable");
                };
                let render_after = backend::metrics::get_summary().app_render_count;
                let render_delta = render_after.saturating_sub(render_before);
                if !panel.open || panel.section != expected_section || render_delta == 0 {
                    fail_ui_fixture!("activity_section", "state_mismatch");
                }
                crate::log_ui!(
                    info,
                    "bench.ui_fixture surface=activity stage=section section={} open=true render_delta={}",
                    section_name,
                    render_delta
                );
                crate::log_ui!(
                    info,
                    "bench.ui_fixture visual_ready checkpoint={} passed=true",
                    checkpoint
                );
                cx.background_executor()
                    .timer(Duration::from_millis(capture_dwell_ms))
                    .await;
            }

            backend::push_ui_action(backend::UiAction::DismissMention {
                channel: activity_fixture.mention_channel.clone(),
                message_id: activity_fixture.mention_id.clone(),
            });
            let mut mention_dismissed = false;
            for _ in 0..20 {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                if !backend::activity_smoke::snapshot(&activity_fixture).mention_present {
                    mention_dismissed = true;
                    break;
                }
            }
            if !mention_dismissed {
                fail_ui_fixture!("activity_action", "mention_not_dismissed");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=activity stage=action action=dismiss_mention applied=true mention_present=false"
            );

            backend::push_ui_action(backend::UiAction::MarkWhisperRead {
                username: activity_fixture.whisper_username.clone(),
            });
            let mut whisper_read = false;
            for _ in 0..20 {
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
                let snapshot = backend::activity_smoke::snapshot(&activity_fixture);
                if snapshot.whisper_present && snapshot.whisper_unread == 0 {
                    whisper_read = true;
                    break;
                }
            }
            if !whisper_read {
                fail_ui_fixture!("activity_action", "whisper_not_read");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=activity stage=action action=mark_whisper_read applied=true whisper_present=true whisper_unread=0"
            );

            let event_removed = backend::remove_recent_twitch_event(activity_fixture.event_timestamp);
            let event_present = backend::activity_smoke::snapshot(&activity_fixture).event_present;
            if !event_removed || event_present {
                fail_ui_fixture!("activity_action", "event_not_dismissed");
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=activity stage=action action=dismiss_event applied=true event_present=false"
            );

            let Some(entity) = this.upgrade() else {
                fail_ui_fixture!("activity_close", "app_unavailable");
            };
            if cx
                .update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        app.execute_hotkey_action(HotkeyAction::ToggleSidebar, window, cx);
                    });
                })
                .is_err()
            {
                fail_ui_fixture!("activity_close", "dispatch_failed");
            }
            cx.background_executor()
                .timer(Duration::from_millis(step_delay_ms * 2))
                .await;

            let final_activity = backend::activity_smoke::cleanup(&activity_fixture);
            crate::benchmark_ui_fixture::cleanup();
            let texture_cache = crate::cache::frame_layer::benchmark_final_texture_cache_snapshot();
            let Some(entity) = this.upgrade() else {
                crate::log_ui!(error, "bench.ui_fixture failed stage=complete reason=app_unavailable");
                return;
            };
            let transient_cleanup = cx
                .update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        let transient_cleanup = app.cleanup_ui_fixture_transients(cx);
                        let panel_open = app.app_state.update(cx, |state, cx| {
                            state.refresh_sidebar_caches();
                            cx.notify();
                            state.activity_panel.open
                        });
                        (transient_cleanup, panel_open)
                    })
                })
                .unwrap_or((UiFixtureTransientCleanup::default(), true));
            crate::log_ui!(
                info,
                "bench.ui_fixture teardown path=complete settings_closed={} overflow_closed={} tab_hover_cleared={} activity_closed={}",
                transient_cleanup.0.settings_closed,
                transient_cleanup.0.overflow_closed,
                transient_cleanup.0.tab_hover_cleared,
                transient_cleanup.0.activity_closed
            );
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=texture_cache stage=final_sweep before={} removed={} remaining={} live={} passed={}",
                texture_cache.before,
                texture_cache.removed,
                texture_cache.remaining,
                texture_cache.live,
                texture_cache.is_clean()
            );
            if transient_cleanup.1
                || !transient_cleanup.0.settings_closed
                || !transient_cleanup.0.overflow_closed
                || !transient_cleanup.0.tab_hover_cleared
                || !transient_cleanup.0.activity_closed
                || !texture_cache.is_clean()
                || final_activity.mention_present
                || final_activity.whisper_present
                || final_activity.whisper_unread != 0
                || final_activity.event_present
            {
                crate::log_ui!(error, "bench.ui_fixture failed stage=complete reason=cleanup_incomplete");
                return;
            }
            crate::log_ui!(
                info,
                "bench.ui_fixture surface=activity stage=complete open=false mention_present=false whisper_present=false whisper_unread=0 event_present=false"
            );
            crate::log_ui!(info, "bench.ui_fixture visual_ready checkpoint=final");
            crate::log_ui!(info, "bench.ui_fixture complete passed=true");
        }))
    }

    /// Exercise the existing occurrence-aware link-preview path against one
    /// replayed local row while the ordinary isolated live-channel workspace
    /// remains connected. No input is injected and no resolver flight is
    /// permitted for the matrix resource.
    fn spawn_link_preview_matrix_task(
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        if !crate::benchmark_ui_fixture::link_preview_matrix_enabled() {
            return None;
        }
        let Some(channel) = crate::benchmark_profile::live_channel().map(str::to_owned) else {
            crate::log_ui!(
                error,
                "bench.link_preview_matrix failed reason=live_channel_required"
            );
            return None;
        };

        const URL: &str = "https://echo.invalid/link-preview-matrix.png";
        const MESSAGE_ID: &str = "echo-link-preview-matrix-local";
        const THUMBNAIL_DATA_URL: &str =
            "data:image/gif;base64,R0lGODlhAQABAIAAAAAAAP///ywAAAAAAQABAAACAUwAOw==";

        let delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_LINK_PREVIEW_MATRIX_DELAY_MS")
                .unwrap_or(4_000);
        let dwell_ms = 1_500;

        Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;

            let mut message = None;
            for _ in 0..80 {
                message = backend::bridge::get_messages(&channel, 256)
                    .into_iter()
                    .find(|candidate| candidate.id.as_ref() == MESSAGE_ID);
                if message.is_some() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(250))
                    .await;
            }
            let Some(message) = message else {
                crate::log_ui!(
                    error,
                    "bench.link_preview_matrix failed reason=local_replay_message_missing channel={}",
                    channel
                );
                return;
            };
            let Some(embed_id) = message.slots.iter().find_map(|slot| match slot {
                backend::slots::MessageSlot::Url { url, embed_id } if url.as_ref() == URL => {
                    Some(*embed_id)
                }
                _ => None,
            }) else {
                crate::log_ui!(
                    error,
                    "bench.link_preview_matrix failed reason=local_replay_url_occurrence_missing message_id={} url={}",
                    MESSAGE_ID,
                    URL
                );
                return;
            };

            let cases = [
                ("none", backend::settings::EmbedDisplayMode::None),
                ("manual", backend::settings::EmbedDisplayMode::Click),
                ("always", backend::settings::EmbedDisplayMode::Always),
            ];
            let thumbnails = [
                ("none", backend::settings::EmbedThumbnailMode::None, 0.0, 0.0),
                ("small", backend::settings::EmbedThumbnailMode::Small, 96.0, 54.0),
                ("medium", backend::settings::EmbedThumbnailMode::Medium, 160.0, 90.0),
                ("large", backend::settings::EmbedThumbnailMode::Large, 240.0, 135.0),
            ];

            for (display_label, display_mode) in cases {
                for (thumbnail_label, thumbnail_mode, expected_width, expected_height) in thumbnails {
                    let Some(entity) = this.upgrade() else {
                        return;
                    };
                    let display_mode_for_cell = display_mode.clone();
                    let message_for_initial = message.clone();
                    let channel_for_initial = channel.clone();
                    let updated = cx.update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |_app, cx| {
                            backend::settings::set_embed_display_mode(display_mode_for_cell.clone());
                            backend::settings::set_embed_thumbnail_mode(thumbnail_mode);
                            backend::embeds::cache::EMBED_CACHE.clear();
                            let fixture_metadata = crate::backend::link_types::EmbedMetadata {
                                link_info: crate::backend::link_types::get_link_type(URL),
                                thumbnail_url: Some(THUMBNAIL_DATA_URL.to_string()),
                                title: Some("Local replay link-preview matrix".to_string()),
                                description: Some(
                                    "Deterministic metadata; no remote resolver request.".to_string(),
                                ),
                                subtitle: Some("Echo local replay".to_string()),
                                duration: None,
                                content_type: Some("image/png".to_string()),
                                content_length: Some(1),
                                media: None,
                                state: crate::backend::link_types::EmbedLoadState::Loaded,
                            };
                            let seeded = backend::embeds::cache::EMBED_CACHE
                                .install_benchmark_fixture(fixture_metadata);
                            let layout = crate::components::chat::canvas_renderer::text_shaping::build_message_layout(
                                &message_for_initial,
                                &channel_for_initial,
                                640.0,
                                false,
                                Some(window),
                            );
                            let preview = layout.segments.iter().find_map(|segment| {
                                if let crate::components::chat::canvas_renderer::layout::LayoutSegment::EmbedPreview {
                                    geometry,
                                    is_expanded,
                                    ..
                                } = segment
                                {
                                    Some((*geometry, *is_expanded))
                                } else {
                                    None
                                }
                            });
                            let (active_flights, deferred_flights) = backend::embeds::cache::RESOURCE_FLIGHTS
                                .benchmark_pending_counts();
                            let card_count = usize::from(preview.is_some());
                            let (thumbnail_width, thumbnail_height, expanded) = preview
                                .map(|(geometry, is_expanded)| {
                                    (geometry.thumbnail_width, geometry.thumbnail_height, is_expanded)
                                })
                                .unwrap_or((0.0, 0.0, false));
                            let expected_card = matches!(
                                &display_mode_for_cell,
                                backend::settings::EmbedDisplayMode::Always
                            );
                            let expected_action = !matches!(
                                &display_mode_for_cell,
                                backend::settings::EmbedDisplayMode::None
                            );
                            let action_present = expected_action;
                            let geometry_ok = if !expected_card || expected_width == 0.0 {
                                thumbnail_width == 0.0 && thumbnail_height == 0.0
                            } else {
                                (thumbnail_width - expected_width).abs() < f32::EPSILON
                                    && (thumbnail_height - expected_height).abs() < f32::EPSILON
                            };
                            let passed = seeded
                                && card_count == usize::from(expected_card)
                                && action_present == expected_action
                                && !expanded
                                && geometry_ok
                                && active_flights == 0
                                && deferred_flights == 0;
                            crate::log_ui!(
                                info,
                                "bench.link_preview_matrix cell display={} thumbnail={} stage=initial source=local_replay message_id={} card_count={} action_present={} physical_fetches=0 active_flights={} deferred_flights={} thumbnail_width={:.1} thumbnail_height={:.1} responsive_scaled=false sent=false passed={}",
                                display_label,
                                thumbnail_label,
                                MESSAGE_ID,
                                card_count,
                                action_present,
                                active_flights,
                                deferred_flights,
                                thumbnail_width,
                                thumbnail_height,
                                passed
                            );
                            cx.notify();
                            passed
                        })
                    });
                    let Ok(passed) = updated else {
                        crate::log_ui!(error, "bench.link_preview_matrix failed reason=window_update_failed");
                        return;
                    };
                    if !passed {
                        crate::log_ui!(
                            error,
                            "bench.link_preview_matrix failed reason=initial_assertion display={} thumbnail={}",
                            display_label,
                            thumbnail_label
                        );
                        return;
                    }
                    crate::log_ui!(
                        info,
                        "bench.link_preview_matrix visual_ready display={} thumbnail={} stage=initial",
                        display_label,
                        thumbnail_label
                    );
                    cx.background_executor()
                        .timer(Duration::from_millis(dwell_ms))
                        .await;

                    if display_label != "manual" {
                        continue;
                    }

                    let Some(entity) = this.upgrade() else {
                        return;
                    };
                    let message_for_shown = message.clone();
                    let channel_for_shown = channel.clone();
                    let shown = cx.update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |_app, cx| {
                            backend::bridge::show_embed_preview(
                                URL,
                                message_for_shown.id.to_string(),
                                embed_id,
                            );
                            let layout = crate::components::chat::canvas_renderer::text_shaping::build_message_layout(
                                &message_for_shown,
                                &channel_for_shown,
                                640.0,
                                false,
                                Some(window),
                            );
                            let preview = layout.segments.iter().find_map(|segment| {
                                if let crate::components::chat::canvas_renderer::layout::LayoutSegment::EmbedPreview {
                                    geometry,
                                    is_expanded,
                                    ..
                                } = segment
                                {
                                    Some((*geometry, *is_expanded))
                                } else {
                                    None
                                }
                            });
                            let (active_flights, deferred_flights) = backend::embeds::cache::RESOURCE_FLIGHTS
                                .benchmark_pending_counts();
                            let geometry_ok = preview.is_some_and(|(geometry, _)| {
                                if expected_width == 0.0 {
                                    geometry.thumbnail_width == 0.0 && geometry.thumbnail_height == 0.0
                                } else {
                                    (geometry.thumbnail_width - expected_width).abs() < f32::EPSILON
                                        && (geometry.thumbnail_height - expected_height).abs()
                                            < f32::EPSILON
                                }
                            });
                            let shown = preview.is_some_and(|(_, is_expanded)| is_expanded)
                                && geometry_ok
                                && active_flights == 0
                                && deferred_flights == 0;
                            crate::log_ui!(
                                info,
                                "bench.link_preview_matrix cell display=manual thumbnail={} stage=shown source=local_replay message_id={} card_count={} exact_occurrence=true physical_fetches=0 active_flights={} deferred_flights={} sent=false passed={}",
                                thumbnail_label,
                                MESSAGE_ID,
                                usize::from(preview.is_some()),
                                active_flights,
                                deferred_flights,
                                shown
                            );
                            cx.notify();
                            shown
                        })
                    });
                    if !matches!(shown, Ok(true)) {
                        crate::log_ui!(error, "bench.link_preview_matrix failed reason=manual_show_assertion thumbnail={}", thumbnail_label);
                        return;
                    }
                    crate::log_ui!(info, "bench.link_preview_matrix visual_ready display=manual thumbnail={} stage=shown", thumbnail_label);
                    cx.background_executor().timer(Duration::from_millis(dwell_ms)).await;

                    let Some(entity) = this.upgrade() else {
                        return;
                    };
                    let message_for_hidden = message.clone();
                    let channel_for_hidden = channel.clone();
                    let hidden = cx.update_window(window_handle, move |_view, window, cx| {
                        entity.update(cx, |_app, cx| {
                            backend::bridge::hide_embed_preview(
                                URL,
                                &message_for_hidden.id,
                                embed_id,
                            );
                            let layout = crate::components::chat::canvas_renderer::text_shaping::build_message_layout(
                                &message_for_hidden,
                                &channel_for_hidden,
                                640.0,
                                false,
                                Some(window),
                            );
                            let card_count = layout.segments.iter().filter(|segment| {
                                matches!(segment, crate::components::chat::canvas_renderer::layout::LayoutSegment::EmbedPreview { .. })
                            }).count();
                            let (active_flights, deferred_flights) = backend::embeds::cache::RESOURCE_FLIGHTS
                                .benchmark_pending_counts();
                            let hidden = card_count == 0 && active_flights == 0 && deferred_flights == 0;
                            crate::log_ui!(
                                info,
                                "bench.link_preview_matrix cell display=manual thumbnail={} stage=hidden source=local_replay message_id={} card_count={} exact_occurrence=true physical_fetches=0 active_flights={} deferred_flights={} sent=false passed={}",
                                thumbnail_label,
                                MESSAGE_ID,
                                card_count,
                                active_flights,
                                deferred_flights,
                                hidden
                            );
                            cx.notify();
                            hidden
                        })
                    });
                    if !matches!(hidden, Ok(true)) {
                        crate::log_ui!(error, "bench.link_preview_matrix failed reason=manual_hide_assertion thumbnail={}", thumbnail_label);
                        return;
                    }
                    crate::log_ui!(info, "bench.link_preview_matrix visual_ready display=manual thumbnail={} stage=hidden", thumbnail_label);
                    cx.background_executor().timer(Duration::from_millis(dwell_ms)).await;
                }
            }
            let attempts = crate::benchmark_profile::live_outbound_user_action_attempts();
            let passed = attempts == 0;
            crate::log_ui!(
                info,
                "bench.link_preview_matrix complete cells=12 source=local_replay outbound_user_action_attempts={} send=false passed={}",
                attempts,
                passed
            );
        }))
    }

    /// Drive the live UI benchmark through Echo's own action dispatcher.
    ///
    /// This is opt-in and deliberately contains no text-entry, message-send, moderation,
    /// browser-launch, or destructive action. Keeping the stimulus inside GPUI makes live
    /// benchmark runs deterministic even when Windows refuses background focus changes.
    pub(super) fn spawn_live_ui_smoke_task(
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        use backend::hotkeys::HotkeyAction;

        if crate::benchmark_ui_fixture::link_preview_matrix_enabled() {
            return Self::spawn_link_preview_matrix_task(window_handle, cx);
        }
        if crate::benchmark_ui_fixture::lifecycle_churn_enabled() {
            return Self::spawn_lifecycle_churn_task(window_handle, cx);
        }
        if crate::benchmark_ui_fixture::responsive_matrix_enabled() {
            return None;
        }
        if crate::benchmark_ui_fixture::is_enabled() {
            return Self::spawn_ui_fixture_smoke_task(window_handle, cx);
        }
        if let Some(task) = Self::spawn_graceful_quit_task(cx) {
            return Some(task);
        }

        let picker_sweep_channel = std::env::var("ECHO_BENCH_PICKER_SWEEP_CHANNEL")
            .ok()
            .map(|channel| channel.trim().trim_start_matches('#').to_ascii_lowercase())
            .filter(|channel| !channel.is_empty());
        let enabled = crate::benchmark_ui_fixture::env_flag("ECHO_BENCH_UI_SMOKE");
        if !enabled && picker_sweep_channel.is_none() {
            return None;
        }

        let delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_UI_SMOKE_DELAY_MS").unwrap_or(5_000);
        let step_delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_UI_SMOKE_STEP_DELAY_MS")
                .unwrap_or(400);

        let mut steps = Vec::with_capacity(40);
        steps.push(HotkeyAction::ToggleMetrics);
        steps.extend(std::iter::repeat_n(HotkeyAction::NextTab, 8));
        steps.extend(std::iter::repeat_n(HotkeyAction::PrevTab, 4));
        steps.push(HotkeyAction::FocusSplitRight);
        steps.push(HotkeyAction::FocusSplitLeft);
        steps.extend(std::iter::repeat_n(HotkeyAction::ScrollUp, 6));
        steps.extend(std::iter::repeat_n(HotkeyAction::ScrollDown, 6));
        if picker_sweep_channel.is_none() {
            steps.push(HotkeyAction::OpenEmotePicker);
            steps.push(HotkeyAction::OpenEmotePicker);
        }
        steps.push(HotkeyAction::ToggleSettings);
        steps.push(HotkeyAction::ToggleSettings);
        steps.push(HotkeyAction::QuickSwitcher);
        steps.push(HotkeyAction::QuickSwitcher);
        steps.push(HotkeyAction::ToggleMetrics);
        steps.push(HotkeyAction::ToggleMetrics);

        Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;

            if let Some(channel) = picker_sweep_channel {
                let Some(entity) = this.upgrade() else {
                    return;
                };
                let channel_for_update = channel.clone();
                let updated = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        let target_found = app.focus_notification_channel(&channel_for_update, cx);
                        crate::log_ui!(
                            info,
                            "bench.picker_sweep request channel={} target_found={}",
                            channel_for_update,
                            target_found
                        );
                        if target_found {
                            crate::managers::queue_emote_popout(channel_for_update.clone());
                        }
                    });
                });
                if updated.is_err() {
                    return;
                }

                cx.background_executor()
                    .timer(Duration::from_millis(1_000))
                    .await;
            }

            for (index, step) in steps.into_iter().enumerate() {
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let updated = cx.update_window(window_handle, move |_view, window, cx| {
                    entity.update(cx, |app, cx| {
                        crate::log_ui!(
                            info,
                            "bench.live_ui_smoke step={} action={:?}",
                            index,
                            step
                        );
                        app.execute_hotkey_action(step, window, cx);
                        if step == HotkeyAction::OpenEmotePicker {
                            let picker_open = app
                                .overlay_manager
                                .read(cx)
                                .emote_picker
                                .read(cx)
                                .state
                                .is_open;
                            crate::log_ui!(
                                info,
                                "bench.live_ui_smoke step={} picker_open={}",
                                index,
                                picker_open
                            );
                        }
                    });
                });
                if updated.is_err() {
                    break;
                }

                cx.background_executor()
                    .timer(Duration::from_millis(step_delay_ms))
                    .await;
            }
        }))
    }

    /// Exercise production tab/split close and reopen cleanup against the
    /// process-local UI fixture. No synthetic input or send action is used.
    fn spawn_lifecycle_churn_task(
        window_handle: AnyWindowHandle,
        cx: &mut Context<Self>,
    ) -> Option<Task<()>> {
        let cycles = std::env::var("ECHO_BENCH_LIFECYCLE_CYCLES")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
            .filter(|value| *value >= 20)
            .unwrap_or(20);
        let delay_ms = crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_LIFECYCLE_CHURN_DELAY_MS")
            .unwrap_or(1_000);
        let cycle_delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_LIFECYCLE_CHURN_STEP_DELAY_MS")
                .unwrap_or(50);
        let settle_delay_ms =
            crate::benchmark_ui_fixture::env_u64("ECHO_BENCH_LIFECYCLE_SETTLE_MS").unwrap_or(1_000);

        Some(cx.spawn(async move |this, cx| {
            if let Some(entity) = this.upgrade() {
                let _ = cx.update_window(window_handle, |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        app.overlay_manager.update(cx, |overlays, cx| {
                            overlays.push_toast(
                                crate::components::overlays::Toast::info(
                                    "Lifecycle benchmark: warm plateau",
                                )
                                .with_detail(format!(
                                    "Hermetic no-send fixture; churn begins in {:.1}s",
                                    delay_ms as f64 / 1000.0
                                ))
                                .with_duration((delay_ms / 1000).clamp(5, 255) as u8),
                                cx,
                            );
                        });
                    });
                });
            }
            cx.background_executor()
                .timer(Duration::from_millis(delay_ms))
                .await;
            let Some(entity) = this.upgrade() else {
                return;
            };

            let warmed = cx.update_window(window_handle, |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let state = app.app_state.read(cx);
                    let tab_count = state.tabs.len();
                    let split_count = state.active_tab().layout.count_leaves();
                    let cache = crate::cache::loader::stats().diagnostic_snapshot();
                    let memory = crate::backend::metrics::get_memory_stats();
                    let layout = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
                    crate::log_ui!(
                        info,
                        "bench.lifecycle_churn phase=warm_plateau tabs={} splits={} chat_views={} split_panes={} split_subscriptions={} split_entities={} active_channels={} stored_messages={} static_images={} animated_images={} image_bytes={} frame_bytes={} active_downloads={} evictions={} bytes_evicted={} layout_queue_depth={} queue_disconnected_total={} worker_panic_total={} acknowledged=true sent=false",
                        tab_count,
                        split_count,
                        app.chat_views.len(),
                        app.split_panes.len(),
                        app.split_pane_subscriptions.len(),
                        app.split_leaf_views.len(),
                        memory.active_channels,
                        memory.stored_messages,
                        cache.static_images,
                        cache.animated_images,
                        cache.memory_used,
                        cache.frame_memory,
                        cache.active_downloads,
                        cache.evictions,
                        cache.bytes_evicted,
                        layout.queue_depth,
                        layout.queue_disconnected_total,
                        layout.worker_panic_total
                    );
                });
            });
            if warmed.is_err() {
                return;
            }

            let mut completed = 0usize;
            for cycle in 1..=cycles {
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let channel = crate::benchmark_ui_fixture::LIFECYCLE_CHURN_CHANNELS
                    [(cycle - 1) % crate::benchmark_ui_fixture::LIFECYCLE_CHURN_CHANNELS.len()]
                    .to_string();
                let opened_channel = channel.clone();
                let updated = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        app.overlay_manager.update(cx, |overlays, cx| {
                            overlays.toast_info(
                                format!(
                                    "Lifecycle cycle {cycle}/{cycles}: split + popout + scroll"
                                ),
                                cx,
                            );
                        });
                        let Some(path) = app.create_placeholder_split_adjacent_to_active(
                            crate::state::AddSplitDirection::Right,
                            cx,
                        ) else {
                            crate::log_ui!(
                                error,
                                "bench.lifecycle_churn phase=cycle cycle={} acknowledged=false sent=false reason=split_create_failed",
                                cycle
                            );
                            return;
                        };
                        app.set_channel_for_path(path, opened_channel.clone(), cx);
                        crate::managers::queue_emote_popout(opened_channel.clone());
                        app.scroll_active_chat_by_pixels(240.0, cx);
                        app.scroll_active_chat_by_page(-1, cx);
                        app.scroll_active_chat_by_pixels(-120.0, cx);
                        let state = app.app_state.read(cx);
                        let tab_count = state.tabs.len();
                        let split_count = state.active_tab().layout.count_leaves();
                        let active_channel = state
                            .active_tab()
                            .layout
                            .get_channel_at_path(&state.active_tab().active_split_path)
                            .unwrap_or_default();
                        crate::log_ui!(
                            info,
                            "bench.lifecycle_churn phase=cycle cycle={} acknowledged={} sent=false tabs={} splits={} viewport_movements=3 popout_requested=true channel={} active_channel={}",
                            cycle,
                            split_count == crate::benchmark_ui_fixture::FIXTURE_SPLIT_COUNT + 1,
                            tab_count,
                            split_count,
                            opened_channel,
                            active_channel
                        );
                    });
                });
                if updated.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(cycle_delay_ms))
                    .await;
                let popout_closed = cx
                    .update_window(window_handle, |_view, _window, cx| {
                        crate::components::overlays::benchmark_close_popout(cx)
                    })
                    .is_ok();
                crate::log_ui!(
                    info,
                    "bench.lifecycle_churn phase=popout cycle={} closed={} open_after={} sent=false",
                    cycle,
                    popout_closed,
                    crate::components::overlays::popout_is_open()
                );
                if !popout_closed || crate::components::overlays::popout_is_open() {
                    break;
                }
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let removed = cx.update_window(window_handle, |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        app.close_active_split(cx);
                    });
                });
                if removed.is_err() {
                    break;
                }
                cx.background_executor()
                    .timer(Duration::from_millis(cycle_delay_ms))
                    .await;
                let Some(entity) = this.upgrade() else {
                    break;
                };
                let cleaned = cx.update_window(window_handle, move |_view, _window, cx| {
                    entity.update(cx, |app, cx| {
                        let state = app.app_state.read(cx);
                        let tab_count = state.tabs.len();
                        let split_count = state.active_tab().layout.count_leaves();
                        let target_channel_present = state.tabs.iter().any(|tab| {
                            tab.layout
                                .channels()
                                .iter()
                                .any(|candidate| candidate == &channel)
                        });
                        let cache = crate::cache::loader::stats().diagnostic_snapshot();
                        let memory = crate::backend::metrics::get_memory_stats();
                        let layout = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
                        let acknowledged = split_count
                            == crate::benchmark_ui_fixture::FIXTURE_SPLIT_COUNT
                            && !target_channel_present
                            && !crate::components::overlays::popout_is_open()
                            && layout.queue_disconnected_total == 0
                            && layout.worker_panic_total == 0;
                        crate::log_ui!(
                            info,
                            "bench.lifecycle_churn phase=checkpoint cycle={} acknowledged={} channel_removed={} popout_open=false tabs={} splits={} chat_views={} split_panes={} split_subscriptions={} split_entities={} active_channels={} stored_messages={} static_images={} animated_images={} image_bytes={} hot_frames={} animation_metadata={} frame_bytes={} active_downloads={} evictions={} bytes_evicted={} texture_reuses={} texture_bytes_saved={} layout_queue_depth={} layout_queue_peak={} queue_full_total={} queue_disconnected_total={} worker_panic_total={} sent=false",
                            cycle,
                            acknowledged,
                            !target_channel_present,
                            tab_count,
                            split_count,
                            app.chat_views.len(),
                            app.split_panes.len(),
                            app.split_pane_subscriptions.len(),
                            app.split_leaf_views.len(),
                            memory.active_channels,
                            memory.stored_messages,
                            cache.static_images,
                            cache.animated_images,
                            cache.memory_used,
                            cache.hot_frames,
                            cache.animation_metadata,
                            cache.frame_memory,
                            cache.active_downloads,
                            cache.evictions,
                            cache.bytes_evicted,
                            cache.texture_reuses,
                            cache.texture_bytes_saved,
                            layout.queue_depth,
                            layout.queue_peak_depth,
                            layout.queue_full_total,
                            layout.queue_disconnected_total,
                            layout.worker_panic_total
                        );
                        acknowledged
                    })
                });
                if !matches!(cleaned, Ok(true)) {
                    break;
                }
                completed = cycle;
            }

            let Some(entity) = this.upgrade() else {
                return;
            };
            let settling = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let state = app.app_state.read(cx);
                    let tab_count = state.tabs.len();
                    let split_count = state.active_tab().layout.count_leaves();
                    crate::log_ui!(
                        info,
                        "bench.lifecycle_churn phase=settling cycles_completed={} tabs={} splits={} sent=false",
                        completed,
                        tab_count,
                        split_count
                    );
                    crate::benchmark_ui_fixture::cleanup();
                    app.overlay_manager.update(cx, |overlays, cx| {
                        overlays.push_toast(
                            crate::components::overlays::Toast::info(
                                "Lifecycle benchmark: settling plateau",
                            )
                            .with_detail(format!(
                                "All {completed} cycles closed; observing cleanup for {:.1}s",
                                settle_delay_ms as f64 / 1000.0
                            ))
                            .with_duration((settle_delay_ms / 1000).clamp(5, 255) as u8),
                            cx,
                        );
                    });
                    cx.notify();
                });
            });
            if settling.is_err() {
                return;
            }
            cx.background_executor()
                .timer(Duration::from_millis(settle_delay_ms))
                .await;
            let Some(entity) = this.upgrade() else {
                return;
            };
            let _ = cx.update_window(window_handle, move |_view, _window, cx| {
                entity.update(cx, |app, cx| {
                    let state = app.app_state.read(cx);
                    let tab_count = state.tabs.len();
                    let split_count = state.active_tab().layout.count_leaves();
                    let cache = crate::cache::loader::stats().diagnostic_snapshot();
                    let memory = crate::backend::metrics::get_memory_stats();
                    let layout = crate::components::chat::canvas_renderer::layout_worker_diagnostics();
                    let passed = completed == cycles
                        && tab_count == crate::benchmark_ui_fixture::FIXTURE_TAB_COUNT
                        && split_count == crate::benchmark_ui_fixture::FIXTURE_SPLIT_COUNT
                        && !crate::components::overlays::popout_is_open()
                        && layout.queue_disconnected_total == 0
                        && layout.worker_panic_total == 0;
                    crate::log_ui!(
                        info,
                        "bench.lifecycle_churn phase=complete cycles_completed={} passed={} cleanup_applied=true sent=false tabs={} splits={} chat_views={} split_panes={} split_subscriptions={} split_entities={} active_channels={} stored_messages={} static_images={} animated_images={} image_bytes={} frame_bytes={} active_downloads={} evictions={} bytes_evicted={} layout_queue_depth={} queue_disconnected_total={} worker_panic_total={} popout_open={}",
                        completed,
                        passed,
                        tab_count,
                        split_count,
                        app.chat_views.len(),
                        app.split_panes.len(),
                        app.split_pane_subscriptions.len(),
                        app.split_leaf_views.len(),
                        memory.active_channels,
                        memory.stored_messages,
                        cache.static_images,
                        cache.animated_images,
                        cache.memory_used,
                        cache.frame_memory,
                        cache.active_downloads,
                        cache.evictions,
                        cache.bytes_evicted,
                        layout.queue_depth,
                        layout.queue_disconnected_total,
                        layout.worker_panic_total,
                        crate::components::overlays::popout_is_open()
                    );
                });
            });
        }))
    }

    fn prepare_main_completion_smoke(
        &mut self,
        text: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<crate::benchmark_ui_fixture::CompletionUiSnapshot> {
        if !crate::benchmark_ui_fixture::is_enabled() {
            return None;
        }
        let (channel, path, split_id) = self.active_split_target(cx)?;
        if channel != crate::benchmark_ui_fixture::OFFLINE_CHANNEL {
            return None;
        }

        self.chat_input_renderer.update(cx, |renderer, cx| {
            renderer
                .input_manager
                .input_texts
                .insert(split_id, text.to_string());
            renderer
                .input_manager
                .set_cursor_for_split(split_id, text.len());
            renderer.input_manager.history_index.remove(&split_id);
            renderer.input_manager.temp_input.remove(&split_id);
            renderer.reset_inline_tab_completion(split_id);
            cx.notify();
        });
        self.update_autocomplete(&channel, &path, Some(text), cx);
        let focus_handle = if backend::settings::get_unified_input_mode() {
            self.input_focus.clone()
        } else {
            self.split_panes
                .get(&split_id)?
                .read(cx)
                .focus_handle
                .clone()
        };
        window.focus(&focus_handle, cx);
        window.refresh();
        cx.notify();
        self.main_completion_smoke_snapshot(window, cx)
    }

    fn completion_smoke_snapshot_for(
        &self,
        target: &str,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<crate::benchmark_ui_fixture::CompletionUiSnapshot> {
        if target != "thread" {
            self.main_completion_smoke_snapshot(window, cx)
        } else {
            let viewer = self.overlay_manager.read(cx).thread_viewer.clone();
            viewer.read(cx).completion_smoke_snapshot(window, cx)
        }
    }

    fn main_completion_smoke_snapshot(
        &self,
        window: &Window,
        cx: &Context<Self>,
    ) -> Option<crate::benchmark_ui_fixture::CompletionUiSnapshot> {
        if !crate::benchmark_ui_fixture::is_enabled() {
            return None;
        }
        let (channel, _path, split_id) = self.active_split_target(cx)?;
        if channel != crate::benchmark_ui_fixture::OFFLINE_CHANNEL {
            return None;
        }
        let renderer = self.chat_input_renderer.read(cx);
        let autocomplete = self.autocomplete(cx);
        let autocomplete = autocomplete.read(cx);
        let selected_text = autocomplete
            .selection_at(
                autocomplete.suggestion_revision(),
                autocomplete.state.selected_index,
            )
            .map(|selection| selection.text);
        Some(crate::benchmark_ui_fixture::CompletionUiSnapshot {
            input: renderer
                .input_manager
                .input_texts
                .get(&split_id)
                .cloned()
                .unwrap_or_default(),
            cursor: renderer.input_manager.cursor_for_split(split_id),
            focused: if backend::settings::get_unified_input_mode() {
                self.input_focus.is_focused(window)
            } else {
                self.split_panes
                    .get(&split_id)
                    .is_some_and(|pane| pane.read(cx).focus_handle.is_focused(window))
            },
            suggestions_open: autocomplete.state.is_open,
            suggestion_count: autocomplete.suggestions.len(),
            selected_text,
        })
    }
}
