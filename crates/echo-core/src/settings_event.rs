//! GPUI-free settings change events and the layout policy derived from them.
//!
//! The application broadcasts a [`SettingsEvent`] whenever a preference
//! changes. Which of those events force a chat-layout invalidation is pure
//! policy over the event, so it lives here next to the event itself rather than
//! inside the polling coordinator.

#![allow(clippy::enum_variant_names)] // *Changed suffix is intentional for event naming

/// Events broadcast when settings change, enabling reactive updates across windows
#[derive(Clone, Debug)]
pub enum SettingsEvent {
    ThemeChanged,
    FontScaleChanged,
    EmoteScaleChanged,
    BadgeScaleChanged,
    AccentChanged,
    EmoteSpacingChanged,
    TabThemeChanged,
    TabOverflowModeChanged,
    TabPlacementChanged,
    TitlebarDensityChanged,
    BadgeDisplayModeChanged,
    ChatDensityChanged,
    HighlightColorChanged,
    EmbedSettingsChanged,
    AnimationPerformanceChanged,
    CosmeticSettingsChanged,
    NicknamesChanged,
    /// A per-emote "show as text" / "use still image" rule changed.
    EmoteDisplayRulesChanged,
    StreamerModeChanged,
    WindowAppearanceChanged,
    OverlaySettingsChanged,
    ChatPauseChanged,
    ChatInteractionChanged,
    TimestampChanged,
    ProxyChanged,
    FiltersChanged,
    /// The main window moved to a display with another DPI scale. Chat canvases
    /// re-lay out on their own; this only refreshes views that pick image tiers
    /// from the main window's scale.
    WindowScaleChanged,
}

/// Whether this settings change invalidates cached chat layout.
#[inline]
pub fn settings_event_requires_layout_invalidation(event: &SettingsEvent) -> bool {
    matches!(
        event,
        SettingsEvent::EmoteScaleChanged
            | SettingsEvent::BadgeScaleChanged
            | SettingsEvent::FontScaleChanged
            | SettingsEvent::EmoteSpacingChanged
            | SettingsEvent::ChatDensityChanged
            | SettingsEvent::ThemeChanged
            | SettingsEvent::AccentChanged
            | SettingsEvent::CosmeticSettingsChanged
            | SettingsEvent::NicknamesChanged
            | SettingsEvent::EmoteDisplayRulesChanged
            | SettingsEvent::StreamerModeChanged
            | SettingsEvent::TimestampChanged
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_affecting_settings_events_require_invalidation() {
        let events = [
            SettingsEvent::ThemeChanged,
            SettingsEvent::FontScaleChanged,
            SettingsEvent::EmoteScaleChanged,
            SettingsEvent::BadgeScaleChanged,
            SettingsEvent::AccentChanged,
            SettingsEvent::EmoteSpacingChanged,
            SettingsEvent::ChatDensityChanged,
            SettingsEvent::EmoteDisplayRulesChanged,
            SettingsEvent::StreamerModeChanged,
            SettingsEvent::TimestampChanged,
        ];

        for event in events {
            assert!(
                settings_event_requires_layout_invalidation(&event),
                "Expected {:?} to require layout invalidation",
                event
            );
        }
    }

    #[test]
    fn non_layout_settings_events_do_not_require_invalidation() {
        let events = [
            SettingsEvent::TabThemeChanged,
            SettingsEvent::TabOverflowModeChanged,
            SettingsEvent::TabPlacementChanged,
            SettingsEvent::BadgeDisplayModeChanged,
            SettingsEvent::HighlightColorChanged,
            SettingsEvent::AnimationPerformanceChanged,
            SettingsEvent::OverlaySettingsChanged,
            SettingsEvent::WindowAppearanceChanged,
            SettingsEvent::TitlebarDensityChanged,
            SettingsEvent::WindowScaleChanged,
        ];

        for event in events {
            assert!(
                !settings_event_requires_layout_invalidation(&event),
                "Expected {:?} to skip layout invalidation",
                event
            );
        }
    }
}
