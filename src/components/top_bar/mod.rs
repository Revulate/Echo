//! Top bar component for Echo
//!
//! Contains the titlebar, window controls, and account switcher.
//! Extracted from app.rs to reduce monolith size.

use crate::backend::hotkeys::HotkeyAction;
use crate::components::auth::render_profile_avatar;
use crate::components::logo::render_logo;
use crate::components::overlays::{
    clear_global_tooltip, clear_global_tooltip_if_source_in_window, show_hotkey_hint_tooltip,
    show_text_tooltip,
};
use crate::components::tabs::{InlineTabDemand, TabBarRenderer};
use crate::components::ui::menu::{MenuHost, MenuKeyboard, menu_panel};
use crate::components::ui::{ButtonExt, PressToClick};
use crate::icons::{
    IconName, dropdown_header_style, icon, icon_button_style, window_control_button_style,
};
use crate::managers::render_hooks::get_cached_auth_state;
use crate::managers::{OverlayManager, SplitLayoutManager};
use crate::state::AppState;
use crate::theme::colors;
use crate::theme::tokens::{FontSize, Radii, RadiiPixels, Spacing};
use crate::{EchoApp, backend, identity};
use gpui::StatefulInteractiveElement;
use gpui::prelude::FluentBuilder;
use gpui::*;

const SHOW_APP_TITLE_MIN_WIDTH_PX: f32 = 520.0;
const SHOW_VERSION_MIN_WIDTH_PX: f32 = 700.0;
const SHOW_LOGO_MIN_WIDTH_PX: f32 = 320.0;
const SHOW_ACTIVITY_LABEL_MIN_WIDTH_PX: f32 = 760.0;
/// The wordmark and version's conservative occupied widths, including their
/// adjacent flex gaps.
const INLINE_APP_TITLE_RESERVATION_PX: f32 = 48.0;
const INLINE_VERSION_RESERVATION_PX: f32 = 52.0;
const INLINE_LOGO_RESERVATION_PX: f32 = 24.0;
const INLINE_BRANDING_RESTORE_SLACK_PX: f32 = 12.0;
const INLINE_LEFT_PADDING_PX: f32 = Spacing::LG;
const INLINE_TAB_SLOT_PADDING_PX: f32 = Spacing::SM;
const INLINE_RIGHT_CONTROLS_COMPACT_PX: f32 = 212.0;
/// Compact controls plus the labelled Activity button's extra width.
const INLINE_RIGHT_CONTROLS_EXPANDED_PX: f32 =
    INLINE_RIGHT_CONTROLS_COMPACT_PX + ACTIVITY_BUTTON_EXPANDED_PX - ACTIVITY_BUTTON_COMPACT_PX;
const ACTIVITY_BUTTON_COMPACT_PX: f32 = 26.0;
/// Fits the icon, the 13px "Activity" label and an 11px "99+" count pill.
const ACTIVITY_BUTTON_EXPANDED_PX: f32 = 100.0;
const ACTIVITY_COUNT_PILL_HEIGHT_PX: f32 = 16.0;
const UPDATE_BADGE_SIZE_PX: f32 = 14.0;

#[derive(Clone, Copy, Default, PartialEq)]
struct InlineBrandingVisibility {
    show_logo: bool,
    show_app_title: bool,
    show_version: bool,
}

impl InlineBrandingVisibility {
    fn normal(logo_allowed: bool, app_title_allowed: bool, version_allowed: bool) -> Self {
        Self {
            show_logo: logo_allowed,
            show_app_title: app_title_allowed,
            show_version: version_allowed && app_title_allowed,
        }
    }

    fn visible_count(self) -> u8 {
        u8::from(self.show_logo) + u8::from(self.show_app_title) + u8::from(self.show_version)
    }
}

/// Inputs that determine how much title branding may share an inline tab bar.
///
/// Keeping these values together makes the viewport policy explicit and keeps
/// the stateful visibility resolver independent of render-local variables.
#[derive(Clone, Copy)]
struct InlineBrandingConstraints {
    inline_tabs: bool,
    window_width: f32,
    logo_allowed: bool,
    app_title_allowed: bool,
    version_allowed: bool,
    show_activity_label: bool,
    tab_demand: InlineTabDemand,
}

#[derive(Clone, PartialEq)]
pub struct TopBarCachedState {
    pub generation: u64,
    pub auth_version: u64,
    pub update_generation: u64,
    pub account_profile_image_url: Option<SharedString>,
    pub account_avatar_label: SharedString,
    pub show_account_identity: bool,
    pub show_account_warning: bool,
    pub notification_count: usize,
    pub mention_count: usize,
    pub app_title: SharedString,
    pub unread_whispers: u32,
    pub update_attention: bool,
}

impl Default for TopBarCachedState {
    fn default() -> Self {
        Self {
            generation: u64::MAX,
            auth_version: u64::MAX,
            update_generation: u64::MAX,
            account_profile_image_url: None,
            account_avatar_label: SharedString::from("Anonymous"),
            show_account_identity: false,
            show_account_warning: false,
            notification_count: 0,
            mention_count: 0,
            app_title: SharedString::from("Echo"),
            unread_whispers: 0,
            update_attention: false,
        }
    }
}

#[derive(Default)]
pub struct TopBarCache {
    state: TopBarCachedState,
    inline_branding_visibility: Option<InlineBrandingVisibility>,
    inline_branding_context: Option<u64>,
    connection: ConnectionTimeline,
}

/// When chat was last seen connected, for the connection-status tooltip.
#[derive(Clone, Copy, Default, PartialEq)]
struct ConnectionTimeline {
    connected_since_ms: Option<i64>,
    last_connected_ms: Option<i64>,
}

impl ConnectionTimeline {
    fn observe(&mut self, status: &backend::ConnectionStatus) {
        let now_ms = chrono::Local::now().timestamp_millis();
        match status {
            backend::ConnectionStatus::Connected => {
                self.connected_since_ms.get_or_insert(now_ms);
            }
            _ => {
                if self.connected_since_ms.take().is_some() {
                    self.last_connected_ms = Some(now_ms);
                }
            }
        }
    }

    fn status_label(self, status: &backend::ConnectionStatus) -> &'static str {
        match status {
            backend::ConnectionStatus::Connected => "Connected",
            backend::ConnectionStatus::Reconnecting => "Reconnecting\u{2026}",
            backend::ConnectionStatus::Disconnected if self.last_connected_ms.is_none() => {
                "Connecting\u{2026}"
            }
            backend::ConnectionStatus::Disconnected => "Disconnected",
        }
    }

    fn tooltip(self, status: &backend::ConnectionStatus) -> String {
        let label = self.status_label(status);
        match (status, self.connected_since_ms, self.last_connected_ms) {
            (backend::ConnectionStatus::Connected, Some(since), _) => format!(
                "Twitch chat: {label} since {}",
                crate::utils::timestamp::format_sidebar_timestamp(since)
            ),
            (_, _, Some(last)) => format!(
                "Twitch chat: {label} (last connected {})",
                crate::utils::timestamp::format_sidebar_timestamp(last)
            ),
            _ => format!("Twitch chat: {label}"),
        }
    }
}

fn connection_status_color(status: &backend::ConnectionStatus) -> Hsla {
    match status {
        backend::ConnectionStatus::Connected => colors::success(),
        backend::ConnectionStatus::Reconnecting => colors::warning(),
        backend::ConnectionStatus::Disconnected => colors::error(),
    }
}

/// Compact status text shown whenever chat is not connected. It sits in the
/// fixed left cluster, so inline tabs never squeeze it out.
fn render_connection_chip(
    status: &backend::ConnectionStatus,
    timeline: ConnectionTimeline,
) -> impl IntoElement + use<> {
    let label = timeline.status_label(status);
    let tooltip = timeline.tooltip(status);
    let color = if timeline.last_connected_ms.is_none()
        && matches!(status, backend::ConnectionStatus::Disconnected)
    {
        colors::text_secondary()
    } else {
        connection_status_color(status)
    };
    div()
        .id("titlebar-connection-chip")
        .flex()
        .flex_shrink_0()
        .items_center()
        .gap(px(Spacing::XS_PLUS))
        .h(px(20.0))
        .px(px(Spacing::SM))
        .rounded(Radii::xl())
        .border_1()
        .border_color(color.opacity(0.45))
        .bg(color.opacity(0.10))
        .on_hover(move |hovered, window, _app| {
            let source = "titlebar-connection-chip".to_string();
            if *hovered {
                show_text_tooltip(
                    window.window_handle(),
                    source,
                    tooltip.clone(),
                    window.mouse_position(),
                );
            } else {
                clear_global_tooltip_if_source_in_window(window.window_handle(), &source);
            }
        })
        .child(div().size(px(6.0)).rounded(Radii::full()).bg(color))
        .child(
            div()
                .text_size(px(FontSize::XS))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .whitespace_nowrap()
                .child(label),
        )
}

impl TopBarCache {
    fn inline_tab_slot_width(
        window_width: f32,
        show_activity_label: bool,
        visibility: InlineBrandingVisibility,
    ) -> f32 {
        let left_width = INLINE_LEFT_PADDING_PX
            + if visibility.show_logo {
                INLINE_LOGO_RESERVATION_PX
            } else {
                0.0
            }
            + if visibility.show_app_title {
                INLINE_APP_TITLE_RESERVATION_PX
            } else {
                0.0
            }
            + if visibility.show_version {
                INLINE_VERSION_RESERVATION_PX
            } else {
                0.0
            };
        let right_width = if show_activity_label {
            INLINE_RIGHT_CONTROLS_EXPANDED_PX
        } else {
            INLINE_RIGHT_CONTROLS_COMPACT_PX
        };

        (window_width - left_width - right_width - INLINE_TAB_SLOT_PADDING_PX).max(0.0)
    }

    fn fits_inline_tab_reservation(
        window_width: f32,
        show_activity_label: bool,
        visibility: InlineBrandingVisibility,
        tab_demand: InlineTabDemand,
        extra_slack: f32,
    ) -> bool {
        Self::inline_tab_slot_width(window_width, show_activity_label, visibility)
            >= tab_demand.required_width_px() + extra_slack
    }

    fn collapse_branding_until_tabs_fit(
        window_width: f32,
        show_activity_label: bool,
        normal: InlineBrandingVisibility,
        tab_demand: InlineTabDemand,
    ) -> InlineBrandingVisibility {
        let mut visibility = normal;
        while !Self::fits_inline_tab_reservation(
            window_width,
            show_activity_label,
            visibility,
            tab_demand,
            0.0,
        ) {
            if visibility.show_version {
                visibility.show_version = false;
            } else if visibility.show_app_title {
                visibility.show_app_title = false;
            } else if visibility.show_logo {
                visibility.show_logo = false;
            } else {
                break;
            }
        }
        visibility
    }

    fn restore_branding_with_slack(
        window_width: f32,
        show_activity_label: bool,
        current: InlineBrandingVisibility,
        target: InlineBrandingVisibility,
        tab_demand: InlineTabDemand,
    ) -> InlineBrandingVisibility {
        let mut visibility = current;
        loop {
            let mut candidate = visibility;
            if !candidate.show_logo && target.show_logo {
                candidate.show_logo = true;
            } else if !candidate.show_app_title && target.show_app_title {
                candidate.show_app_title = true;
            } else if !candidate.show_version && target.show_version {
                candidate.show_version = true;
            } else {
                break;
            }

            if !Self::fits_inline_tab_reservation(
                window_width,
                show_activity_label,
                candidate,
                tab_demand,
                INLINE_BRANDING_RESTORE_SLACK_PX,
            ) {
                break;
            }
            visibility = candidate;
        }
        visibility
    }

    fn resolve_inline_branding_visibility(
        &mut self,
        constraints: InlineBrandingConstraints,
    ) -> InlineBrandingVisibility {
        let InlineBrandingConstraints {
            inline_tabs,
            window_width,
            logo_allowed,
            app_title_allowed,
            version_allowed,
            show_activity_label,
            tab_demand,
        } = constraints;
        let normal =
            InlineBrandingVisibility::normal(logo_allowed, app_title_allowed, version_allowed);
        if !inline_tabs || !tab_demand.is_known() {
            self.inline_branding_visibility = None;
            self.inline_branding_context = None;
            return normal;
        }

        let target = Self::collapse_branding_until_tabs_fit(
            window_width,
            show_activity_label,
            normal,
            tab_demand,
        );
        let context_matches = self.inline_branding_context == Some(tab_demand.context());
        let visibility = match self.inline_branding_visibility.filter(|_| context_matches) {
            None => target,
            Some(current) if target.visible_count() <= current.visible_count() => target,
            Some(current) => Self::restore_branding_with_slack(
                window_width,
                show_activity_label,
                current,
                target,
                tab_demand,
            ),
        };

        self.inline_branding_visibility = Some(visibility);
        self.inline_branding_context = Some(tab_demand.context());
        visibility
    }

    pub fn snapshot(&mut self) -> TopBarCachedState {
        let generation = crate::state::ui_invalidation::top_bar_state_version();
        let auth_version = crate::state::VersionRegistry::auth_version();
        let update_generation = backend::updates::state_generation();
        if self.state.generation == generation
            && self.state.auth_version == auth_version
            && self.state.update_generation == update_generation
        {
            return self.state.clone();
        }

        let auth_state = get_cached_auth_state();

        let accounts_vec = backend::get_accounts();
        let active_account = accounts_vec.iter().find(|account| account.is_active);
        let account_display_name = auth_state
            .display_name
            .or_else(|| active_account.and_then(|account| account.display_name.clone()))
            .map(SharedString::from);
        let account_username = auth_state
            .username
            .or_else(|| active_account.map(|account| account.username.clone()))
            .map(SharedString::from);
        let account_profile_image_url = auth_state
            .profile_image_url
            .or_else(|| active_account.and_then(|account| account.profile_image_url.clone()))
            .map(SharedString::from);
        let notification_count = backend::get_unread_twitch_event_count();
        let mention_count = backend::get_unread_global_mention_count();
        let unread_whispers = backend::get_unread_whisper_count();
        let active_account_missing_token = backend::bridge::active_account_missing_runtime_token();
        let is_anon_mode = backend::bridge::is_anonymous_mode();
        let needs_reauth = backend::bridge::get_needs_reauth();
        let show_account_identity =
            auth_state.is_authenticated && !active_account_missing_token && !is_anon_mode;
        let display_label = account_display_name
            .as_ref()
            .or(account_username.as_ref())
            .map(SharedString::as_ref)
            .unwrap_or("Anonymous");
        let account_avatar_label = if backend::streamer_mode::should_hide_account_names() {
            SharedString::from("Account")
        } else {
            SharedString::from(display_label)
        };
        let show_account_warning =
            needs_reauth || is_anon_mode || (!accounts_vec.is_empty() && !show_account_identity);
        let update_attention = matches!(
            backend::updates::state_kind(),
            backend::updates::UpdateStateKind::Available
                | backend::updates::UpdateStateKind::Downloading
                | backend::updates::UpdateStateKind::Ready
        );

        self.state = TopBarCachedState {
            generation,
            auth_version: auth_state.version,
            update_generation,
            account_profile_image_url,
            account_avatar_label,
            show_account_identity,
            show_account_warning,
            notification_count,
            mention_count,
            app_title: SharedString::from("Echo"),
            unread_whispers,
            update_attention,
        };
        self.state.clone()
    }
}

/// Everything the titlebar paints from, resolved once per app frame. The view
/// re-renders only when this differs from the frame it last rendered, when it
/// is notified (logo pulse, hover styles, tooltips), or when the inline tab
/// bar it hosts is stale.
#[derive(Clone, PartialEq)]
struct TopBarFrame {
    titlebar_height: f32,
    connection_status: backend::ConnectionStatus,
    connection_timeline: ConnectionTimeline,
    show_connection_chip: bool,
    activity_open: bool,
    cached: TopBarCachedState,
    branding: InlineBrandingVisibility,
    show_activity_label: bool,
    inline_tabs: bool,
    account_switcher_open: bool,
    /// Streamer mode can hide the avatar without any top-bar version bump.
    hide_avatars: bool,
    reduce_motion: bool,
    theme_key: (u64, u64, bool),
    /// Echo draws its own minimize/maximize/close buttons.
    window_controls: bool,
}

/// Whether Echo must draw its own window buttons. Windows always uses Echo's
/// custom title bar; on Linux a compositor that grants server-side
/// decorations (KWin) already draws them in its own title bar, so Echo's
/// would be duplicates. Client-decorated sessions still need Echo's.
fn draws_window_controls(window: &Window) -> bool {
    cfg!(target_os = "windows") || matches!(window.window_decorations(), Decorations::Client { .. })
}

/// The fixed-height titlebar (product actions, window controls and optional
/// inline tabs) as its own view, so app frames that change nothing in it reuse
/// its subtree instead of rebuilding it.
pub struct TopBarView {
    app: WeakEntity<EchoApp>,
    app_state: Entity<AppState>,
    overlay_manager: Entity<OverlayManager>,
    split_layout: Entity<SplitLayoutManager>,
    tab_bar: Entity<TabBarRenderer>,
    cache: TopBarCache,
    frame: Option<TopBarFrame>,
    rendered_frame: Option<TopBarFrame>,
    tab_bar_height: f32,
}

impl TopBarView {
    pub fn new(
        app: WeakEntity<EchoApp>,
        app_state: Entity<AppState>,
        overlay_manager: Entity<OverlayManager>,
        split_layout: Entity<SplitLayoutManager>,
        tab_bar: Entity<TabBarRenderer>,
    ) -> Self {
        Self {
            app,
            app_state,
            overlay_manager,
            split_layout,
            tab_bar,
            cache: TopBarCache::default(),
            frame: None,
            rendered_frame: None,
            tab_bar_height: 0.0,
        }
    }

    /// Resolve this frame's titlebar inputs. Runs on every app render, before
    /// layout, because connection state, activity counts, overlays, settings
    /// and theme change without notifying this view.
    ///
    /// Returns `true` when the retained subtree is stale and the view must be
    /// rendered uncached this frame.
    pub fn prepare(
        &mut self,
        inline_tabs: bool,
        tab_demand: InlineTabDemand,
        tab_bar_height: f32,
        tab_bar_stale: bool,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (connection_status, activity_open) = {
            let state = self.app_state.read(cx);
            (state.connection_status.clone(), state.activity_open())
        };
        self.cache.connection.observe(&connection_status);
        let show_connection_chip =
            !matches!(connection_status, backend::ConnectionStatus::Connected)
                && !crate::benchmark_profile::network_lanes_suppressed()
                && !backend::irc::get_desired_channels().is_empty();
        let cached = self.cache.snapshot();

        let viewport_width: f32 = window.viewport_size().width.into();
        let window_width = if viewport_width > 0.0 {
            viewport_width
        } else {
            self.split_layout.read(cx).last_window_width()
        };
        let constraints = InlineBrandingConstraints {
            inline_tabs,
            window_width,
            logo_allowed: window_width >= SHOW_LOGO_MIN_WIDTH_PX,
            app_title_allowed: window_width >= SHOW_APP_TITLE_MIN_WIDTH_PX,
            version_allowed: window_width >= SHOW_VERSION_MIN_WIDTH_PX,
            show_activity_label: window_width >= SHOW_ACTIVITY_LABEL_MIN_WIDTH_PX,
            tab_demand,
        };
        let branding = self.cache.resolve_inline_branding_visibility(constraints);

        let hide_avatars =
            cached.show_account_identity && backend::streamer_mode::should_hide_avatars();
        let frame = TopBarFrame {
            titlebar_height: backend::settings::titlebar_height_px(),
            connection_status,
            connection_timeline: self.cache.connection,
            show_connection_chip,
            activity_open,
            cached,
            branding,
            show_activity_label: constraints.show_activity_label,
            inline_tabs,
            account_switcher_open: self.overlay_manager.read(cx).account_switcher_open,
            hide_avatars,
            reduce_motion: cx.reduce_motion(),
            theme_key: colors::render_key(),
            window_controls: draws_window_controls(window),
        };
        self.tab_bar_height = tab_bar_height;
        let stale = self.rendered_frame.as_ref() != Some(&frame) || (inline_tabs && tab_bar_stale);
        self.frame = Some(frame);
        stale
    }

    /// The titlebar as a child view, cached unless `prepare` reported it stale.
    pub fn view_element(entity: &Entity<Self>, titlebar_height: f32, stale: bool) -> AnyElement {
        if stale {
            entity.clone().into_any_element()
        } else {
            entity
                .clone()
                .cached(
                    StyleRefinement::default()
                        .w_full()
                        .h(px(titlebar_height))
                        .flex_shrink_0(),
                )
                .into_any_element()
        }
    }
}

impl Render for TopBarView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(frame) = self.frame.clone() else {
            return div().into_any_element();
        };
        self.rendered_frame = Some(frame.clone());

        let TopBarFrame {
            titlebar_height,
            connection_status,
            connection_timeline,
            show_connection_chip,
            activity_open,
            cached,
            branding,
            show_activity_label,
            inline_tabs,
            account_switcher_open,
            window_controls,
            ..
        } = frame;
        // The titlebar only renders fresh, so the nested tab bar must too. An
        // uncached parent mount does not set GPUI's `refreshing` flag, and a
        // cached child would then replay prepaint ranges recorded before the
        // parent's last reuse.
        let tab_bar = inline_tabs
            .then(|| TabBarRenderer::view_element(&self.tab_bar, self.tab_bar_height, true));
        let has_activity =
            cached.notification_count > 0 || cached.mention_count > 0 || cached.unread_whispers > 0;
        let update_attention = cached.update_attention;
        let settings_target =
            update_attention.then_some(crate::components::settings::SettingsSectionId::Updates);
        let logo = branding
            .show_logo
            .then(|| render_logo(&connection_status, 18.0, window, cx));
        let show_app_title = branding.show_app_title;
        let show_version = branding.show_version;
        let activity_count = cached
            .notification_count
            .saturating_add(cached.mention_count)
            .saturating_add(cached.unread_whispers as usize);
        let hover_tab_bar = self.tab_bar.clone();
        let activity_app_state = self.app_state.clone();
        let settings_app = self.app.clone();

        div()
            .id("titlebar")
            .relative()
            .h(px(titlebar_height))
            .flex_shrink_0()
            .w_full()
            .flex()
            .items_start()
            .justify_between()
            .bg(colors::bg_plate())
            .border_b_1()
            .border_color(colors::border_subtle())
            .child(
                div()
                    .id("titlebar-drag-surface")
                    .absolute()
                    .inset_0()
                    .cursor(CursorStyle::OpenHand)
                    .on_mouse_down(MouseButton::Left, |_event, window, _cx| {
                        if crate::utils::handle_titlebar_click() {
                            crate::utils::toggle_maximize(window);
                        }
                    })
                    .on_mouse_move(move |event: &MouseMoveEvent, _window, cx| {
                        if inline_tabs {
                            hover_tab_bar.update(cx, |renderer, cx| {
                                renderer.update_hovered_tab_from_position(event.position, cx);
                            });
                        }
                        if crate::utils::should_start_drag() {
                            #[cfg(target_os = "windows")]
                            crate::utils::start_window_drag();
                            #[cfg(not(target_os = "windows"))]
                            _window.start_window_move();
                        }
                    }),
            )
            .on_mouse_move(|_event: &MouseMoveEvent, _window, _cx| {
                if crate::utils::should_start_drag() {
                    #[cfg(target_os = "windows")]
                    crate::utils::start_window_drag();
                    #[cfg(not(target_os = "windows"))]
                    _window.start_window_move();
                }
            })
            .on_mouse_up(MouseButton::Left, |_event, _window, _cx| {
                crate::utils::cancel_pending_drag();
            })
            .child(
                div()
                    .id("titlebar-left")
                    .flex()
                    .items_center()
                    .flex_shrink_0()
                    .h(px(titlebar_height))
                    .px(px(Spacing::SM))
                    .gap(px(Spacing::XS_PLUS))
                    .when_some(logo, |s, logo| {
                        let tooltip = connection_timeline.tooltip(&connection_status);
                        s.child(
                            div()
                                .id("titlebar-logo")
                                .on_hover(move |hovered, window, _app| {
                                    let source = "titlebar-logo".to_string();
                                    if *hovered {
                                        show_text_tooltip(
                                            window.window_handle(),
                                            source,
                                            tooltip.clone(),
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source,
                                        );
                                    }
                                })
                                .child(logo),
                        )
                    })
                    .when(show_app_title, |s| {
                        s.child(
                            div()
                                .flex()
                                .items_end()
                                .gap(px(Spacing::XS_PLUS))
                                .child(
                                    div()
                                        .text_size(px(FontSize::MD))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(colors::text_primary())
                                        .child(cached.app_title.clone()),
                                )
                                .when(show_version, |title| {
                                    title.child(
                                        div()
                                            .text_size(px(FontSize::XS))
                                            .text_color(colors::text_secondary())
                                            .pb(px(Spacing::HAIRLINE))
                                            .child(identity::VERSION_LABEL),
                                    )
                                }),
                        )
                    })
                    .when(show_connection_chip, |s| {
                        s.child(render_connection_chip(
                            &connection_status,
                            connection_timeline,
                        ))
                    }),
            )
            .when_some(tab_bar, |titlebar, tab_bar| {
                titlebar.child(
                    div()
                        .id("titlebar-tabbar-slot")
                        .flex_1()
                        .min_w_0()
                        .h(px(titlebar_height))
                        .flex()
                        .items_center()
                        .px(px(Spacing::XS))
                        .child(tab_bar),
                )
            })
            .child(
                div()
                    .id("titlebar-right")
                    .flex()
                    .flex_shrink_0()
                    .items_center()
                    .h(px(titlebar_height))
                    .pr(px(if window_controls {
                        Spacing::XXS
                    } else {
                        Spacing::SM
                    }))
                    .gap(px(Spacing::XXS))
                    .child(
                        div()
                            .id("btn-activity-wrapper")
                            .relative()
                            .on_hover({
                                let source_id = "topbar-activity".to_string();
                                move |hovered, window, _app| {
                                    if *hovered {
                                        show_hotkey_hint_tooltip(
                                            window.window_handle(),
                                            source_id.clone(),
                                            HotkeyAction::ToggleSidebar,
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source_id,
                                        );
                                    }
                                }
                            })
                            .child(
                                icon_button_style()
                                    .id("btn-activity")
                                    .relative()
                                    .w(px(if show_activity_label {
                                        ACTIVITY_BUTTON_EXPANDED_PX
                                    } else {
                                        ACTIVITY_BUTTON_COMPACT_PX
                                    }))
                                    .h(px(26.0))
                                    .rounded(Radii::sm())
                                    .gap_1()
                                    .when(activity_open, |button| {
                                        button.bg(colors::accent().opacity(0.12))
                                    })
                                    .hover(|s| s.bg(colors::bg_hover()))
                                    .on_activate(move |_, _, cx| {
                                        activity_app_state.update(cx, |state, cx| {
                                            state.toggle_activity_panel(cx);
                                        });
                                        cx.stop_propagation();
                                    })
                                    .child(icon(
                                        IconName::Inbox,
                                        px(16.0),
                                        if activity_open {
                                            colors::accent()
                                        } else {
                                            colors::interactive_foreground()
                                        },
                                    ))
                                    .when(show_activity_label, |button| {
                                        button.child(
                                            div()
                                                .text_size(px(FontSize::BASE))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(if activity_open {
                                                    colors::accent()
                                                } else {
                                                    colors::interactive_foreground()
                                                })
                                                .child("Activity"),
                                        )
                                    })
                                    .when(show_activity_label && activity_count > 0, |button| {
                                        button.child(
                                            div()
                                                .min_w(px(ACTIVITY_COUNT_PILL_HEIGHT_PX))
                                                .h(px(ACTIVITY_COUNT_PILL_HEIGHT_PX))
                                                .line_height(px(ACTIVITY_COUNT_PILL_HEIGHT_PX))
                                                .px_1()
                                                .rounded(Radii::full())
                                                .bg(colors::accent().opacity(0.16))
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .child(
                                                    div()
                                                        .text_size(px(FontSize::XS))
                                                        .font_weight(FontWeight::SEMIBOLD)
                                                        .text_color(colors::accent_text())
                                                        .child(if activity_count > 99 {
                                                            "99+".to_string()
                                                        } else {
                                                            activity_count.to_string()
                                                        }),
                                                ),
                                        )
                                    })
                                    .when(has_activity && !show_activity_label, |button| {
                                        button.child(
                                            div()
                                                .absolute()
                                                .top(px(2.0))
                                                .right(px(2.0))
                                                .w(px(7.0))
                                                .h(px(7.0))
                                                .rounded(Radii::full())
                                                .bg(colors::accent())
                                                .border_1()
                                                .border_color(colors::bg_plate()),
                                        )
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .id("account-switcher-wrapper")
                            .relative()
                            .on_hover({
                                let source_id = "topbar-account-switcher".to_string();
                                move |hovered, window, _app| {
                                    if *hovered {
                                        show_hotkey_hint_tooltip(
                                            window.window_handle(),
                                            source_id.clone(),
                                            HotkeyAction::ToggleAccountSwitcher,
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source_id,
                                        );
                                    }
                                }
                            })
                            .child(render_account_switcher(
                                self.app.clone(),
                                account_switcher_open,
                                &cached,
                            )),
                    )
                    .child(
                        div()
                            .id("btn-settings-wrapper")
                            .relative()
                            .on_hover({
                                let source_id = "topbar-settings".to_string();
                                move |hovered, window, _app| {
                                    if *hovered {
                                        show_hotkey_hint_tooltip(
                                            window.window_handle(),
                                            source_id.clone(),
                                            HotkeyAction::ToggleSettings,
                                            window.mouse_position(),
                                        );
                                    } else {
                                        clear_global_tooltip_if_source_in_window(
                                            window.window_handle(),
                                            &source_id,
                                        );
                                    }
                                }
                            })
                            .child(
                                icon_button_style()
                                    .id("btn-settings")
                                    .relative()
                                    .w(px(24.0))
                                    .h(px(24.0))
                                    .rounded(Radii::sm())
                                    .hover(|s| s.bg(colors::bg_hover()))
                                    .on_activate(move |_, _, cx| {
                                        settings_app
                                            .update(cx, |this, cx| {
                                                this.open_settings(settings_target, cx);
                                            })
                                            .ok();
                                        cx.stop_propagation();
                                    })
                                    .child(icon(
                                        IconName::Settings,
                                        px(16.0),
                                        colors::interactive_foreground(),
                                    ))
                                    .when(update_attention, |button| {
                                        button.child(
                                            div()
                                                .absolute()
                                                .top(px(-2.0))
                                                .right(px(-2.0))
                                                .size(px(UPDATE_BADGE_SIZE_PX))
                                                .rounded(Radii::full())
                                                .bg(colors::accent())
                                                .border_1()
                                                .line_height(px(UPDATE_BADGE_SIZE_PX - 2.0))
                                                .border_color(colors::bg_plate())
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .text_size(px(FontSize::XS))
                                                .font_weight(FontWeight::BOLD)
                                                .text_color(colors::bg_void())
                                                .child("↓"),
                                        )
                                    }),
                            ),
                    )
                    .when(window_controls, |right| {
                        right.child(render_window_controls())
                    }),
            )
            .into_any_element()
    }
}

/// Render Windows-style window control buttons (minimize, maximize, close)
fn render_window_controls() -> impl IntoElement + use<> {
    let hover_bg = colors::bg_hover();

    div()
        .id("window-controls")
        .flex()
        .items_center()
        .h_full()
        .ml(px(Spacing::XXS))
        .child(
            window_control_button_style()
                .id("btn-minimize")
                .role(Role::Button)
                .aria_label("Minimize")
                .w(px(40.0))
                .hover(|s| s.bg(hover_bg))
                .on_press_click(|_, window, _| window.minimize_window())
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_primary())
                        .child("—"),
                ),
        )
        .child(
            window_control_button_style()
                .id("btn-maximize")
                .role(Role::Button)
                .aria_label("Maximize")
                .w(px(40.0))
                .hover(|s| s.bg(hover_bg))
                .on_press_click(|_, window, _| {
                    if window.is_maximized() {
                        crate::utils::restore_window(window);
                    } else {
                        window.zoom_window();
                    }
                })
                .child(
                    div()
                        .w(px(10.0))
                        .h(px(10.0))
                        .border_1()
                        .border_color(colors::text_primary())
                        .rounded(Radii::xs()),
                ),
        )
        .child(
            window_control_button_style()
                .id("btn-close")
                .role(Role::Button)
                .aria_label("Close Echo")
                .w(px(44.0))
                // The glyph inherits this, so the hover/active white wins in
                // every theme instead of a dark glyph on red.
                .text_color(colors::text_primary())
                .hover(|s| {
                    s.bg(colors::signal_red_close())
                        .text_color(colors::text_on_accent())
                })
                .on_press_click(|event, window, cx| {
                    if event.click_count() > 1 {
                        return;
                    }
                    #[cfg(windows)]
                    {
                        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
                        use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
                        use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
                        if let Ok(handle) = HasWindowHandle::window_handle(window)
                            && let RawWindowHandle::Win32(handle) = handle.as_raw()
                            && unsafe {
                                PostMessageW(
                                    Some(HWND(handle.hwnd.get() as *mut _)),
                                    WM_CLOSE,
                                    WPARAM(0),
                                    LPARAM(0),
                                )
                            }
                            .is_ok()
                        {
                            return;
                        }
                    }
                    let close_start = std::time::Instant::now();
                    crate::log_ui!(info, "Window close requested from top bar");
                    crate::config::flush_tab_saves(std::time::Duration::from_secs(2));
                    let backend_start = std::time::Instant::now();
                    backend::shutdown();
                    crate::log_ui!(
                        info,
                        elapsed_ms = backend_start.elapsed().as_millis(),
                        "Top bar close: backend shutdown returned"
                    );
                    window.remove_window();
                    cx.quit();
                    crate::log_ui!(
                        info,
                        elapsed_ms = close_start.elapsed().as_millis(),
                        "Top bar close handling complete"
                    );
                })
                .active(|s| {
                    s.bg(colors::signal_red_close_active())
                        .text_color(colors::text_on_accent())
                })
                .child(
                    div()
                        .text_size(px(FontSize::SM))
                        .child(crate::icons::CLOSE_GLYPH),
                ),
        )
}

/// Render account switcher button (profile avatar with warning badge)
fn render_account_switcher(
    app: WeakEntity<EchoApp>,
    is_open: bool,
    cached: &TopBarCachedState,
) -> impl IntoElement + use<> {
    let show_warning = cached.show_account_warning;

    div()
        .id("account-switcher-trigger")
        .relative()
        .w(px(28.0))
        .h(px(28.0))
        .rounded(Radii::sm())
        .cursor_pointer()
        .flex()
        .items_center()
        .justify_center()
        .hover(|s| s.bg(colors::hover_surface()))
        .when(is_open, |s| s.bg(colors::pressed_surface()))
        .on_activate(move |_event, _window, cx| {
            app.update(cx, |this, cx| {
                this.overlay_manager.update(cx, |overlays, _cx| {
                    overlays.toggle_account_switcher();
                });
                cx.notify();
            })
            .ok();
            cx.stop_propagation();
        })
        .child(if cached.show_account_identity {
            render_profile_avatar(
                cached
                    .account_profile_image_url
                    .as_ref()
                    .map(SharedString::as_ref),
                cached.account_avatar_label.as_ref(),
                18.0,
            )
            .into_any_element()
        } else {
            div()
                .text_sm()
                .text_color(colors::text_secondary())
                .child("👤")
                .into_any_element()
        })
        .when(show_warning, |s| {
            s.child(
                div()
                    .absolute()
                    .top(px(-2.0))
                    .right(px(-2.0))
                    .w(px(8.0))
                    .h(px(8.0))
                    .rounded(Radii::full())
                    .bg(colors::warning())
                    .border_1()
                    .border_color(colors::bg_elevated()),
            )
        })
        .into_any_element()
}

/// Render account dropdown as app-level overlay (renders on top of everything)
pub fn render_account_dropdown(
    overlay_manager: &Entity<OverlayManager>,
    menu: &MenuHost,
    chrome_height: f32,
    window: &Window,
    cx: &mut Context<EchoApp>,
) -> impl IntoElement + use<> {
    if !overlay_manager.read(cx).account_switcher_open {
        return div().into_any_element();
    }

    clear_global_tooltip();

    let auth_state = backend::get_auth_state();
    let accounts = backend::get_accounts();
    let is_logging_in = auth_state.loading || auth_state.user_code.is_some();
    let anonymous_mode = backend::bridge::is_anonymous_mode()
        || backend::get_settings()
            .map(|s| s.anonymous_mode)
            .unwrap_or(false);
    let viewport = window.viewport_size();
    let viewport_width = f32::from(viewport.width);
    let available_height = (f32::from(viewport.height) - chrome_height).max(0.0);
    let dropdown_fit = crate::utils::fit_overlay_to_viewport(
        size(viewport.width, px(available_height)),
        220.0,
        available_height,
        8.0,
    );
    let maximum_right =
        (viewport_width - dropdown_fit.width - dropdown_fit.margin_x).max(dropdown_fit.margin_x);
    let right_offset = 170.0_f32.clamp(dropdown_fit.margin_x, maximum_right);
    let (account_rows, actions) = if is_logging_in {
        (
            crate::components::auth::render_device_code_inline(&auth_state).into_any_element(),
            Vec::new(),
        )
    } else {
        let (rows, actions) = crate::components::auth::render_account_list_inline(
            &accounts,
            anonymous_mode,
            &menu.nav,
        );
        (
            div()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    render_account_status_banner(&auth_state, &accounts, anonymous_mode)
                        .into_any_element(),
                )
                .child(rows)
                .into_any_element(),
            actions,
        )
    };

    div()
        .id("account-dropdown-overlay")
        .absolute()
        .top_0()
        .left_0()
        .right_0()
        .bottom_0()
        .occlude()
        .on_mouse_move(|_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_up(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .on_scroll(|_, _, cx| cx.stop_propagation())
        .on_mouse_down(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| {
                this.overlay_manager
                    .update(cx, |o, _| o.close_account_switcher());
                cx.notify();
                cx.stop_propagation();
            }),
        )
        .on_mouse_down(MouseButton::Right, |_, _, cx| cx.stop_propagation())
        .on_mouse_down(MouseButton::Middle, |_, _, cx| cx.stop_propagation())
        .child(
            menu_panel()
                .absolute()
                .top(px(chrome_height + 4.0))
                .right(px(right_offset))
                .w(px(dropdown_fit.width))
                .id("account-dropdown-panel")
                .max_h(px((dropdown_fit.height - 4.0).max(0.0)))
                .overflow_y_scroll()
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .menu_keyboard(&menu.focus, &menu.nav, actions, menu.close_handler())
                .child(
                    dropdown_header_style().child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(colors::text_primary())
                            .child("Accounts"),
                    ),
                )
                .child(account_rows),
        )
        .into_any_element()
}

fn render_account_status_banner(
    auth_state: &backend::AuthState,
    accounts: &[backend::Account],
    anonymous_mode: bool,
) -> impl IntoElement + use<> {
    let active_account = accounts.iter().find(|account| account.is_active);
    let hide_account_names = backend::streamer_mode::should_hide_account_names();

    let banner = if anonymous_mode {
        Some((
            "Anonymous chat active",
            "Select an account below or toggle Anonymous off to resume authenticated chat."
                .to_string(),
        ))
    } else if backend::bridge::active_account_needs_reauth() {
        Some((
            "Active account needs re-authentication",
            auth_state.error.clone().unwrap_or_else(|| {
                if hide_account_names {
                    "Log in again to restore the saved Twitch account.".to_string()
                } else {
                    let label = active_account
                        .and_then(|account| account.display_name.clone())
                        .unwrap_or_else(|| {
                            active_account
                                .map(|account| format!("@{}", account.username))
                                .unwrap_or_else(|| "the saved Twitch account".to_string())
                        });
                    format!("Log in again to restore {label}.")
                }
            }),
        ))
    } else if auth_state.error.is_some() {
        Some((
            "Last login attempt failed",
            auth_state.error.clone().unwrap_or_default(),
        ))
    } else {
        None
    };

    match banner {
        Some((title, detail)) => div()
            .mx_3()
            .mt_3()
            .px_3()
            .py_2()
            .rounded(Radii::md())
            .bg(colors::error().opacity(0.08))
            .border_1()
            .border_color(colors::error().opacity(0.24))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(colors::text_primary())
                            .child(title),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(colors::text_muted())
                            .child(detail),
                    ),
            )
            .into_any_element(),
        None => div().into_any_element(),
    }
}
