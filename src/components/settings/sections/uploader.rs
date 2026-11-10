//! Uploader settings section.

use crate::backend::image_uploader::{
    ImageUploaderConfig, export_uploader_config_json, import_uploader_config_json,
};
use crate::components::settings::editor_state::{EditorSnapshot, EditorState};
use crate::components::settings::settings_navigation::SettingDescriptor;
use crate::components::settings::widgets::{
    SettingsMetrics, SettingsPageUi, empty_state, labelled_toggle, quiet_action_button,
    setting_reset, setting_reset_with_window, setting_row_with_control_width, setting_target,
    settings_defaults, settings_disclosure_with_layout, settings_group, settings_group_contents,
    settings_subheading, settings_subtopic, settings_summary_disclosure,
};
use crate::components::ui::ButtonExt;
use crate::components::ui::PressToClick;
use crate::components::ui::input::{Escape, Input, InputEvent, InputState};
use crate::components::ui::menu::{MenuAction, MenuKeyboard, MenuNav, menu_item, menu_panel};
use crate::theme::colors;
use crate::theme::tokens::{Radii, RadiiPixels, Spacing};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::cell::Cell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex, MutexGuard};

const MIB: u64 = 1024 * 1024;
const UPLOADS_MORE_DISCLOSURE: &str = "uploads-more";
const CUSTOM_PROVIDER_DISCLOSURE: &str = "uploads-custom-provider";
const UPLOAD_HISTORY_DISCLOSURE: &str = "uploads-history";

const UPLOADER_PROVIDER: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-provider",
    label: "Uploader provider",
    aliases: &["preset", "kappa", "nuuls", "olrite", "sharex", "custom"],
    control_id: "uploader-provider",
    disclosures: &[],
};

const UPLOADER_ENABLED: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-enabled",
    label: "Enable image uploader",
    aliases: &["upload", "image host"],
    control_id: "uploader-enabled",
    disclosures: &[],
};

const UPLOADER_PASTE: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-paste",
    label: "Paste uploads",
    aliases: &["clipboard", "paste"],
    control_id: "uploader-paste",
    disclosures: &[],
};

const UPLOADER_ASK_PASTE: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-ask-paste",
    label: "Ask before every paste upload",
    aliases: &["confirm", "clipboard", "paste", "prompt"],
    control_id: "uploader-ask-paste",
    disclosures: &[],
};

const UPLOADER_DRAG_DROP: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-drag-drop",
    label: "Drag and drop uploads",
    aliases: &["drop", "file"],
    control_id: "uploader-drag-drop",
    disclosures: &[UPLOADS_MORE_DISCLOSURE],
};

const UPLOADER_COPY_LINK: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-copy-link",
    label: "Copy link after upload",
    aliases: &["clipboard", "link"],
    control_id: "uploader-copy-link",
    disclosures: &[UPLOADS_MORE_DISCLOSURE],
};

const UPLOADER_CONFIRM_LARGE: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-confirm-large",
    label: "Confirm large uploads",
    aliases: &["max size", "confirm", "warn"],
    control_id: "uploader-confirm-large",
    disclosures: &[UPLOADS_MORE_DISCLOSURE],
};

const UPLOADER_REQUEST_URL: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-request-url",
    label: "Request URL",
    aliases: &["endpoint", "host", "api", "Custom provider configuration"],
    control_id: "uploader-request-url",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_FORM_FIELD: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-form-field",
    label: "Multipart form field",
    aliases: &["multipart", "field name"],
    control_id: "uploader-form-field",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_EXTRA_HEADERS: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-extra-headers",
    label: "Extra headers",
    aliases: &["header", "authorization", "token"],
    control_id: "uploader-extra-headers",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_MAX_BYTES: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-max-bytes",
    label: "Max upload bytes",
    aliases: &["size limit", "bytes", "mib"],
    control_id: "uploader-max-bytes",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_IMAGE_LINK: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-image-link",
    label: "Image link",
    aliases: &["response mapping", "json", "placeholder"],
    control_id: "uploader-image-link",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_DELETION_LINK: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-deletion-link",
    label: "Deletion link",
    aliases: &["response mapping", "delete", "json"],
    control_id: "uploader-deletion-link",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOAD_HISTORY: SettingDescriptor = SettingDescriptor {
    id: "links-media.upload-history",
    label: "Upload history",
    aliases: &["history", "recent uploads", "log", "Upload History"],
    control_id: "uploader-history",
    disclosures: &[UPLOAD_HISTORY_DISCLOSURE],
};

const UPLOADER_SAVE: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-save",
    label: "Save uploader configuration",
    aliases: &["save", "request mapping", "response mapping"],
    control_id: "uploader-button-save",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_IMPORT_PRESET: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-import-preset",
    label: "Import uploader preset",
    aliases: &["clipboard", "sharex", "preset"],
    control_id: "uploader-button-import-from-clipboard",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

const UPLOADER_EXPORT_PRESET: SettingDescriptor = SettingDescriptor {
    id: "links-media.uploader-export-preset",
    label: "Export uploader preset",
    aliases: &["clipboard", "sharex", "preset"],
    control_id: "uploader-button-export-to-clipboard",
    disclosures: &[CUSTOM_PROVIDER_DISCLOSURE],
};

/// Links & Media › Image uploads.
pub const LINKS_MEDIA_IMAGE_UPLOADS_SETTINGS: &[SettingDescriptor] = &[
    UPLOADER_PROVIDER,
    UPLOADER_ENABLED,
    UPLOADER_PASTE,
    UPLOADER_ASK_PASTE,
    UPLOADER_DRAG_DROP,
    UPLOADER_COPY_LINK,
    UPLOADER_CONFIRM_LARGE,
    UPLOADER_REQUEST_URL,
    UPLOADER_FORM_FIELD,
    UPLOADER_EXTRA_HEADERS,
    UPLOADER_MAX_BYTES,
    UPLOADER_IMAGE_LINK,
    UPLOADER_DELETION_LINK,
    UPLOAD_HISTORY,
    UPLOADER_SAVE,
    UPLOADER_IMPORT_PRESET,
    UPLOADER_EXPORT_PRESET,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum UploaderPreset {
    Kappa,
    Nuuls,
    OlriteRegular,
    OlritePreserved,
    OlriteAccount,
    OlriteAccountPreserved,
    Custom,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrimaryProviderSelection {
    ApplySavedPreset,
    OpenMappingEditor,
    RefusePendingDraft,
    AlreadyUsing,
}

fn primary_provider_selection(
    preset: UploaderPreset,
    saved_preset: UploaderPreset,
    has_pending_draft: bool,
) -> PrimaryProviderSelection {
    if preset == UploaderPreset::Custom {
        PrimaryProviderSelection::OpenMappingEditor
    } else if preset == saved_preset {
        PrimaryProviderSelection::AlreadyUsing
    } else if has_pending_draft {
        PrimaryProviderSelection::RefusePendingDraft
    } else {
        PrimaryProviderSelection::ApplySavedPreset
    }
}

impl UploaderPreset {
    const ALL: [Self; 7] = [
        Self::Kappa,
        Self::Nuuls,
        Self::OlriteRegular,
        Self::OlritePreserved,
        Self::OlriteAccount,
        Self::OlriteAccountPreserved,
        Self::Custom,
    ];

    fn label(self) -> &'static str {
        match self {
            Self::Kappa => "Kappa.lol (default)",
            Self::Nuuls => "i.nuuls.com",
            Self::OlriteRegular => "olrite.lol · 24 hours",
            Self::OlritePreserved => "olrite.lol · preserved",
            Self::OlriteAccount => "olrite.lol · account",
            Self::OlriteAccountPreserved => "olrite.lol · account + preserved",
            Self::Custom => "Custom",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Self::Kappa => "Kappa's published segs.lol endpoint · 100 MiB",
            Self::Nuuls => "Anonymous upload · raw URL response",
            Self::OlriteRegular => "Anonymous · automatic 24-hour expiry · 300 MiB",
            Self::OlritePreserved => "Anonymous · no automatic expiry · 50 MiB",
            Self::OlriteAccount => "Account history · replace API key placeholder · 300 MiB",
            Self::OlriteAccountPreserved => {
                "Account history + no automatic expiry · replace API key · 50 MiB"
            }
            Self::Custom => "Manually edited request and response mapping",
        }
    }

    fn base_mapping(self, current: &ImageUploaderConfig) -> ImageUploaderConfig {
        let mut config = current.clone();
        match self {
            Self::Kappa => {
                config.request_url = crate::backend::image_uploader::KAPPA_UPLOAD_URL.to_string();
                config.form_field = "file".to_string();
                config.extra_headers.clear();
                config.image_link_template =
                    crate::backend::image_uploader::KAPPA_IMAGE_LINK_TEMPLATE.to_string();
                config.deletion_link_template =
                    crate::backend::image_uploader::KAPPA_DELETION_LINK_TEMPLATE.to_string();
                config.max_upload_bytes = 100 * MIB;
            }
            Self::Nuuls => {
                config.request_url = "https://i.nuuls.com/upload".to_string();
                config.form_field = "attachment".to_string();
                config.extra_headers.clear();
                config.image_link_template.clear();
                config.deletion_link_template.clear();
                config.max_upload_bytes = 10 * MIB;
            }
            Self::OlriteRegular => {
                apply_olrite_base(&mut config, 300 * MIB);
                config.extra_headers.clear();
            }
            Self::OlritePreserved => {
                apply_olrite_base(&mut config, 50 * MIB);
                config.extra_headers = "X-Olrite-Preserve: true".to_string();
            }
            Self::OlriteAccount => {
                apply_olrite_base(&mut config, 300 * MIB);
                config.extra_headers = "X-API-Key: YOUR_API_KEY_HERE".to_string();
            }
            Self::OlriteAccountPreserved => {
                apply_olrite_base(&mut config, 50 * MIB);
                config.extra_headers =
                    "X-API-Key: YOUR_API_KEY_HERE ; X-Olrite-Preserve: true".to_string();
            }
            Self::Custom => {}
        }
        config
    }

    fn apply(self, current: &ImageUploaderConfig) -> Result<ImageUploaderConfig, String> {
        let mut config = self.base_mapping(current);
        if matches!(self, Self::OlriteAccount | Self::OlriteAccountPreserved)
            && matches!(
                Self::identify(current),
                Self::OlriteAccount | Self::OlriteAccountPreserved
            )
            && let Ok(headers) =
                crate::backend::image_uploader::parse_extra_headers(&current.extra_headers)
            && let Some((name, value)) = headers
                .into_iter()
                .find(|(name, _)| name.eq_ignore_ascii_case("X-API-Key"))
        {
            config.extra_headers = format!("{name}: {value}");
            if self == Self::OlriteAccountPreserved {
                config.extra_headers.push_str(" ; X-Olrite-Preserve: true");
            }
        }
        if has_placeholder_credential(&config) {
            config.enabled = false;
        }
        crate::backend::image_uploader::validate_config(&config)?;
        Ok(config)
    }

    fn identify(config: &ImageUploaderConfig) -> Self {
        builtin_preset_mappings()
            .iter()
            .find_map(|(preset, expected)| {
                preset_mapping_matches(*preset, config, expected, true).then_some(*preset)
            })
            .unwrap_or(Self::Custom)
    }
}

fn builtin_preset_mappings() -> &'static [(UploaderPreset, ImageUploaderConfig)] {
    static PRESETS: LazyLock<Vec<(UploaderPreset, ImageUploaderConfig)>> = LazyLock::new(|| {
        UploaderPreset::ALL
            .into_iter()
            .filter(|preset| *preset != UploaderPreset::Custom)
            .map(|preset| (preset, preset.base_mapping(&ImageUploaderConfig::default())))
            .collect()
    });
    &PRESETS
}

fn mapping_is_pristine(config: &ImageUploaderConfig) -> bool {
    builtin_preset_mappings()
        .iter()
        .any(|(preset, expected)| preset_mapping_matches(*preset, config, expected, false))
}

fn mapping_replacement_requires_confirmation(
    saved: &ImageUploaderConfig,
    next: &ImageUploaderConfig,
) -> bool {
    !mapping_is_pristine(saved) && !compatible_account_switch(saved, next)
}

fn compatible_account_switch(saved: &ImageUploaderConfig, next: &ImageUploaderConfig) -> bool {
    let before = UploaderPreset::identify(saved);
    let after = UploaderPreset::identify(next);
    if before == after
        || !matches!(
            before,
            UploaderPreset::OlriteAccount | UploaderPreset::OlriteAccountPreserved
        )
        || !matches!(
            after,
            UploaderPreset::OlriteAccount | UploaderPreset::OlriteAccountPreserved
        )
    {
        return false;
    }
    let account_key = |config: &ImageUploaderConfig| {
        crate::backend::image_uploader::parse_extra_headers(&config.extra_headers)
            .ok()?
            .into_iter()
            .find(|(name, _)| name.eq_ignore_ascii_case("X-API-Key"))
            .map(|(name, value)| (name.to_ascii_lowercase(), value))
    };
    account_key(saved) == account_key(next)
}

fn preset_mapping_matches(
    preset: UploaderPreset,
    actual: &ImageUploaderConfig,
    expected: &ImageUploaderConfig,
    allow_account_credentials: bool,
) -> bool {
    if actual.request_url.trim() != expected.request_url.trim()
        || actual.form_field.trim() != expected.form_field.trim()
        || actual.image_link_template.trim() != expected.image_link_template.trim()
        || actual.deletion_link_template.trim() != expected.deletion_link_template.trim()
        || actual.max_upload_bytes != expected.max_upload_bytes
    {
        return false;
    }
    let Ok(mut actual_headers) =
        crate::backend::image_uploader::parse_extra_headers(&actual.extra_headers)
    else {
        return false;
    };
    let Ok(mut expected_headers) =
        crate::backend::image_uploader::parse_extra_headers(&expected.extra_headers)
    else {
        return false;
    };
    for (name, value) in actual_headers.iter_mut().chain(expected_headers.iter_mut()) {
        *name = name.to_ascii_lowercase();
        if name == "x-api-key"
            && crate::backend::image_uploader::is_uploader_api_key_placeholder(value)
        {
            *value = crate::backend::image_uploader::UPLOADER_API_KEY_PLACEHOLDER.to_string();
        }
    }
    if allow_account_credentials
        && matches!(
            preset,
            UploaderPreset::OlriteAccount | UploaderPreset::OlriteAccountPreserved
        )
    {
        let credentials: Vec<_> = actual_headers
            .iter()
            .filter(|(name, _)| name == "x-api-key")
            .collect();
        if credentials.len() != 1 {
            return false;
        }
        if let Some(key) = expected_headers
            .iter_mut()
            .find(|(name, _)| name == "x-api-key")
        {
            *key = credentials[0].clone();
        }
    }
    actual_headers.sort();
    expected_headers.sort();
    actual_headers == expected_headers
}

pub(crate) fn uploader_confirmation_target(
    applied: bool,
    config: &ImageUploaderConfig,
) -> Option<&'static str> {
    (applied && has_placeholder_credential(config)).then_some(UPLOADER_EXTRA_HEADERS.id)
}

fn has_placeholder_credential(config: &ImageUploaderConfig) -> bool {
    crate::backend::image_uploader::has_placeholder_uploader_key(config)
}

fn mapping_confirmation_matches(
    saved: &ImageUploaderConfig,
    current: &ImageUploaderConfig,
    snapshot: &EditorSnapshot<UploaderEditorDraft>,
    generation: u64,
) -> bool {
    current == saved
        && snapshot.generation == generation
        && !uploader_mapping_change_blocked(snapshot)
}

fn apply_olrite_base(config: &mut ImageUploaderConfig, max_upload_bytes: u64) {
    config.request_url = "https://olrite.lol/api/upload".to_string();
    config.form_field = "file".to_string();
    config.image_link_template = "{url}".to_string();
    config.deletion_link_template = "{delete}".to_string();
    config.max_upload_bytes = max_upload_bytes;
}

#[derive(Clone)]
pub struct UploaderEditorInputs {
    request_url: Entity<InputState>,
    form_field: Entity<InputState>,
    extra_headers: Entity<InputState>,
    image_link_template: Entity<InputState>,
    deletion_link_template: Entity<InputState>,
    max_upload_bytes: Entity<InputState>,
    preset_menu_open: Rc<Cell<bool>>,
    placeholder_seen: Rc<Cell<bool>>,
    preset_menu_focus: FocusHandle,
    preset_menu_nav: MenuNav,
    _subscriptions: Rc<Vec<Subscription>>,
}

#[derive(Clone, Copy)]
enum UploaderField {
    RequestUrl,
    FormField,
    ExtraHeaders,
    ImageLink,
    DeletionLink,
    MaxBytes,
}

impl UploaderField {
    const ALL: [Self; 6] = [
        Self::RequestUrl,
        Self::FormField,
        Self::ExtraHeaders,
        Self::ImageLink,
        Self::DeletionLink,
        Self::MaxBytes,
    ];

    fn index(self) -> usize {
        match self {
            Self::RequestUrl => 0,
            Self::FormField => 1,
            Self::ExtraHeaders => 2,
            Self::ImageLink => 3,
            Self::DeletionLink => 4,
            Self::MaxBytes => 5,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::RequestUrl => UPLOADER_REQUEST_URL.label,
            Self::FormField => UPLOADER_FORM_FIELD.label,
            Self::ExtraHeaders => UPLOADER_EXTRA_HEADERS.label,
            Self::ImageLink => UPLOADER_IMAGE_LINK.label,
            Self::DeletionLink => UPLOADER_DELETION_LINK.label,
            Self::MaxBytes => UPLOADER_MAX_BYTES.label,
        }
    }
}

#[derive(Clone)]
struct UploaderEditorDraft {
    values: [String; 6],
    dirty: [bool; 6],
}

impl UploaderEditorDraft {
    fn from_config(config: &ImageUploaderConfig) -> Self {
        Self {
            values: uploader_config_values(config),
            dirty: [false; 6],
        }
    }

    fn value(&self, field: UploaderField) -> &str {
        &self.values[field.index()]
    }

    fn reconcile(&mut self, config: &ImageUploaderConfig) -> bool {
        let authority = uploader_config_values(config);
        let mut changed = false;
        for field in UploaderField::ALL {
            let index = field.index();
            if !self.dirty[index] && self.values[index] != authority[index] {
                self.values[index] = authority[index].clone();
                changed = true;
            }
        }
        changed
    }
}

static UPLOADER_EDITOR: LazyLock<EditorState<UploaderEditorDraft>> = LazyLock::new(|| {
    EditorState::new(
        "uploader",
        UploaderEditorDraft::from_config(&ImageUploaderConfig::default()),
    )
});

fn uploader_config_values(config: &ImageUploaderConfig) -> [String; 6] {
    [
        config.request_url.clone(),
        config.form_field.clone(),
        config.extra_headers.clone(),
        config.image_link_template.clone(),
        config.deletion_link_template.clone(),
        config.max_upload_bytes.to_string(),
    ]
}

#[cfg(test)]
mod uploader_draft_tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn untouched_field_follows_external_authority() {
        let mut authority = ImageUploaderConfig::default();
        let mut draft = UploaderEditorDraft::from_config(&authority);
        authority.form_field = "attachment".to_string();

        assert!(draft.reconcile(&authority));
        assert_eq!(draft.value(UploaderField::FormField), "attachment");
        assert!(!draft.dirty[UploaderField::FormField.index()]);
    }

    #[test]
    fn reset_authority_discards_a_pending_draft() {
        let mut pending = ImageUploaderConfig::default();
        pending.extra_headers = "Authorization: Bearer draft".to_string();
        let authority = ImageUploaderConfig::default();
        let mut draft = UploaderEditorDraft::from_config(&authority);
        draft.values[UploaderField::ExtraHeaders.index()] = pending.extra_headers;
        draft.dirty[UploaderField::ExtraHeaders.index()] = true;
        assert!(draft.dirty.iter().any(|dirty| *dirty));

        let reset = UploaderEditorDraft::from_config(&authority);
        assert!(!reset.dirty.iter().any(|dirty| *dirty));
        assert_eq!(reset.values, uploader_config_values(&authority));
    }

    #[test]
    fn dirty_fields_report_an_unsaved_draft() {
        let authority = ImageUploaderConfig::default();
        let mut draft = UploaderEditorDraft::from_config(&authority);
        draft.dirty[UploaderField::MaxBytes.index()] = true;
        assert!(draft.dirty.iter().any(|dirty| *dirty));
    }

    #[test]
    fn primary_provider_policy_applies_only_saved_presets() {
        assert_eq!(
            primary_provider_selection(UploaderPreset::Kappa, UploaderPreset::Nuuls, false),
            PrimaryProviderSelection::ApplySavedPreset
        );
        assert_eq!(
            primary_provider_selection(UploaderPreset::Kappa, UploaderPreset::Nuuls, true),
            PrimaryProviderSelection::RefusePendingDraft
        );
        assert_eq!(
            primary_provider_selection(UploaderPreset::Custom, UploaderPreset::Kappa, true),
            PrimaryProviderSelection::OpenMappingEditor
        );

        let saved = ImageUploaderConfig::default();
        let mut draft = UploaderEditorDraft::from_config(&saved);
        draft.values[UploaderField::RequestUrl.index()] = "https://draft.example/upload".into();
        draft.dirty[UploaderField::RequestUrl.index()] = true;
        assert_eq!(UploaderPreset::identify(&saved), UploaderPreset::Kappa);
        let pending = EditorSnapshot {
            draft,
            generation: 0,
            needs_review: false,
        };
        assert!(uploader_mapping_change_blocked(&pending));
        assert_eq!(
            primary_provider_selection(
                UploaderPreset::Nuuls,
                UploaderPreset::identify(&saved),
                uploader_mapping_change_blocked(&pending)
            ),
            PrimaryProviderSelection::RefusePendingDraft
        );
        assert!(
            uploader_pending_mapping_message(&saved, &pending)
                .expect("pending drafts need an actionable message")
                .contains("Request URL")
        );
    }

    #[test]
    fn customized_mapping_fields_are_identified_as_custom() {
        let base = ImageUploaderConfig::default();
        for preset in UploaderPreset::ALL
            .into_iter()
            .filter(|preset| *preset != UploaderPreset::Custom)
        {
            let mapping = preset.apply(&base).unwrap();
            assert_eq!(UploaderPreset::identify(&mapping), preset);
            let mut resized = mapping.clone();
            resized.max_upload_bytes += 1;
            assert_eq!(UploaderPreset::identify(&resized), UploaderPreset::Custom);
            let mut extra = mapping.clone();
            extra.extra_headers.push_str(" ; X-Custom: different");
            assert_eq!(UploaderPreset::identify(&extra), UploaderPreset::Custom);
        }
    }

    #[test]
    fn confirmed_placeholder_result_reveals_the_exact_mapping_field() {
        let mapping = UploaderPreset::OlriteAccount
            .apply(&ImageUploaderConfig::default())
            .unwrap();
        let id = uploader_confirmation_target(true, &mapping).unwrap();
        let target =
            crate::components::settings::settings_navigation::setting_target_by_id(id).unwrap();
        assert_eq!(target.setting.disclosures, &[CUSTOM_PROVIDER_DISCLOSURE]);
        assert!(uploader_confirmation_target(false, &mapping).is_none());
        assert!(uploader_confirmation_target(true, &ImageUploaderConfig::default()).is_none());
    }

    #[test]
    fn reselecting_an_account_preset_is_a_noop() {
        let mut saved = UploaderPreset::OlriteAccount
            .apply(&ImageUploaderConfig::default())
            .unwrap();
        saved.extra_headers = "X-API-Key: saved-test-key".into();
        let before = saved.clone();
        assert_eq!(
            primary_provider_selection(
                UploaderPreset::OlriteAccount,
                UploaderPreset::identify(&saved),
                false
            ),
            PrimaryProviderSelection::AlreadyUsing
        );
        assert_eq!(
            primary_provider_selection(
                UploaderPreset::OlriteAccount,
                UploaderPreset::identify(&saved),
                true
            ),
            PrimaryProviderSelection::AlreadyUsing
        );
        assert_eq!(saved, before);
    }

    #[test]
    fn account_preservation_switch_keeps_all_credential_headers() {
        let mut saved = UploaderPreset::OlriteAccount
            .apply(&ImageUploaderConfig::default())
            .unwrap();
        saved.extra_headers = "X-API-Key: saved-test-key".into();
        let next = UploaderPreset::OlriteAccountPreserved
            .apply(&saved)
            .unwrap();
        assert_eq!(
            crate::backend::image_uploader::parse_extra_headers(&next.extra_headers)
                .unwrap()
                .into_iter()
                .filter(|(name, _)| name == "X-API-Key")
                .collect::<Vec<_>>(),
            crate::backend::image_uploader::parse_extra_headers(&saved.extra_headers).unwrap()
        );
        assert!(next.extra_headers.contains("X-Olrite-Preserve: true"));
        assert!(!has_placeholder_credential(&next));
        assert!(!mapping_replacement_requires_confirmation(&saved, &next));
    }

    #[test]
    fn switching_to_another_provider_requires_credential_removal_confirmation() {
        let mut saved = UploaderPreset::OlriteAccount
            .apply(&ImageUploaderConfig::default())
            .unwrap();
        saved.extra_headers = "X-API-Key: saved-test-key".into();
        let next = UploaderPreset::Kappa.apply(&saved).unwrap();
        assert!(mapping_replacement_requires_confirmation(&saved, &next));
        assert!(saved.extra_headers.contains("saved-test-key"));
        let mut kappa = ImageUploaderConfig::default();
        kappa.extra_headers = "Authorization: Bearer saved-test-auth".into();
        let nuuls = UploaderPreset::Nuuls.apply(&kappa).unwrap();
        assert!(mapping_replacement_requires_confirmation(&kappa, &nuuls));
    }

    #[test]
    fn every_custom_or_unparseable_saved_mapping_requires_confirmation() {
        let pristine = ImageUploaderConfig::default();
        let next = UploaderPreset::Nuuls.apply(&pristine).unwrap();
        assert!(!mapping_replacement_requires_confirmation(&pristine, &next));
        for (url, headers, field) in [
            ("https://upload.example.test/?key=SECRET", "", "file"),
            ("https://user:pass@upload.example.test/upload", "", "file"),
            (pristine.request_url.as_str(), "X-Session: sid-test", "file"),
            (
                pristine.request_url.as_str(),
                "X-Upload-Pass: pass-test",
                "file",
            ),
            (pristine.request_url.as_str(), "Sid: sid-test", "file"),
            (pristine.request_url.as_str(), "", "edited-multipart"),
            ("upload.example.com", "Authorization Bearer abc", "file"),
        ] {
            let saved = ImageUploaderConfig {
                request_url: url.into(),
                extra_headers: headers.into(),
                form_field: field.into(),
                ..pristine.clone()
            };
            assert!(mapping_replacement_requires_confirmation(&saved, &next));
            assert!(UploaderPreset::Kappa.apply(&saved).is_ok());
        }
    }

    #[test]
    fn authorization_is_custom_and_cannot_bypass_replacement_confirmation() {
        let mut saved = UploaderPreset::OlriteAccount
            .apply(&ImageUploaderConfig::default())
            .unwrap();
        saved.extra_headers = "Authorization: Bearer real-test-key".into();
        assert_eq!(UploaderPreset::identify(&saved), UploaderPreset::Custom);
        assert!(!mapping_is_pristine(&saved));
        let next = UploaderPreset::OlriteAccountPreserved
            .apply(&saved)
            .unwrap();
        assert!(mapping_replacement_requires_confirmation(&saved, &next));
        assert!(!compatible_account_switch(&saved, &next));
    }

    #[test]
    fn placeholder_only_account_mapping_is_pristine() {
        for preset in [
            UploaderPreset::OlriteAccount,
            UploaderPreset::OlriteAccountPreserved,
        ] {
            let saved = preset.apply(&ImageUploaderConfig::default()).unwrap();
            assert!(mapping_is_pristine(&saved));
            assert!(!mapping_replacement_requires_confirmation(
                &saved,
                &ImageUploaderConfig::default()
            ));
            let mut real_key = saved;
            real_key.extra_headers = "X-API-Key: real-test-key".into();
            assert!(!mapping_is_pristine(&real_key));
        }
    }

    #[test]
    fn silent_import_keeps_all_current_upload_preferences() {
        let mut saved = ImageUploaderConfig::default();
        saved.copy_link_after_upload = false;
        saved.ask_before_large_upload = false;
        let mut imported = UploaderPreset::Nuuls.base_mapping(&saved);
        imported.enabled = true;
        imported.paste_upload_enabled = true;
        imported.drag_drop_upload_enabled = true;
        imported.copy_link_after_upload = true;
        imported.ask_before_large_upload = true;
        let prepared = prepare_imported_mapping(&saved, imported);
        assert!(!prepared.enabled);
        assert!(!prepared.paste_upload_enabled);
        assert!(!prepared.drag_drop_upload_enabled);
        assert!(!prepared.copy_link_after_upload);
        assert!(!prepared.ask_before_large_upload);
        assert_eq!(prepared.request_url, "https://i.nuuls.com/upload");
    }

    #[test]
    fn disabling_never_validates_a_legacy_malformed_mapping() {
        let saved = ImageUploaderConfig {
            enabled: true,
            request_url: "upload.example.com".into(),
            extra_headers: "Authorization Bearer abc".into(),
            ..ImageUploaderConfig::default()
        };
        let snapshot = EditorSnapshot {
            draft: UploaderEditorDraft::from_config(&saved),
            generation: 0,
            needs_review: false,
        };
        let next = prepare_enabled_config(&saved, &snapshot, false).unwrap();
        assert!(!next.enabled);
        assert_eq!(next.extra_headers, saved.extra_headers);
    }

    #[test]
    fn placeholder_presets_stay_disabled_and_cannot_be_enabled() {
        let mut saved = ImageUploaderConfig::default();
        saved.enabled = true;
        let next = UploaderPreset::OlriteAccount.apply(&saved).unwrap();
        assert!(has_placeholder_credential(&next));
        assert!(!next.enabled);
        let clean = EditorSnapshot {
            draft: UploaderEditorDraft::from_config(&next),
            generation: 0,
            needs_review: false,
        };
        assert!(
            prepare_enabled_config(&next, &clean, true)
                .unwrap_err()
                .contains("Replace YOUR_API_KEY_HERE")
        );
    }

    #[test]
    fn credential_confirmation_rejects_changed_authority_or_drafts() {
        let saved = ImageUploaderConfig::default();
        let mut snapshot = EditorSnapshot {
            draft: UploaderEditorDraft::from_config(&saved),
            generation: 3,
            needs_review: false,
        };
        assert!(mapping_confirmation_matches(&saved, &saved, &snapshot, 3));
        let mut changed = saved.clone();
        changed.extra_headers = "Authorization: Bearer changed-test-key".into();
        assert!(!mapping_confirmation_matches(
            &saved, &changed, &snapshot, 3
        ));
        assert!(!mapping_confirmation_matches(&saved, &saved, &snapshot, 2));
        snapshot.draft.dirty[UploaderField::ExtraHeaders.index()] = true;
        assert!(!mapping_confirmation_matches(&saved, &saved, &snapshot, 3));
        assert!(uploader_mapping_change_blocked(&snapshot));
        snapshot.draft.dirty.fill(false);
        snapshot.needs_review = true;
        assert!(uploader_mapping_change_blocked(&snapshot));
    }

    #[test]
    fn enable_preparation_uses_only_the_saved_mapping() {
        let mut saved = ImageUploaderConfig::default();
        saved.enabled = false;
        saved.request_url = "https://saved.example/upload".into();
        saved.extra_headers = "Authorization: Bearer saved".into();
        let clean = EditorSnapshot {
            draft: UploaderEditorDraft::from_config(&saved),
            generation: 0,
            needs_review: false,
        };

        let enabled = prepare_enabled_config(&saved, &clean, true)
            .expect("a clean saved mapping can be enabled");
        assert!(enabled.enabled);
        assert_eq!(enabled.request_url, saved.request_url);
        assert_eq!(enabled.extra_headers, saved.extra_headers);

        let mut valid_draft = clean.clone();
        valid_draft.draft.values[UploaderField::RequestUrl.index()] =
            "https://draft.example/upload".into();
        valid_draft.draft.dirty[UploaderField::RequestUrl.index()] = true;
        assert!(
            prepare_enabled_config(&saved, &valid_draft, true)
                .expect_err("enabling must not persist a valid pending draft")
                .contains("Request URL")
        );

        let mut invalid_draft = clean.clone();
        invalid_draft.draft.values[UploaderField::MaxBytes.index()] = "abc".into();
        invalid_draft.draft.dirty[UploaderField::MaxBytes.index()] = true;
        assert!(
            prepare_enabled_config(&saved, &invalid_draft, true)
                .expect_err("enabling must name an invalid pending field")
                .contains("Max upload bytes")
        );

        let disabled = prepare_enabled_config(&saved, &invalid_draft, false)
            .expect("disabling only changes the saved enabled flag");
        assert!(!disabled.enabled);
        assert_eq!(disabled.request_url, saved.request_url);
        assert_eq!(
            invalid_draft.draft.values[UploaderField::MaxBytes.index()],
            "abc"
        );
    }
}

fn uploader_editor_snapshot(config: &ImageUploaderConfig) -> EditorSnapshot<UploaderEditorDraft> {
    UPLOADER_EDITOR.sync_and_snapshot(
        |_| UploaderEditorDraft::from_config(config),
        |draft| draft.reconcile(config),
    )
}

fn uploader_editor_has_pending(snapshot: &EditorSnapshot<UploaderEditorDraft>) -> bool {
    snapshot.needs_review || snapshot.draft.dirty.iter().any(|dirty| *dirty)
}

fn uploader_mapping_change_blocked(snapshot: &EditorSnapshot<UploaderEditorDraft>) -> bool {
    uploader_editor_has_pending(snapshot)
}

fn uploader_pending_mapping_message(
    config: &ImageUploaderConfig,
    snapshot: &EditorSnapshot<UploaderEditorDraft>,
) -> Option<String> {
    if snapshot.needs_review {
        return Some(
            "Review the recovered uploader draft before changing the saved mapping.".into(),
        );
    }
    let fields: Vec<_> = UploaderField::ALL
        .into_iter()
        .filter(|field| snapshot.draft.dirty[field.index()])
        .map(|field| field.label())
        .collect();
    if fields.is_empty() {
        return None;
    }
    match draft_to_config(config, &snapshot.draft) {
        Ok(_) => Some(format!(
            "Save or Reload the unsaved {} draft before changing the saved mapping.",
            fields.join(", ")
        )),
        Err(error) => Some(format!(
            "Fix the unsaved {} draft before enabling or changing presets: {error}",
            fields.join(", ")
        )),
    }
}

fn prepare_enabled_config(
    saved: &ImageUploaderConfig,
    draft: &EditorSnapshot<UploaderEditorDraft>,
    enabled: bool,
) -> Result<ImageUploaderConfig, String> {
    if enabled && uploader_mapping_change_blocked(draft) {
        return Err(uploader_pending_mapping_message(saved, draft)
            .unwrap_or_else(|| "Save or Reload the pending uploader draft first.".into()));
    }

    let mut next = saved.clone();
    next.enabled = enabled;
    if enabled && has_placeholder_credential(&next) {
        return Err("Replace YOUR_API_KEY_HERE with your API key before enabling.".into());
    }
    if enabled && next.max_upload_bytes == 0 {
        return Err("Saved Max upload bytes must be greater than zero before enabling.".into());
    }
    if enabled {
        crate::backend::image_uploader::validate_config(&next)?;
    }
    Ok(next)
}

fn draft_to_config(
    base: &ImageUploaderConfig,
    draft: &UploaderEditorDraft,
) -> Result<ImageUploaderConfig, String> {
    let max_upload_bytes = draft
        .value(UploaderField::MaxBytes)
        .trim()
        .parse::<u64>()
        .map_err(|error| format!("Invalid Max upload bytes: {error}"))?;
    if max_upload_bytes == 0 {
        return Err("Max upload bytes must be greater than zero".to_string());
    }

    let config = ImageUploaderConfig {
        enabled: base.enabled,
        paste_upload_enabled: base.paste_upload_enabled,
        drag_drop_upload_enabled: base.drag_drop_upload_enabled,
        copy_link_after_upload: base.copy_link_after_upload,
        ask_before_large_upload: base.ask_before_large_upload,
        max_upload_bytes,
        request_url: draft.value(UploaderField::RequestUrl).trim().to_string(),
        form_field: draft.value(UploaderField::FormField).trim().to_string(),
        extra_headers: draft.value(UploaderField::ExtraHeaders).trim().to_string(),
        image_link_template: draft.value(UploaderField::ImageLink).trim().to_string(),
        deletion_link_template: draft.value(UploaderField::DeletionLink).trim().to_string(),
    };
    crate::backend::image_uploader::validate_config(&config)?;
    Ok(config)
}

pub(crate) fn reset_uploader_editor_after_settings_replace() {
    let config = crate::backend::settings::get_image_uploader_config();
    UPLOADER_EDITOR.replace_from_authority(UploaderEditorDraft::from_config(&config));
    lock_uploader_status(&UPLOADER_STATUS).clear();
}

impl UploaderEditorInputs {
    pub(crate) fn close_provider_menu(&self, window: &mut Window) {
        if self.preset_menu_open.replace(false) {
            self.preset_menu_nav.reset();
            window.refresh();
        }
    }

    pub fn new<T: 'static>(window: &mut Window, cx: &mut Context<T>) -> Self {
        let config = crate::backend::settings::get_image_uploader_config();
        let draft = uploader_editor_snapshot(&config).draft;
        let request_url = new_input(
            draft.value(UploaderField::RequestUrl),
            "https://example.com/upload",
            window,
            cx,
        );
        let form_field = new_input(draft.value(UploaderField::FormField), "file", window, cx);
        let extra_headers = new_input(
            draft.value(UploaderField::ExtraHeaders),
            "Authorization: Bearer ... ; X-Key: value",
            window,
            cx,
        );
        let image_link_template = new_input(
            draft.value(UploaderField::ImageLink),
            "{data.url}",
            window,
            cx,
        );
        let deletion_link_template = new_input(
            draft.value(UploaderField::DeletionLink),
            "{data.deleteUrl}",
            window,
            cx,
        );
        let max_upload_bytes =
            new_input(draft.value(UploaderField::MaxBytes), "10485760", window, cx);
        let subscriptions = vec![
            subscribe_uploader_input(&request_url, UploaderField::RequestUrl, window, cx),
            subscribe_uploader_input(&form_field, UploaderField::FormField, window, cx),
            subscribe_uploader_input(&extra_headers, UploaderField::ExtraHeaders, window, cx),
            subscribe_uploader_input(&image_link_template, UploaderField::ImageLink, window, cx),
            subscribe_uploader_input(
                &deletion_link_template,
                UploaderField::DeletionLink,
                window,
                cx,
            ),
            subscribe_uploader_input(&max_upload_bytes, UploaderField::MaxBytes, window, cx),
        ];
        Self {
            request_url,
            form_field,
            extra_headers,
            image_link_template,
            deletion_link_template,
            max_upload_bytes,
            preset_menu_open: Rc::new(Cell::new(false)),
            placeholder_seen: Rc::new(Cell::new(false)),
            preset_menu_focus: cx.focus_handle(),
            preset_menu_nav: MenuNav::default(),
            _subscriptions: Rc::new(subscriptions),
        }
    }

    fn to_config<T: 'static>(
        &self,
        base: &ImageUploaderConfig,
        _cx: &Context<T>,
    ) -> Result<ImageUploaderConfig, String> {
        let snapshot = uploader_editor_snapshot(base);
        let draft = UPLOADER_EDITOR
            .prepare_ready(snapshot.generation, |_| {
                UploaderEditorDraft::from_config(base)
            })
            .ok_or_else(|| {
                "Review or edit the recovered configuration before saving.".to_string()
            })?;
        draft_to_config(base, &draft)
    }

    fn reload<T: 'static>(
        &self,
        config: &ImageUploaderConfig,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        UPLOADER_EDITOR.replace_from_authority(UploaderEditorDraft::from_config(config));
        self.sync_from_authority(config, window, cx);
        self.preset_menu_open.set(false);
    }

    fn sync_from_authority<T: 'static>(
        &self,
        config: &ImageUploaderConfig,
        window: &mut Window,
        cx: &mut Context<T>,
    ) {
        let snapshot = uploader_editor_snapshot(config);
        for (field, input) in [
            (UploaderField::RequestUrl, &self.request_url),
            (UploaderField::FormField, &self.form_field),
            (UploaderField::ExtraHeaders, &self.extra_headers),
            (UploaderField::ImageLink, &self.image_link_template),
            (UploaderField::DeletionLink, &self.deletion_link_template),
            (UploaderField::MaxBytes, &self.max_upload_bytes),
        ] {
            set_input_value(input, snapshot.draft.value(field), window, cx);
        }
    }
}

#[derive(Clone)]
struct UploaderStatus {
    message: String,
    is_error: bool,
    revision: u64,
}

static UPLOADER_STATUS: LazyLock<Mutex<HashMap<u64, UploaderStatus>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
static UPLOADER_STATUS_REVISION: AtomicU64 = AtomicU64::new(1);

fn lock_uploader_status(
    status: &Mutex<HashMap<u64, UploaderStatus>>,
) -> MutexGuard<'_, HashMap<u64, UploaderStatus>> {
    match status.lock() {
        Ok(status) => status,
        Err(poisoned) => {
            let mut status_guard = poisoned.into_inner();
            status_guard.clear();
            status.clear_poison();
            crate::log_ui!(warn, "Reset poisoned image uploader status");
            status_guard
        }
    }
}

fn new_input<T: 'static>(
    value: &str,
    placeholder: &'static str,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<InputState> {
    let value = value.to_string();
    cx.new(|cx| {
        InputState::new(window, cx)
            .with_placeholder(placeholder)
            .with_value(value)
    })
}

fn set_input_value<T: 'static>(
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let value = crate::components::ui::input::normalize_single_line_text(value);
    if input.read(cx).value() == value {
        return;
    }
    input.update(cx, |input, cx| {
        input.set_value(value.to_string(), window, cx);
    });
}

fn subscribe_uploader_input<T: 'static>(
    input: &Entity<InputState>,
    field: UploaderField,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Subscription {
    cx.subscribe_in(
        input,
        window,
        move |_this, input, event: &InputEvent, _window, cx| {
            if !matches!(event, InputEvent::Change) {
                return;
            }
            let value = input.read(cx).value().to_string();
            let config = crate::backend::settings::get_image_uploader_config();
            if crate::components::ui::input::normalize_single_line_text(
                uploader_editor_snapshot(&config).draft.value(field),
            ) == value
            {
                return;
            }
            let authority = uploader_config_values(&config);
            let _ = UPLOADER_EDITOR.apply_user_edit(
                |_| UploaderEditorDraft::from_config(&config),
                |draft| {
                    let index = field.index();
                    draft.values[index] = value;
                    draft.dirty[index] = draft.values[index] != authority[index];
                },
            );
            cx.notify();
        },
    )
}

fn revert_uploader_field<T: 'static>(
    field: UploaderField,
    input: &Entity<InputState>,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let config = crate::backend::settings::get_image_uploader_config();
    let authority = uploader_config_values(&config);
    UPLOADER_EDITOR.apply_authoritative_update(
        |_| UploaderEditorDraft::from_config(&config),
        |draft| {
            let index = field.index();
            draft.values[index] = authority[index].clone();
            draft.dirty[index] = false;
        },
    );
    set_input_value(input, &authority[field.index()], window, cx);
    set_status(
        cx.entity_id().as_u64(),
        format!("Reverted {} to the saved value", field.label()),
        false,
    );
    cx.notify();
}

pub(crate) fn clear_uploader_status(host: u64) {
    if lock_uploader_status(&UPLOADER_STATUS)
        .remove(&host)
        .is_some()
    {
        crate::backend::push_ui_action(crate::backend::UiAction::CancelUploaderConfirmation {
            host,
        });
    }
}

fn uploader_status(host: u64) -> Option<UploaderStatus> {
    lock_uploader_status(&UPLOADER_STATUS).get(&host).cloned()
}

fn write_status(host: u64, message: impl Into<String>, is_error: bool) -> u64 {
    let revision = UPLOADER_STATUS_REVISION.fetch_add(1, Ordering::Relaxed);
    lock_uploader_status(&UPLOADER_STATUS).insert(
        host,
        UploaderStatus {
            message: message.into(),
            is_error,
            revision,
        },
    );
    revision
}

fn set_status(host: u64, message: impl Into<String>, is_error: bool) {
    write_status(host, message, is_error);
}

pub(crate) fn resolve_uploader_status(host: u64, revision: u64, applied: bool) {
    if !applied && uploader_status(host).is_some_and(|status| status.revision == revision) {
        set_status(host, "Kept the current mapping", false);
    }
}

#[cfg(test)]
mod uploader_status_tests {
    use super::*;
    use core::prelude::v1::test;
    #[test]
    fn closing_one_host_keeps_the_other_status() {
        set_status(101, "First host", false);
        set_status(102, "Second host", false);
        clear_uploader_status(101);
        assert!(uploader_status(101).is_none());
        assert_eq!(uploader_status(102).unwrap().message, "Second host");
        clear_uploader_status(102);
    }
    #[test]
    fn replaced_confirmation_does_not_clear_newer_pending_status() {
        let old = write_status(103, "Old confirmation", false);
        let new = write_status(103, "New confirmation", false);
        resolve_uploader_status(103, old, false);
        assert_eq!(uploader_status(103).unwrap().revision, new);
        resolve_uploader_status(103, new, false);
        assert_eq!(
            uploader_status(103).unwrap().message,
            "Kept the current mapping"
        );
        clear_uploader_status(103);
    }

    #[test]
    fn closed_and_reopened_host_rejects_old_confirmation() {
        let old = write_status(104, "Awaiting confirmation", false);
        assert!(confirmation_request_is_live(104, old));
        clear_uploader_status(104);
        assert!(!confirmation_request_is_live(104, old));
        let reopened = write_status(104, "Reopened host", false);
        assert!(confirmation_request_is_live(104, reopened));
        assert!(!confirmation_request_is_live(104, old));
        clear_uploader_status(104);
    }
}

/// Render the Links & Media > Image uploads anchor body with real GPUI text
/// inputs. Modal and popout instances own separate input entities while both
/// persist through the shared backend configuration.
pub fn render_uploader_section<T: 'static>(
    inputs: &UploaderEditorInputs,
    ui: &mut SettingsPageUi,
    window: &mut Window,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let config = crate::backend::settings::get_image_uploader_config();
    inputs.sync_from_authority(&config, window, cx);
    let defaults = settings_defaults();
    let stacked = ui.stacked;
    let more = ui.disclosure(UPLOADS_MORE_DISCLOSURE, cx);
    let custom_provider = ui.disclosure(CUSTOM_PROVIDER_DISCLOSURE, cx);
    let placeholder = has_placeholder_credential(&config);
    if placeholder && !inputs.placeholder_seen.replace(placeholder) {
        custom_provider.set_expanded(true, window, cx);
        set_status(
            cx.entity_id().as_u64(),
            "Replace YOUR_API_KEY_HERE with your API key. The uploader remains disabled.",
            true,
        );
    } else if !placeholder {
        inputs.placeholder_seen.set(false);
    }
    let history = ui.disclosure(UPLOAD_HISTORY_DISCLOSURE, cx);
    let more_modified = usize::from(
        config.drag_drop_upload_enabled != defaults.image_uploader.drag_drop_upload_enabled,
    ) + usize::from(
        config.copy_link_after_upload != defaults.image_uploader.copy_link_after_upload,
    ) + usize::from(
        config.ask_before_large_upload != defaults.image_uploader.ask_before_large_upload,
    );
    let custom_draft = uploader_editor_snapshot(&config);
    let pending_mapping = uploader_editor_has_pending(&custom_draft);
    let custom_summary = if custom_draft.needs_review {
        "Needs review"
    } else if pending_mapping {
        "Unsaved draft"
    } else {
        "Active request and response mapping. Presets fill these fields; editing them makes the provider Custom."
    };
    let history_count = crate::backend::upload_history::len();
    let history_summary = if history_count == 0 {
        "No uploads".to_string()
    } else {
        format!(
            "{history_count} upload{}",
            if history_count == 1 { "" } else { "s" }
        )
    };

    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::GROUP_GAP))
        .child(settings_group(
            "uploads",
            "Uploads",
            true,
            settings_group_contents(
                "uploads-content",
                "Uploads",
                false,
                div()
                    .w_full()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .child(setting_target(
                        UPLOADER_ENABLED,
                        uploader_toggle_row(
                            UPLOADER_ENABLED,
                            (config.enabled, defaults.image_uploader.enabled),
                            "Use the selected provider for pasted or dropped images.",
                            stacked,
                            cx,
                            move |enabled, cx| {
                                persist_enabled_change(enabled, cx.entity_id().as_u64());
                                cx.notify();
                            },
                            move |cx| {
                                persist_enabled_change(
                                    defaults.image_uploader.enabled,
                                    cx.entity_id().as_u64(),
                                );
                                cx.notify();
                            },
                        ),
                    ))
                    .child(setting_target(
                        UPLOADER_PROVIDER,
                        setting_row_with_control_width(
                            UPLOADER_PROVIDER.id,
                            UPLOADER_PROVIDER.label,
                            Some(
                                "Choose a preset or use Custom for a request and response mapping.",
                            ),
                            render_provider_dropdown(inputs, &custom_provider, cx),
                            uploader_mapping_modified(&config, &defaults.image_uploader),
                            uploader_mapping_modified(&config, &defaults.image_uploader).then(
                                || {
                                    let inputs = inputs.clone();
                                    setting_reset_with_window(
                                        "reset-uploader-provider",
                                        UPLOADER_PROVIDER.label,
                                        cx,
                                        move |window, cx| {
                                            reset_uploader_mapping(&inputs, window, cx)
                                        },
                                    )
                                },
                            ),
                            stacked,
                            320.0,
                        ),
                    ))
                    .when_some(uploader_status(cx.entity_id().as_u64()), |body, status| {
                        body.child(render_status(status))
                    })
                    .child(setting_target(
                        UPLOADER_PASTE,
                        uploader_toggle_row(
                            UPLOADER_PASTE,
                            (
                                config.paste_upload_enabled,
                                defaults.image_uploader.paste_upload_enabled,
                            ),
                            "Upload image data pasted into chat.",
                            stacked,
                            cx,
                            |enabled, cx| {
                                persist_toggle_change(cx.entity_id().as_u64(), |config| {
                                    config.paste_upload_enabled = enabled
                                });
                                cx.notify();
                            },
                            move |cx| {
                                persist_toggle_change(cx.entity_id().as_u64(), |config| {
                                    config.paste_upload_enabled =
                                        defaults.image_uploader.paste_upload_enabled
                                });
                                cx.notify();
                            },
                        ),
                    ))
                    .child(setting_target(
                        UPLOADER_ASK_PASTE,
                        uploader_ask_paste_row(stacked, cx),
                    ))
                    .child(settings_disclosure_with_layout(
                        UPLOADS_MORE_DISCLOSURE,
                        "Upload options",
                        Some("Drag and drop, copied links, and large uploads"),
                        more_modified,
                        &more,
                        stacked,
                        cx,
                        move |cx| {
                            render_upload_options(
                                config,
                                defaults.image_uploader.clone(),
                                stacked,
                                cx,
                            )
                        },
                    )),
            ),
        ))
        .child(settings_group(
            "uploads-request-response-mapping-group",
            "Request and response mapping",
            false,
            settings_summary_disclosure(
                CUSTOM_PROVIDER_DISCLOSURE,
                "Request and response mapping",
                custom_summary,
                &custom_provider,
                cx,
                move |cx| render_custom_provider_editor(inputs, cx),
            ),
        ))
        .child(settings_group(
            "uploads-history-group",
            "Upload history",
            false,
            setting_target(
                UPLOAD_HISTORY,
                settings_summary_disclosure(
                    UPLOAD_HISTORY_DISCLOSURE,
                    "Upload history",
                    &history_summary,
                    &history,
                    cx,
                    render_upload_history,
                ),
            ),
        ))
}

fn uploader_toggle_row<
    T: 'static,
    F: Fn(bool, &mut Context<T>) + 'static,
    G: Fn(&mut Context<T>) + 'static,
>(
    descriptor: SettingDescriptor,
    (value, default): (bool, bool),
    help: &'static str,
    stacked: bool,
    cx: &mut Context<T>,
    on_change: F,
    on_reset: G,
) -> impl IntoElement + use<T, F, G> {
    crate::components::settings::widgets::setting_compact_row(
        descriptor.id,
        descriptor.label,
        Some(help),
        labelled_toggle(
            descriptor.control_id,
            descriptor.label,
            value,
            cx,
            on_change,
        ),
        value != default,
        (value != default).then(|| {
            setting_reset(
                format!("reset-{}", descriptor.control_id),
                descriptor.label,
                cx,
                on_reset,
            )
        }),
        stacked,
    )
}

fn uploader_ask_paste_row<T: 'static>(
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let value = crate::backend::settings::get_ask_before_paste_upload();
    let default = settings_defaults().ask_before_paste_upload;
    crate::components::settings::widgets::setting_compact_row(
        UPLOADER_ASK_PASTE.id,
        UPLOADER_ASK_PASTE.label,
        Some("Confirm every clipboard image before it is sent to the image host."),
        labelled_toggle(
            UPLOADER_ASK_PASTE.control_id,
            UPLOADER_ASK_PASTE.label,
            value,
            cx,
            |enabled, cx| {
                crate::backend::settings::set_ask_before_paste_upload(enabled);
                cx.notify();
            },
        ),
        value != default,
        (value != default).then(|| {
            setting_reset(
                "reset-uploader-ask-paste",
                UPLOADER_ASK_PASTE.label,
                cx,
                move |cx| {
                    crate::backend::settings::set_ask_before_paste_upload(default);
                    cx.notify();
                },
            )
        }),
        stacked,
    )
}

fn render_upload_options<T: 'static>(
    config: ImageUploaderConfig,
    defaults: ImageUploaderConfig,
    stacked: bool,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .child(settings_subheading("Upload handling"))
        .child(setting_target(
            UPLOADER_DRAG_DROP,
            uploader_toggle_row(
                UPLOADER_DRAG_DROP,
                (
                    config.drag_drop_upload_enabled,
                    defaults.drag_drop_upload_enabled,
                ),
                "Upload files dropped into chat.",
                stacked,
                cx,
                |enabled, cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.drag_drop_upload_enabled = enabled
                    });
                    cx.notify();
                },
                move |cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.drag_drop_upload_enabled = defaults.drag_drop_upload_enabled
                    });
                    cx.notify();
                },
            ),
        ))
        .child(setting_target(
            UPLOADER_COPY_LINK,
            uploader_toggle_row(
                UPLOADER_COPY_LINK,
                (
                    config.copy_link_after_upload,
                    defaults.copy_link_after_upload,
                ),
                "Copy the resolved image link after a successful upload.",
                stacked,
                cx,
                |enabled, cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.copy_link_after_upload = enabled
                    });
                    cx.notify();
                },
                move |cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.copy_link_after_upload = defaults.copy_link_after_upload
                    });
                    cx.notify();
                },
            ),
        ))
        .child(setting_target(
            UPLOADER_CONFIRM_LARGE,
            uploader_toggle_row(
                UPLOADER_CONFIRM_LARGE,
                (
                    config.ask_before_large_upload,
                    defaults.ask_before_large_upload,
                ),
                "Ask before uploads at least half of the configured maximum size.",
                stacked,
                cx,
                |enabled, cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.ask_before_large_upload = enabled
                    });
                    cx.notify();
                },
                move |cx| {
                    persist_toggle_change(cx.entity_id().as_u64(), |config| {
                        config.ask_before_large_upload = defaults.ask_before_large_upload
                    });
                    cx.notify();
                },
            ),
        ))
}

fn uploader_mapping_modified(current: &ImageUploaderConfig, default: &ImageUploaderConfig) -> bool {
    current.request_url != default.request_url
        || current.form_field != default.form_field
        || current.extra_headers != default.extra_headers
        || current.image_link_template != default.image_link_template
        || current.deletion_link_template != default.deletion_link_template
        || current.max_upload_bytes != default.max_upload_bytes
}

fn reset_uploader_mapping<T: 'static>(
    inputs: &UploaderEditorInputs,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let saved = crate::backend::settings::get_image_uploader_config();
    let Some(generation) = mapping_replacement_generation(&saved, cx.entity_id().as_u64()) else {
        cx.notify();
        return;
    };
    let default = ImageUploaderConfig::default();
    let mut config = saved.clone();
    config.request_url = default.request_url;
    config.form_field = default.form_field;
    config.extra_headers = default.extra_headers;
    config.image_link_template = default.image_link_template;
    config.deletion_link_template = default.deletion_link_template;
    config.max_upload_bytes = default.max_upload_bytes;
    if apply_uploader_mapping_change(&saved, config, generation, window, cx) {
        let config = crate::backend::settings::get_image_uploader_config();
        inputs.reload(&config, window, cx);
    }
    cx.notify();
}

fn render_custom_provider_editor<T: 'static>(
    inputs: &UploaderEditorInputs,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    div()
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .gap(px(Spacing::LG))
        .child(
            div()
                .text_xs()
                .text_color(colors::text_secondary())
                .child("These are the active request and response fields. Presets fill them; editing them makes the provider Custom."),
        )
        .child(settings_subtopic(
            "uploads-custom-request",
            "Request",
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(setting_target(UPLOADER_REQUEST_URL, render_labeled_text_box(UPLOADER_REQUEST_URL, UploaderField::RequestUrl, &inputs.request_url, cx)))
                .child(setting_target(UPLOADER_FORM_FIELD, render_labeled_text_box(UPLOADER_FORM_FIELD, UploaderField::FormField, &inputs.form_field, cx)))
                .child(setting_target(UPLOADER_EXTRA_HEADERS, render_labeled_text_box(UPLOADER_EXTRA_HEADERS, UploaderField::ExtraHeaders, &inputs.extra_headers, cx)))
                .child(div().text_xs().text_color(colors::text_secondary()).child("Separate headers with semicolons: Name: value ; Next: value."))
                .child(setting_target(UPLOADER_MAX_BYTES, render_labeled_text_box(UPLOADER_MAX_BYTES, UploaderField::MaxBytes, &inputs.max_upload_bytes, cx))),
        ))
        .child(settings_subtopic(
            "uploads-custom-response",
            "Response mapping",
            div()
                .w_full()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(setting_target(UPLOADER_IMAGE_LINK, render_labeled_text_box(UPLOADER_IMAGE_LINK, UploaderField::ImageLink, &inputs.image_link_template, cx)))
                .child(setting_target(UPLOADER_DELETION_LINK, render_labeled_text_box(UPLOADER_DELETION_LINK, UploaderField::DeletionLink, &inputs.deletion_link_template, cx)))
                .child(div().text_xs().text_color(colors::text_secondary()).child("Map JSON with placeholders such as {data.url} or {files.0.url}. A blank image mapping accepts a raw JSON string URL.")),
        ))
        .child(setting_target(UPLOADER_SAVE, render_action_row(inputs, cx)))
        .child(settings_subtopic(
            "uploads-custom-presets",
            "Import & export",
            div()
                .w_full()
                .min_w_0()
                .flex()
                .flex_col()
                .gap(px(Spacing::SM))
                .child(div().text_xs().text_color(colors::text_secondary()).child("Portable presets use ShareX-style JSON. Exported headers may contain secrets."))
                .child(render_preset_actions(inputs, cx)),
        ))
}

fn render_upload_history<T: 'static>(cx: &mut Context<T>) -> AnyElement {
    let history = crate::backend::upload_history::entries();
    if history.is_empty() {
        return empty_state("Uploads completed through Echo will appear here.").into_any_element();
    }

    let count = history.len();
    div()
        .w_full()
        .flex()
        .flex_col()
        .gap_2()
        .child(
            div()
                .flex()
                .flex_wrap()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(format!(
                            "{count} local record{} · newest first · maximum {}",
                            if count == 1 { "" } else { "s" },
                            crate::backend::upload_history::MAX_UPLOAD_HISTORY
                        )),
                )
                .child(quiet_action_button(
                    "uploads-history-clear",
                    "Clear history",
                    false,
                    cx,
                    |_cx| {
                        crate::backend::push_ui_action(
                            crate::backend::UiAction::ConfirmClearUploadHistory,
                        );
                    },
                )),
        )
        .children(
            history
                .into_iter()
                .map(|entry| render_upload_history_entry(entry, cx)),
        )
        .into_any_element()
}

fn render_upload_history_entry<T: 'static>(
    entry: crate::backend::upload_history::UploadHistoryEntry,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let timestamp = crate::utils::format_date_time(entry.uploaded_at_ms);
    let image_url_for_copy = entry.image_url.clone();
    let image_url_for_open = entry.image_url.clone();
    let entry_id = entry.id.clone();
    let is_image = entry.mime_type.starts_with("image/");
    let render_preview = is_image && !crate::bench::ui_fixture::is_enabled();

    div()
        .id(SharedString::from(format!("upload-history-{entry_id}")))
        .w_full()
        .p_2()
        .rounded(Radii::md())
        .bg(colors::bg_surface())
        .border_1()
        .border_color(colors::border())
        .flex()
        .flex_wrap()
        .gap_2()
        .child(
            div()
                .w(px(48.0))
                .h(px(48.0))
                .flex_none()
                .relative()
                .overflow_hidden()
                .rounded(Radii::md())
                .bg(colors::bg_surface())
                .border_1()
                .border_color(colors::border())
                .flex()
                .items_center()
                .justify_center()
                .child(
                    div()
                        .text_xs()
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(colors::text_muted())
                        .child(if is_image { "IMAGE" } else { "FILE" }),
                )
                .when(render_preview, |preview| {
                    preview.child(
                        img(SharedString::from(entry.image_url.clone()))
                            .absolute()
                            .inset_0()
                            .w_full()
                            .h_full()
                            .object_fit(ObjectFit::Cover),
                    )
                }),
        )
        .child(
            div()
                .flex_1()
                .flex_basis(px(260.0))
                .min_w_0()
                .flex()
                .flex_col()
                .gap_1()
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .min_w_0()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(colors::text_primary())
                                .truncate()
                                .child(entry.file_name.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_xs()
                                .text_color(colors::text_muted())
                                .child(timestamp),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::text_muted())
                        .child(format!("{} · #{}", entry.provider, entry.channel)),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(colors::link())
                        .truncate()
                        .child(entry.image_url.clone()),
                )
                .child(
                    div()
                        .flex()
                        .flex_wrap()
                        .gap_1()
                        .child(render_history_action(
                            entry_id.clone(),
                            "Open",
                            move |cx| {
                                crate::backend::push_ui_action(
                                    crate::backend::UiAction::ConfirmOpenLink {
                                        url: image_url_for_open.clone(),
                                        mode: crate::backend::ui_actions::LinkOpenMode::Configured,
                                    },
                                );
                                cx.notify();
                            },
                            cx,
                        ))
                        .child(render_history_action(
                            entry_id.clone(),
                            "Copy link",
                            move |cx| {
                                cx.write_to_clipboard(ClipboardItem::new_string(
                                    image_url_for_copy.clone(),
                                ));
                            },
                            cx,
                        ))
                        .when_some(entry.deletion_url.clone(), |actions, deletion_url| {
                            let deletion_for_copy = deletion_url.clone();
                            actions
                                .child(render_history_action(
                                    entry_id.clone(),
                                    "Open delete link",
                                    move |cx| {
                                        crate::backend::push_ui_action(
                                            crate::backend::UiAction::ConfirmOpenLink {
                                                url: deletion_url.clone(),
                                                mode: crate::backend::ui_actions::LinkOpenMode::Configured,
                                            },
                                        );
                                        cx.notify();
                                    },
                                    cx,
                                ))
                                .child(render_history_action(
                                    entry_id.clone(),
                                    "Copy delete link",
                                    move |cx| {
                                        cx.write_to_clipboard(ClipboardItem::new_string(
                                            deletion_for_copy.clone(),
                                        ));
                                    },
                                    cx,
                                ))
                        })
                        .child(render_history_action(
                            entry_id.clone(),
                            "Forget",
                            move |cx| {
                                crate::backend::upload_history::remove_entry(&entry_id);
                                cx.notify();
                            },
                            cx,
                        )),
                ),
        )
}

fn render_history_action<T: 'static, F: Fn(&mut Context<T>) + 'static>(
    entry_id: String,
    label: &'static str,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    quiet_action_button(
        SharedString::from(format!(
            "uploader-history-{entry_id}-{}",
            crate::components::settings::widgets::control_slug(label)
        )),
        label,
        label == "Forget",
        cx,
        move |cx| {
            on_click(cx);
            cx.stop_propagation();
        },
    )
}

fn render_provider_dropdown<T: 'static>(
    inputs: &UploaderEditorInputs,
    custom_provider: &crate::components::settings::widgets::SettingsDisclosureState,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let selected = UploaderPreset::identify(&crate::backend::settings::get_image_uploader_config());
    let is_open = inputs.preset_menu_open.get();
    let menu_open_for_trigger = Rc::clone(&inputs.preset_menu_open);
    let menu_open_for_close = Rc::clone(&inputs.preset_menu_open);
    let focus = inputs.preset_menu_focus.clone();
    let focus_for_trigger = focus.clone();
    let nav = inputs.preset_menu_nav.clone();
    let nav_for_trigger = nav.clone();
    let outside = Rc::clone(&inputs.preset_menu_open);
    let entity = cx.entity();
    let actions: Vec<MenuAction> = UploaderPreset::ALL
        .into_iter()
        .map(|preset| {
            let inputs = inputs.clone();
            let custom_provider = custom_provider.clone();
            let entity = entity.clone();
            Rc::new(move |window: &mut Window, cx: &mut App| {
                entity.update(cx, |_, cx| {
                    load_provider_preset(&inputs, preset, &custom_provider, window, cx)
                });
            }) as MenuAction
        })
        .collect();
    let toggle: MenuAction = Rc::new(move |window: &mut Window, cx: &mut App| {
        let opening = !menu_open_for_trigger.get();
        menu_open_for_trigger.set(opening);
        nav_for_trigger.reset();
        if opening {
            window.focus(&focus_for_trigger, cx);
        }
        window.refresh();
    });

    let accessible_toggle = toggle.clone();
    let expand_open = inputs.preset_menu_open.clone();
    let expand_focus = inputs.preset_menu_focus.clone();
    let expand_nav = inputs.preset_menu_nav.clone();
    let collapse_open = inputs.preset_menu_open.clone();
    let collapse_nav = inputs.preset_menu_nav.clone();
    let blur_open = inputs.preset_menu_open.clone();
    let blur_focus = inputs.preset_menu_focus.clone();
    div()
        .id("uploader-provider-preset")
        .w_full()
        .min_w_0()
        .flex()
        .flex_col()
        .on_mouse_down_out(move |_, window, _| {
            if outside.replace(false) {
                window.refresh();
            }
        })
        .child(
            div()
                .id("uploader-provider-preset-trigger")
                .role(Role::ComboBox)
                .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
                .aria_label(UPLOADER_PROVIDER.label)
                .aria_value(SharedString::from(selected.label()))
                .aria_expanded(is_open)
                .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                    accessible_toggle(window, cx)
                })
                .on_a11y_action(AccessibleAction::Expand, move |_, window, cx| {
                    expand_open.set(true);
                    expand_nav.reset();
                    window.focus(&expand_focus, cx);
                    window.refresh();
                })
                .on_a11y_action(AccessibleAction::Collapse, move |_, window, _| {
                    collapse_open.set(false);
                    collapse_nav.reset();
                    window.refresh();
                })
                .w_full()
                .min_h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
                .px(px(SettingsMetrics::INPUT_PADDING_X))
                .rounded(Radii::md())
                .bg(colors::bg_elevated())
                .border_1()
                .border_color(if is_open {
                    colors::accent()
                } else {
                    colors::border()
                })
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .child(
                    div()
                        .min_w_0()
                        .flex_1()
                        .text_size(px(SettingsMetrics::ROW_LABEL_TEXT))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(colors::text_primary())
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .child(selected.label()),
                )
                .child(
                    div()
                        .flex_none()
                        .text_size(px(SettingsMetrics::ROW_LABEL_TEXT))
                        .text_color(colors::text_primary())
                        .child(if is_open { "▴" } else { "▾" }),
                )
                .hover(|style| style.border_color(colors::accent()))
                .tab_index(0)
                .map(|trigger| {
                    crate::components::ui::accessibility::accessible_focus(trigger, focus.clone())
                })
                .map(|trigger| {
                    if is_open {
                        trigger
                            .on_press_click(move |_, window, cx| toggle(window, cx))
                            .menu_keyboard(&focus, &nav, actions.clone(), move |window, _| {
                                menu_open_for_close.set(false);
                                window.refresh();
                            })
                    } else {
                        trigger.on_activate(move |_, window, cx| toggle(window, cx))
                    }
                })
                .focus_visible(|style| {
                    style.shadow(crate::components::ui::focus_ring(
                        colors::settings_group_surface(),
                    ))
                }),
        )
        .when(is_open, |dropdown| {
            dropdown.child(
                canvas(
                    move |_, window, cx| {
                        if blur_open.get() && !blur_focus.contains_focused(window, cx) {
                            blur_open.set(false);
                            window.on_next_frame(|window, _| window.refresh());
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .inset_0(),
            )
        })
        .when(is_open, |dropdown| {
            dropdown.child(
                menu_panel()
                    .id("uploader-provider-menu")
                    .role(Role::Menu)
                    .aria_label(UPLOADER_PROVIDER.label)
                    .w_full()
                    .max_h(px(240.0))
                    .overflow_y_scroll()
                    .children(
                        UploaderPreset::ALL
                            .into_iter()
                            .zip(actions.clone())
                            .enumerate()
                            .map(|(index, (preset, action))| {
                                let accessible = action.clone();
                                menu_item(
                                    SharedString::from(format!("uploader-provider-{:?}", preset)),
                                    index,
                                    &inputs.preset_menu_nav,
                                    action,
                                )
                                .role(Role::MenuItem)
                                .aria_label(preset.label())
                                .aria_selected(preset == selected)
                                .on_a11y_action(AccessibleAction::Click, move |_, window, cx| {
                                    accessible(window, cx);
                                })
                                .py(px(SettingsMetrics::COMPACT_BUTTON_PADDING_Y))
                                .flex_col()
                                .items_start()
                                .justify_center()
                                .gap(px(SettingsMetrics::LABEL_GAP))
                                .when(
                                    preset == selected
                                        && !inputs.preset_menu_nav.is_highlighted(index),
                                    |row| row.bg(colors::accent_transparent(0.12)),
                                )
                                .child(
                                    div()
                                        .text_size(px(SettingsMetrics::LABEL_TEXT))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(colors::text_primary())
                                        .child(preset.label()),
                                )
                                .child(
                                    div()
                                        .text_size(px(SettingsMetrics::HELPER_TEXT))
                                        .text_color(colors::text_muted())
                                        .child(preset.description()),
                                )
                            }),
                    ),
            )
        })
}

fn load_provider_preset<T: 'static>(
    inputs: &UploaderEditorInputs,
    preset: UploaderPreset,
    custom_provider: &crate::components::settings::widgets::SettingsDisclosureState,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    inputs.preset_menu_open.set(false);
    let saved = crate::backend::settings::get_image_uploader_config();
    let draft = uploader_editor_snapshot(&saved);
    match primary_provider_selection(
        preset,
        UploaderPreset::identify(&saved),
        uploader_mapping_change_blocked(&draft),
    ) {
        PrimaryProviderSelection::AlreadyUsing => {
            set_status(
                cx.entity_id().as_u64(),
                format!("Already using {}", preset.label()),
                false,
            );
        }
        PrimaryProviderSelection::OpenMappingEditor => {
            custom_provider.set_expanded(true, window, cx);
            set_status(
                cx.entity_id().as_u64(),
                "Request and response mapping opened below. Edit it, then Save.",
                false,
            );
        }
        PrimaryProviderSelection::RefusePendingDraft => {
            set_status(
                cx.entity_id().as_u64(),
                uploader_pending_mapping_message(&saved, &draft)
                    .unwrap_or_else(|| "Save or Reload the pending uploader draft first.".into()),
                true,
            );
        }
        PrimaryProviderSelection::ApplySavedPreset => match preset.apply(&saved) {
            Ok(next) => {
                let placeholder = has_placeholder_credential(&next);
                if apply_uploader_mapping_change(&saved, next, draft.generation, window, cx) {
                    let saved = crate::backend::settings::get_image_uploader_config();
                    inputs.reload(&saved, window, cx);
                    if placeholder {
                        custom_provider.set_expanded(true, window, cx);
                    }
                }
            }
            Err(error) => set_status(cx.entity_id().as_u64(), error, true),
        },
    }
    cx.notify();
}

fn mapping_replacement_generation(saved: &ImageUploaderConfig, host: u64) -> Option<u64> {
    let snapshot = uploader_editor_snapshot(saved);
    if uploader_mapping_change_blocked(&snapshot) {
        set_status(
            host,
            uploader_pending_mapping_message(saved, &snapshot)
                .unwrap_or_else(|| "Save or Reload the pending uploader draft first.".into()),
            true,
        );
        None
    } else {
        Some(snapshot.generation)
    }
}

fn apply_uploader_mapping_change<T: 'static>(
    saved: &ImageUploaderConfig,
    mut next: ImageUploaderConfig,
    generation: u64,
    window: &Window,
    cx: &Context<T>,
) -> bool {
    if has_placeholder_credential(&next) {
        next.enabled = false;
    }
    if mapping_replacement_requires_confirmation(saved, &next) {
        let status_revision = write_status(
            cx.entity_id().as_u64(),
            "Awaiting confirmation to replace the saved uploader mapping",
            false,
        );
        crate::backend::push_ui_action(
            crate::backend::UiAction::ConfirmUploaderMappingReplacement {
                saved: crate::log::Secret(saved.clone()),
                replacement: crate::log::Secret(next),
                draft_generation: generation,
                origin: cx.entity().downgrade().into(),
                origin_window: window.window_handle(),
                status_revision,
            },
        );
        false
    } else {
        save_uploader_mapping(next, cx.entity_id().as_u64())
    }
}

fn save_uploader_mapping(mut config: ImageUploaderConfig, host: u64) -> bool {
    let placeholder = has_placeholder_credential(&config);
    if placeholder {
        config.enabled = false;
    }
    match crate::backend::settings::set_image_uploader_config(config.clone()) {
        Ok(()) => {
            UPLOADER_EDITOR.replace_from_authority(UploaderEditorDraft::from_config(&config));
            if placeholder {
                set_status(
                    host,
                    "Replace YOUR_API_KEY_HERE with your API key. The uploader remains disabled.",
                    true,
                );
            } else {
                set_status(
                    host,
                    format!(
                        "Applied and saved {}",
                        UploaderPreset::identify(&config).label()
                    ),
                    false,
                );
            }
            true
        }
        Err(error) => {
            set_status(host, error, true);
            false
        }
    }
}

pub(crate) fn confirm_uploader_mapping(
    saved: &ImageUploaderConfig,
    replacement: &ImageUploaderConfig,
    generation: u64,
    host: u64,
    status_revision: u64,
) -> bool {
    if !confirmation_request_is_live(host, status_revision) {
        crate::log_ui!(
            debug,
            "Ignored uploader confirmation without a live pending settings request"
        );
        return false;
    }
    let current = crate::backend::settings::get_image_uploader_config();
    let snapshot = uploader_editor_snapshot(&current);
    if !mapping_confirmation_matches(saved, &current, &snapshot, generation) {
        set_status(
            host,
            "Kept the current mapping. Configuration changed; review it and retry.",
            true,
        );
        return false;
    }
    save_uploader_mapping(replacement.clone(), host)
}

fn confirmation_request_is_live(host: u64, status_revision: u64) -> bool {
    uploader_status(host).is_some_and(|status| status.revision == status_revision)
}

fn prepare_imported_mapping(
    saved: &ImageUploaderConfig,
    mut imported: ImageUploaderConfig,
) -> ImageUploaderConfig {
    if mapping_is_pristine(saved) {
        imported.enabled = saved.enabled;
        imported.paste_upload_enabled = saved.paste_upload_enabled;
        imported.drag_drop_upload_enabled = saved.drag_drop_upload_enabled;
        imported.copy_link_after_upload = saved.copy_link_after_upload;
        imported.ask_before_large_upload = saved.ask_before_large_upload;
    }
    imported
}

fn render_labeled_text_box<T: 'static>(
    descriptor: SettingDescriptor,
    field: UploaderField,
    input: &Entity<InputState>,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let input_for_escape = input.clone();
    div()
        .flex()
        .flex_col()
        .gap(px(SettingsMetrics::LABEL_GAP))
        .child(
            div()
                .text_size(px(SettingsMetrics::ROW_LABEL_TEXT))
                .font_weight(FontWeight::MEDIUM)
                .text_color(colors::text_primary())
                .child(descriptor.label.to_string()),
        )
        .child(
            div()
                .id(SharedString::from(descriptor.control_id.to_string()))
                .w_full()
                .h(px(SettingsMetrics::INPUT_MIN_HEIGHT))
                .flex()
                .items_center()
                .text_size(px(SettingsMetrics::LABEL_TEXT))
                .font_family("monospace")
                .capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                    revert_uploader_field(field, &input_for_escape, window, cx);
                    cx.stop_propagation();
                }))
                .child(
                    Input::new(input)
                        .accessibility_label(descriptor.label)
                        .accessibility_secure(matches!(
                            field,
                            UploaderField::ExtraHeaders | UploaderField::RequestUrl
                        )),
                ),
        )
}

fn render_status(status: UploaderStatus) -> impl IntoElement {
    div()
        .px_2()
        .py_1()
        .rounded(Radii::sm())
        .bg(if status.is_error {
            colors::error().opacity(0.10)
        } else {
            colors::bg_surface()
        })
        .text_xs()
        .text_color(if status.is_error {
            colors::error()
        } else {
            colors::text_secondary()
        })
        .child(status.message)
}

fn render_action_row<T: 'static>(
    inputs: &UploaderEditorInputs,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let inputs_for_save = inputs.clone();
    let inputs_for_reload = inputs.clone();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(render_button(
            "Save",
            true,
            move |_window, cx| save_editor(&inputs_for_save, cx),
            cx,
        ))
        .child(render_button(
            "Reload",
            false,
            move |window, cx| reload_editor(&inputs_for_reload, window, cx),
            cx,
        ))
}

fn render_preset_actions<T: 'static>(
    inputs: &UploaderEditorInputs,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T> {
    let inputs_for_import = inputs.clone();
    let inputs_for_export = inputs.clone();
    div()
        .flex()
        .flex_wrap()
        .items_center()
        .gap_2()
        .child(setting_target(
            UPLOADER_IMPORT_PRESET,
            render_button(
                "Import from clipboard",
                false,
                move |window, cx| import_uploader_preset(&inputs_for_import, window, cx),
                cx,
            ),
        ))
        .child(setting_target(
            UPLOADER_EXPORT_PRESET,
            render_button(
                "Export to clipboard",
                false,
                move |_window, cx| export_uploader_preset(&inputs_for_export, cx),
                cx,
            ),
        ))
}

fn render_button<T: 'static, F: Fn(&mut Window, &mut Context<T>) + 'static>(
    label: &str,
    primary: bool,
    on_click: F,
    cx: &mut Context<T>,
) -> impl IntoElement + use<T, F> {
    div()
        .id(SharedString::from(format!(
            "uploader-button-{}",
            crate::components::settings::widgets::control_slug(label)
        )))
        .role(Role::Button)
        .map(|control| crate::components::settings::widgets::settings_keyboard_control(control, cx))
        .a11y_synthetic_children(crate::components::ui::accessibility::apply_description)
        .aria_label(SharedString::from(label.to_string()))
        .px_3()
        .py_1()
        .rounded(Radii::sm())
        .bg(if primary {
            colors::accent()
        } else {
            colors::bg_elevated()
        })
        .border_1()
        .border_color(if primary {
            colors::accent()
        } else {
            colors::border()
        })
        .cursor_pointer()
        .child(
            div()
                .text_sm()
                .font_weight(FontWeight::MEDIUM)
                .text_color(if primary {
                    colors::text_on_accent_contrast()
                } else {
                    colors::text_primary()
                })
                .child(label.to_string()),
        )
        .on_accessible_activate(cx.listener(move |_this, _event, window, cx| on_click(window, cx)))
}

fn save_editor<T: 'static>(inputs: &UploaderEditorInputs, cx: &mut Context<T>) {
    let saved = crate::backend::settings::get_image_uploader_config();
    match inputs.to_config(&saved, cx) {
        Ok(config) => {
            save_uploader_mapping(config, cx.entity_id().as_u64());
        }
        Err(error) => set_status(cx.entity_id().as_u64(), error, true),
    }
    cx.notify();
}

fn persist_enabled_change(enabled: bool, host: u64) {
    let current = crate::backend::settings::get_image_uploader_config();
    let draft = uploader_editor_snapshot(&current);
    let result = prepare_enabled_config(&current, &draft, enabled)
        .and_then(crate::backend::settings::set_image_uploader_config);

    match result {
        Ok(()) => {
            set_status(
                host,
                if enabled {
                    "Image uploader enabled with the saved mapping"
                } else {
                    "Image uploader disabled"
                },
                false,
            );
        }
        Err(error) => set_status(host, error, true),
    }
}

fn persist_toggle_change(host: u64, update: impl FnOnce(&mut ImageUploaderConfig)) {
    let mut config = crate::backend::settings::get_image_uploader_config();
    update(&mut config);
    match crate::backend::settings::set_image_uploader_config(config) {
        Ok(()) => set_status(host, "Saved uploader option", false),
        Err(error) => set_status(host, error, true),
    }
}

fn reload_editor<T: 'static>(
    inputs: &UploaderEditorInputs,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let config = crate::backend::settings::get_image_uploader_config();
    inputs.reload(&config, window, cx);
    set_status(
        cx.entity_id().as_u64(),
        "Reloaded the saved request and response mapping",
        false,
    );
    cx.notify();
}

fn export_uploader_preset<T: 'static>(inputs: &UploaderEditorInputs, cx: &mut Context<T>) {
    let current = crate::backend::settings::get_image_uploader_config();
    let result = inputs
        .to_config(&current, cx)
        .and_then(|config| export_uploader_config_json(&config));
    match result {
        Ok(json) => {
            cx.write_to_clipboard(ClipboardItem::new_string(json));
            set_status(
                cx.entity_id().as_u64(),
                "Copied the visible uploader preset; review headers before sharing",
                false,
            );
        }
        Err(error) => set_status(cx.entity_id().as_u64(), error, true),
    }
    cx.notify();
}

fn import_uploader_preset<T: 'static>(
    inputs: &UploaderEditorInputs,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let current = crate::backend::settings::get_image_uploader_config();
    let Some(generation) = mapping_replacement_generation(&current, cx.entity_id().as_u64()) else {
        cx.notify();
        return;
    };
    let clipboard = crate::utils::read_text(cx).unwrap_or_default();
    match import_uploader_config_json(&clipboard, &current) {
        Ok(config) => {
            let config = prepare_imported_mapping(&current, config);
            if apply_uploader_mapping_change(&current, config, generation, window, cx) {
                let config = crate::backend::settings::get_image_uploader_config();
                inputs.reload(&config, window, cx);
            }
        }
        Err(error) => set_status(cx.entity_id().as_u64(), error, true),
    }
    cx.notify();
}
