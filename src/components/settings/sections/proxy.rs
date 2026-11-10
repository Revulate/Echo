//! Network proxy settings backed by persisted settings.
//!
//! The saved URL intentionally outlives Direct mode. This lets a person turn
//! proxy routing off temporarily without having to re-enter a working URL.

use crate::backend;
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SelectOption, SettingsPageUi, quiet_action_button, select_buttons, setting_reset,
    setting_target, settings_defaults, settings_group,
};
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, normalize_single_line_text,
};
use crate::theme::colors;
use crate::theme::tokens::Spacing;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

const PROXY_EDITOR_DISCLOSURE: &str = "advanced-proxy-editor";

const PROXY_MODE: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.proxy-mode",
    label: "Proxy mode",
    aliases: &["direct connection", "manual proxy", "network"],
    control_id: "proxy-mode",
    disclosures: &[],
};

const PROXY_URL: SettingDescriptor = SettingDescriptor {
    id: "developer-tools.proxy-url",
    label: "Manual proxy URL",
    aliases: &["http", "https", "socks5", "url"],
    control_id: "settings-proxy-url-field",
    disclosures: &[PROXY_EDITOR_DISCLOSURE],
};

/// Advanced › Network.
pub const DEVELOPER_NETWORK_PROXY_SETTINGS: &[SettingDescriptor] = &[PROXY_MODE, PROXY_URL];

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProxySettingsField {
    Url,
}

#[derive(Clone, Default)]
struct ProxySettingsEditor {
    url: String,
    status: Option<String>,
    status_is_error: bool,
    status_is_transient: bool,
    transient_status_owner: Option<EntityId>,
    focused: Option<ProxySettingsField>,
    focused_owner: Option<EntityId>,
    /// A request to turn on Manual after the current URL draft is explicitly
    /// saved. The segmented control itself always reflects persisted routing.
    manual_enable_requested: bool,
    manual_enable_owner: Option<EntityId>,
    dirty: bool,
}

static PROXY_SETTINGS_EDITOR: LazyLock<EditorState<ProxySettingsEditor>> =
    LazyLock::new(|| EditorState::new("proxy", ProxySettingsEditor::default()));

struct ProxyEditorInputs {
    url: Entity<InputState>,
    _subscription: Subscription,
}

thread_local! {
    static PROXY_EDITOR_INPUTS: RefCell<HashMap<EntityId, ProxyEditorInputs>> = RefCell::new(HashMap::new());
}

/// Drop retained input entities as a settings host closes or leaves Advanced.
/// The editor state remains available for a recoverable draft.
pub(crate) fn release_proxy_inputs_for_host(owner: EntityId) {
    drop_proxy_input_for_host(owner);
    update_proxy_settings_editor_transient(|editor| {
        clear_proxy_session_intent_for_owner(editor, owner);
    });
}

fn drop_proxy_input_for_host(owner: EntityId) {
    let removed = PROXY_EDITOR_INPUTS
        .try_with(|inputs| inputs.borrow_mut().remove(&owner))
        .ok()
        .flatten();
    let had_input = removed.is_some();
    drop(removed);
    if had_input {
        update_proxy_settings_editor_transient(|editor| {
            clear_proxy_focus_for_owner(editor, owner);
        });
    }
}

/// Render Advanced › Network.
pub fn render_proxy_section<T: 'static>(
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let proxy_enabled = backend::settings::get_proxy_enabled();
    let proxy_url = backend::settings::get_proxy_url();
    let editor_snapshot = sync_proxy_editor(&proxy_url, proxy_enabled);
    let editor = editor_snapshot.draft;
    let owner = cx.entity_id();
    let editor_disclosure = ui.disclosure(PROXY_EDITOR_DISCLOSURE, cx);
    let editor_visible = proxy_editor_is_visible(
        proxy_enabled,
        pending_manual_enable_for_owner(&editor, owner),
        editor_disclosure.is_expanded(),
    );

    if editor_visible {
        ensure_proxy_input(&editor, window, cx);
    } else {
        drop_proxy_input_for_host(owner);
    }

    let defaults = settings_defaults();
    let mode_modified = proxy_enabled != defaults.proxy_enabled;
    let url_modified = proxy_url != defaults.proxy_url;
    let status = editor.status.clone();
    let status_is_error = editor.status_is_error || editor_snapshot.needs_review;
    // The networking layer may retain a detailed parser error. The settings
    // surface never renders it because it can include URL userinfo.
    let routing_error = backend::http::proxy_config_error()
        .map(|_| "Proxy configuration is invalid. Network requests are blocked until it is fixed.");
    let mode = if proxy_enabled { "manual" } else { "direct" };
    let input = proxy_editor_input(cx);

    settings_group(
        "advanced-network",
        "Network",
        true,
        div()
            .w_full()
            .min_w_0()
            .flex()
            .flex_col()
            .gap(px(Spacing::SM))
            .child(setting_target(
                PROXY_MODE,
                crate::components::settings::widgets::setting_compact_row(
                    PROXY_MODE.id,
                    PROXY_MODE.label,
                    Some("Choose Echo's network path."),
                    select_buttons(
                        PROXY_MODE.control_id,
                        &[
                            SelectOption::new("direct", "Direct"),
                            SelectOption::new("manual", "Manual"),
                        ],
                        mode,
                        cx,
                        move |value, cx| {
                            if value == "direct" {
                                backend::settings::set_proxy_enabled(false);
                                update_proxy_settings_editor_transient(|editor| {
                                    clear_proxy_focus_for_owner(editor, cx.entity_id());
                                    clear_proxy_session_intent_for_owner(editor, cx.entity_id());
                                });
                            } else {
                                select_manual_proxy(cx.entity_id());
                            }
                            cx.notify();
                        },
                    ),
                    mode_modified,
                    mode_modified.then(|| {
                        setting_reset("reset-proxy-mode", PROXY_MODE.label, cx, |cx| {
                            let default = settings_defaults().proxy_enabled;
                            backend::settings::set_proxy_enabled(default);
                            update_proxy_settings_editor_transient(|editor| {
                                clear_proxy_focus_for_owner(editor, cx.entity_id());
                                clear_proxy_session_intent_for_owner(editor, cx.entity_id());
                            });
                            cx.notify();
                        })
                    }),
                    ui.stacked,
                ),
            ))
            .when(editor_visible, |content| {
                content.child(setting_target(
                    PROXY_URL,
                    div()
                        .w_full()
                        .min_w_0()
                        .flex()
                        .flex_col()
                        .gap(px(Spacing::SM))
                        .child(
                            div()
                                .w_full()
                                .min_w_0()
                                .flex()
                                .flex_col()
                                .gap(px(Spacing::SM))
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .items_center()
                                        .gap(px(Spacing::SM))
                                        .child(
                                            div()
                                                .text_size(px(
                                                    crate::components::settings::widgets::SettingsMetrics::CALM_LABEL_TEXT,
                                                ))
                                                .font_weight(FontWeight::MEDIUM)
                                                .text_color(colors::text_primary())
                                                .child(PROXY_URL.label),
                                        )
                                        .when(url_modified, |label| {
                                            label.child(
                                                div()
                                                    .text_size(px(
                                                        crate::components::settings::widgets::SettingsMetrics::CALM_HELPER_TEXT,
                                                    ))
                                                    .text_color(colors::text_secondary())
                                                    .child("Modified"),
                                            )
                                        }),
                                )
                                .child(
                                    div()
                                        .text_size(px(
                                            crate::components::settings::widgets::SettingsMetrics::CALM_HELPER_TEXT,
                                        ))
                                        .text_color(colors::text_secondary())
                                        .whitespace_normal()
                                        .child("Applies when Proxy mode is Manual. The saved URL is retained in Direct mode."),
                                )
                                .child(render_proxy_url_input(input.clone(), window, cx)),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_wrap()
                                .items_center()
                                .gap(px(Spacing::SM))
                                .child(quiet_action_button("proxy-save", "Save", false, cx, move |cx| {
                                    commit_proxy_editor(editor_snapshot.generation, owner);
                                    cx.notify();
                                }))
                                .child(quiet_action_button("proxy-paste-url", "Paste URL", false, cx, move |cx| {
                                    paste_proxy_url(owner, cx);
                                    cx.notify();
                                }))
                                .child(quiet_action_button("proxy-clear", "Clear", true, cx, |cx| {
                                    backend::settings::set_proxy_url(String::new());
                                    backend::settings::set_proxy_enabled(false);
                                    replace_proxy_editor("Manual proxy cleared");
                                    cx.notify();
                                }))
                                .when(url_modified, |actions| {
                                    actions.child(setting_reset("reset-proxy-url", PROXY_URL.label, cx, move |cx| {
                                        backend::settings::set_proxy_url(defaults.proxy_url.clone());
                                        backend::settings::set_proxy_enabled(defaults.proxy_enabled);
                                        replace_proxy_editor("Manual proxy URL reset");
                                        cx.notify();
                                    }))
                                }),
                        )
                        .when_some(status, |body, status| {
                            body.child(
                                div()
                                    .text_xs()
                                    .text_color(if status_is_error { colors::error() } else { colors::text_muted() })
                                    .child(status),
                            )
                        })
                        .when(pending_manual_enable_for_owner(&editor, owner), |body| {
                            body.child(
                                div()
                                    .text_xs()
                                    .text_color(colors::text_secondary())
                                    .child("Manual mode will turn on when you save this URL."),
                            )
                        })
                        .when_some(routing_error, |body, message| {
                            body.child(div().text_xs().text_color(colors::error()).child(message))
                        })
                        .child(
                            div()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child("Supported schemes: http://, https://, socks5://, socks5h://."),
                        )
                        .child(
                            div()
                                .max_w(px(640.0))
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child("This proxy is used for HTTP clients plus proxy-aware WebSocket and IRC connections, including HTTPS CONNECT tunnels."),
                        ),
                ))
            }),
    )
}

fn render_proxy_url_input<T: 'static>(
    input: Option<Entity<InputState>>,
    _window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .id(PROXY_URL.control_id)
        .w_full()
        .min_w_0()
        .h(px(
            crate::components::settings::widgets::SettingsMetrics::INPUT_MIN_HEIGHT,
        ))
        .font_family("monospace")
        .when_some(input, |body, input| {
            let input_for_escape = input.clone();
            body.capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                revert_proxy_url(&input_for_escape, window, cx);
                cx.stop_propagation();
                cx.notify();
            }))
            .child(
                Input::new(&input)
                    .accessibility_label(PROXY_URL.label)
                    .accessibility_secure(true),
            )
        })
}

fn recovered_proxy_editor(proxy_url: String) -> ProxySettingsEditor {
    ProxySettingsEditor {
        url: proxy_url,
        status: Some(
            "Proxy draft was reset after an internal editor error. Review or edit it before saving."
                .to_string(),
        ),
        status_is_error: true,
        status_is_transient: false,
        transient_status_owner: None,
        focused: None,
        focused_owner: None,
        manual_enable_requested: false,
        manual_enable_owner: None,
        dirty: false,
    }
}

fn update_proxy_settings_editor(update: impl FnOnce(&mut ProxySettingsEditor)) {
    let _ = PROXY_SETTINGS_EDITOR.apply_user_edit(
        |_| recovered_proxy_editor(backend::settings::get_proxy_url()),
        update,
    );
}

/// Update focus or local presentation without treating it as a draft edit.
/// In particular, a focus transition must not clear a recovered editor's
/// review state or replace its current status.
fn update_proxy_settings_editor_transient(update: impl FnOnce(&mut ProxySettingsEditor)) {
    let _ = PROXY_SETTINGS_EDITOR.sync_and_snapshot(
        |_| recovered_proxy_editor(backend::settings::get_proxy_url()),
        |editor| {
            update(editor);
            false
        },
    );
}

fn sync_proxy_editor(proxy_url: &str, proxy_enabled: bool) -> EditorSnapshot<ProxySettingsEditor> {
    PROXY_SETTINGS_EDITOR.sync_and_snapshot(
        |_| recovered_proxy_editor(proxy_url.to_string()),
        |editor| {
            if editor.focused.is_some()
                || editor.dirty
                || (editor.manual_enable_requested && !proxy_enabled)
            {
                return false;
            }
            if editor.url != proxy_url {
                editor.url = proxy_url.to_string();
                return true;
            }
            false
        },
    )
}

fn proxy_editor_snapshot() -> EditorSnapshot<ProxySettingsEditor> {
    PROXY_SETTINGS_EDITOR
        .snapshot_or_reset(|_| recovered_proxy_editor(backend::settings::get_proxy_url()))
}

fn replace_proxy_editor(status: impl Into<String>) {
    PROXY_SETTINGS_EDITOR.replace_from_authority(ProxySettingsEditor {
        url: backend::settings::get_proxy_url(),
        status: Some(status.into()),
        status_is_error: false,
        status_is_transient: false,
        transient_status_owner: None,
        focused: None,
        focused_owner: None,
        manual_enable_requested: false,
        manual_enable_owner: None,
        dirty: false,
    });
}

pub(super) fn reset_proxy_editor_after_settings_replace() {
    replace_proxy_editor("Proxy draft reset after settings were replaced");
}

fn proxy_editor_input<T: 'static>(cx: &Context<T>) -> Option<Entity<InputState>> {
    PROXY_EDITOR_INPUTS.with(|inputs| {
        inputs
            .borrow()
            .get(&cx.entity_id())
            .map(|inputs| inputs.url.clone())
    })
}

fn ensure_proxy_input<T: 'static>(
    editor: &ProxySettingsEditor,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let owner = cx.entity_id();
    if !PROXY_EDITOR_INPUTS.with(|inputs| inputs.borrow().contains_key(&owner)) {
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .with_placeholder("http://127.0.0.1:8080")
                .with_value(normalize_single_line_text(&editor.url))
        });
        let subscription = cx.subscribe_in(
            &input,
            window,
            move |_this, input, event: &InputEvent, window, cx| {
                match event {
                    InputEvent::Focus => update_proxy_settings_editor_transient(|editor| {
                        editor.focused = Some(ProxySettingsField::Url);
                        editor.focused_owner = Some(owner);
                    }),
                    InputEvent::Change => {
                        let raw = input.read(cx).value().to_string();
                        let value = normalize_single_line_text(&raw).trim().to_string();
                        if raw != value {
                            input
                                .update(cx, |input, cx| input.set_value(value.clone(), window, cx));
                        }
                        let snapshot = proxy_editor_snapshot();
                        if normalize_single_line_text(&snapshot.draft.url) == value {
                            return;
                        }
                        let authority = backend::settings::get_proxy_url();
                        update_proxy_settings_editor(|editor| {
                            editor.url = value;
                            editor.dirty = editor.url != authority;
                            clear_proxy_transient_status_for_owner(editor, owner);
                        });
                    }
                    InputEvent::PressEnter => {
                        let snapshot = proxy_editor_snapshot();
                        if snapshot.draft.dirty {
                            commit_proxy_editor(snapshot.generation, owner);
                        }
                    }
                    InputEvent::Blur => {
                        update_proxy_settings_editor_transient(|editor| {
                            clear_proxy_focus_for_owner(editor, owner);
                        });
                    }
                }
                cx.notify();
            },
        );
        PROXY_EDITOR_INPUTS.with(|inputs| {
            inputs.borrow_mut().insert(
                owner,
                ProxyEditorInputs {
                    url: input,
                    _subscription: subscription,
                },
            );
        });
    }
    sync_proxy_input(window, cx);
}

fn sync_proxy_input<T: 'static>(window: &mut Window, cx: &mut Context<T>) {
    let Some(input) = proxy_editor_input(cx) else {
        return;
    };
    let value = normalize_single_line_text(&proxy_editor_snapshot().draft.url);
    if input.read(cx).value() != value {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

fn select_manual_proxy(owner: EntityId) {
    let editor = proxy_editor_snapshot().draft;
    let saved_url = backend::settings::get_proxy_url();
    match saved_url_for_manual_enable(&editor, &saved_url) {
        Ok(_) => {
            backend::settings::set_proxy_enabled(true);
            replace_proxy_editor("Manual proxy enabled");
        }
        Err(status) => update_proxy_settings_editor_transient(|editor| {
            editor.manual_enable_requested = true;
            editor.manual_enable_owner = Some(owner);
            set_proxy_transient_status(editor, owner, status, true);
        }),
    }
}

/// Manual mode may only enable the persisted URL. A recovered or unsaved draft
/// must be saved first so routing never starts with a different value.
fn saved_url_for_manual_enable<'a>(
    editor: &ProxySettingsEditor,
    saved_url: &'a str,
) -> Result<&'a str, String> {
    if editor.dirty {
        return Err("Save the manual proxy URL before enabling Manual mode".to_string());
    }
    if saved_url.trim().is_empty() || validate_proxy_url(saved_url).is_err() {
        return Err("Enter a valid proxy URL before enabling Manual mode".to_string());
    }
    Ok(saved_url)
}

fn commit_proxy_editor(expected_generation: u64, owner: EntityId) {
    let Some(editor) = PROXY_SETTINGS_EDITOR.prepare_ready(expected_generation, |_| {
        recovered_proxy_editor(backend::settings::get_proxy_url())
    }) else {
        return;
    };
    let enable_manual = should_enable_manual_on_save(
        backend::settings::get_proxy_enabled(),
        pending_manual_enable_for_owner(&editor, owner),
    );
    match save_proxy_url_setting(&editor.url, enable_manual) {
        Ok(status) => {
            replace_proxy_editor(status);
        }
        Err(error) => update_proxy_settings_editor(|editor| {
            editor.status = Some(error);
            editor.status_is_error = true;
            editor.status_is_transient = false;
            editor.transient_status_owner = None;
        }),
    }
}

fn paste_proxy_url(owner: EntityId, cx: &App) {
    let Some(text) = crate::utils::read_text(cx) else {
        return;
    };
    let value = normalize_single_line_text(&text).trim().to_string();
    update_proxy_settings_editor(|editor| {
        editor.url = value.clone();
        editor.dirty = editor.url != backend::settings::get_proxy_url();
        clear_proxy_transient_status_for_owner(editor, owner);
    });
}

fn revert_proxy_url<T: 'static>(
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let saved = backend::settings::get_proxy_url();
    update_proxy_settings_editor(|editor| {
        editor.url = saved.clone();
        editor.dirty = false;
        clear_proxy_focus_for_owner(editor, cx.entity_id());
        clear_proxy_transient_status_for_owner(editor, cx.entity_id());
    });
    let value = normalize_single_line_text(&saved);
    if input.read(cx).value() != value {
        input.update(cx, |input, cx| input.set_value(value, window, cx));
    }
}

fn clear_proxy_focus_for_owner(editor: &mut ProxySettingsEditor, owner: EntityId) {
    if editor.focused_owner == Some(owner) {
        editor.focused = None;
        editor.focused_owner = None;
    }
}

fn set_proxy_transient_status(
    editor: &mut ProxySettingsEditor,
    owner: EntityId,
    status: String,
    is_error: bool,
) {
    editor.status = Some(status);
    editor.status_is_error = is_error;
    editor.status_is_transient = true;
    editor.transient_status_owner = Some(owner);
}

fn clear_proxy_transient_status_for_owner(editor: &mut ProxySettingsEditor, owner: EntityId) {
    if editor.status_is_transient && editor.transient_status_owner == Some(owner) {
        editor.status = None;
        editor.status_is_error = false;
        editor.status_is_transient = false;
        editor.transient_status_owner = None;
    }
}

fn clear_proxy_session_intent_for_owner(editor: &mut ProxySettingsEditor, owner: EntityId) {
    if editor.manual_enable_owner == Some(owner) {
        editor.manual_enable_requested = false;
        editor.manual_enable_owner = None;
        clear_proxy_transient_status_for_owner(editor, owner);
    }
}

fn proxy_editor_is_visible(
    proxy_enabled: bool,
    pending_manual_enable: bool,
    search_revealed: bool,
) -> bool {
    proxy_enabled || pending_manual_enable || search_revealed
}

fn pending_manual_enable_for_owner(editor: &ProxySettingsEditor, owner: EntityId) -> bool {
    editor.manual_enable_requested && editor.manual_enable_owner == Some(owner)
}

fn should_enable_manual_on_save(proxy_enabled: bool, pending_manual_enable: bool) -> bool {
    proxy_enabled || pending_manual_enable
}

fn save_proxy_url_setting(value: &str, enable_manual: bool) -> Result<String, String> {
    let trimmed = value.trim().trim_matches('"');
    if trimmed.is_empty() {
        return Err("Enter a proxy URL before enabling Manual mode".to_string());
    }
    validate_proxy_url(trimmed)?;
    backend::settings::set_proxy_url(trimmed.to_string());
    if enable_manual {
        backend::settings::set_proxy_enabled(true);
        Ok("Manual proxy saved and enabled".to_string())
    } else {
        Ok("Manual proxy URL saved; Direct connection remains selected".to_string())
    }
}

fn validate_proxy_url(value: &str) -> Result<(), String> {
    let parsed = url::Url::parse(value).map_err(|_| "Enter a valid proxy URL".to_string())?;
    match parsed.scheme().to_ascii_lowercase().as_str() {
        "http" | "https" | "socks5" | "socks5h" => {}
        _ => return Err("Use http://, https://, socks5://, or socks5h://".to_string()),
    }
    if parsed.host_str().is_none() {
        return Err("Proxy URL must include a host".to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn closing_another_host_keeps_the_active_proxy_input_focused() {
        let owner = EntityId::from(41_u64);
        let mut editor = ProxySettingsEditor {
            focused: Some(ProxySettingsField::Url),
            focused_owner: Some(owner),
            dirty: true,
            ..Default::default()
        };
        clear_proxy_focus_for_owner(&mut editor, EntityId::from(42_u64));
        assert!(editor.focused.is_some());
        clear_proxy_focus_for_owner(&mut editor, owner);
        assert!(editor.focused.is_none());
        assert!(editor.focused_owner.is_none());
        assert!(editor.dirty);
    }

    #[test]
    fn peer_mode_action_keeps_the_active_proxy_input_focused() {
        let owner = EntityId::from(41_u64);
        let peer = EntityId::from(42_u64);
        let mut editor = ProxySettingsEditor {
            focused: Some(ProxySettingsField::Url),
            focused_owner: Some(owner),
            ..Default::default()
        };
        // Direct and Reset both use this owner-scoped helper.
        clear_proxy_focus_for_owner(&mut editor, peer);
        assert!(editor.focused.is_some());
        assert!(editor.focused_owner == Some(owner));
    }

    #[test]
    fn proxy_validation_errors_never_echo_url_credentials() {
        for url in [
            "ftp://private-user:private-secret@proxy.test",
            "http://private-user:private-secret@",
            "private-user:private-secret@proxy.test",
        ] {
            let error = validate_proxy_url(url).unwrap_err();
            assert!(!error.contains("private-user"));
            assert!(!error.contains("private-secret"));
        }
    }

    #[test]
    fn manual_enable_requires_saving_a_pending_draft() {
        let editor = ProxySettingsEditor {
            url: "http://draft.proxy.test:8080".to_string(),
            dirty: true,
            ..Default::default()
        };
        assert_eq!(
            saved_url_for_manual_enable(&editor, "http://saved.proxy.test:8080"),
            Err("Save the manual proxy URL before enabling Manual mode".to_string())
        );
    }

    #[test]
    fn manual_enable_uses_the_saved_url_when_the_editor_is_clean() {
        let editor = ProxySettingsEditor {
            url: "not a URL".to_string(),
            ..Default::default()
        };
        assert_eq!(
            saved_url_for_manual_enable(&editor, "socks5://saved.proxy.test:1080"),
            Ok("socks5://saved.proxy.test:1080")
        );
    }

    #[test]
    fn direct_draft_save_stays_direct_without_manual_intent() {
        assert!(!should_enable_manual_on_save(false, false));
        assert!(should_enable_manual_on_save(false, true));
        assert!(!proxy_editor_is_visible(false, false, false));
        assert!(proxy_editor_is_visible(false, false, true));
    }

    #[test]
    fn closing_a_peer_preserves_the_pending_manual_intent() {
        let owner = EntityId::from(41_u64);
        let mut editor = ProxySettingsEditor {
            manual_enable_requested: true,
            manual_enable_owner: Some(owner),
            status: Some("Save first".to_string()),
            status_is_error: true,
            status_is_transient: true,
            transient_status_owner: Some(owner),
            ..Default::default()
        };
        clear_proxy_session_intent_for_owner(&mut editor, EntityId::from(42_u64));
        assert!(editor.manual_enable_requested);
        assert_eq!(editor.status.as_deref(), Some("Save first"));
        clear_proxy_session_intent_for_owner(&mut editor, owner);
        assert!(!editor.manual_enable_requested);
        assert!(editor.status.is_none());
    }

    #[test]
    fn peer_save_cannot_enable_another_hosts_pending_manual_request() {
        let owner = EntityId::from(41_u64);
        let peer = EntityId::from(42_u64);
        let editor = ProxySettingsEditor {
            manual_enable_requested: true,
            manual_enable_owner: Some(owner),
            ..Default::default()
        };
        assert!(pending_manual_enable_for_owner(&editor, owner));
        assert!(!pending_manual_enable_for_owner(&editor, peer));
        assert!(!should_enable_manual_on_save(
            false,
            pending_manual_enable_for_owner(&editor, peer)
        ));
    }
}
