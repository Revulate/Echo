//! Visible-only semantics for the chat canvas, retained with its element state.

use super::*;
use crate::backend::HighlightType;
use crate::components::ui::accessibility::{claim_chat_announcement, clipped_accessible_bounds};
use std::sync::Weak;

struct AccessibleRow {
    message: Arc<ChatMessage>,
    node: accesskit::Node,
}

#[derive(Default)]
pub(super) struct ChatAccessibilityState {
    rows: Vec<AccessibleRow>,
    messages: Weak<Vec<Arc<ChatMessage>>>,
    bounds: Option<Bounds<Pixels>>,
    clip: Option<Bounds<Pixels>>,
    scale: f32,
    show_timestamp: bool,
    timestamp_generation: u64,
    last_tail: Option<Arc<str>>,
    announcement: Option<accesskit::Node>,
    announcement_serial: u64,
}

impl ChatAccessibilityState {
    fn update(
        &mut self,
        source: &CanvasMessageList,
        bounds: Bounds<Pixels>,
        clip: Bounds<Pixels>,
        scale: f32,
        stable_rows: bool,
        window_id: WindowId,
    ) {
        let messages = &source.message_model.messages;
        let messages_changed = self.messages.as_ptr() != Arc::as_ptr(messages);
        let timestamp_generation = settings::message_timestamp_formatter().generation;
        let text_settings_changed = self.show_timestamp != source.show_timestamp
            || (source.show_timestamp && self.timestamp_generation != timestamp_generation);
        self.update_announcement(source, messages_changed, window_id);
        if stable_rows
            && !messages_changed
            && !text_settings_changed
            && self.bounds == Some(bounds)
            && self.clip == Some(clip)
            && self.scale == scale
        {
            return;
        }

        let mut count = 0;
        for row in &source.viewport_state.visible_render_rows {
            let row_bounds = Bounds::new(
                point(bounds.left(), bounds.top() + px(row.viewport_y)),
                size(bounds.size.width, px(row.layout.height)),
            );
            let Some(rect) = clipped_accessible_bounds(row_bounds, bounds.intersect(&clip), scale)
            else {
                continue;
            };
            let Some(message) = messages.get(row.message_index) else {
                continue;
            };
            if count == self.rows.len() {
                self.rows.push(AccessibleRow {
                    message: Arc::clone(message),
                    node: accesskit::Node::new(Role::ListItem),
                });
                self.rows[count]
                    .node
                    .set_label(message_label(message, source.show_timestamp));
            } else if text_settings_changed || !Arc::ptr_eq(&self.rows[count].message, message) {
                self.rows[count].message = Arc::clone(message);
                self.rows[count]
                    .node
                    .set_label(message_label(message, source.show_timestamp));
            }
            self.rows[count].node.set_bounds(rect);
            count += 1;
        }
        self.rows.truncate(count);
        self.messages = Arc::downgrade(messages);
        self.bounds = Some(bounds);
        self.clip = Some(clip);
        self.scale = scale;
        self.show_timestamp = source.show_timestamp;
        self.timestamp_generation = timestamp_generation;
    }

    fn update_announcement(
        &mut self,
        source: &CanvasMessageList,
        messages_changed: bool,
        window_id: WindowId,
    ) {
        let status = self.announcement.get_or_insert_with(|| {
            let mut node = accesskit::Node::new(Role::Status);
            node.set_live(accesskit::Live::Polite);
            node
        });
        if !source.highlights_enabled {
            status.clear_label();
        }
        if !messages_changed {
            return;
        }
        let messages = &source.message_model.messages;
        let tail = messages.last().map(|message| Arc::clone(&message.id));
        // Initial/history snapshots establish a baseline, never announce old chat.
        if let Some(previous) = self.last_tail.as_deref()
            && tail.as_deref() != Some(previous)
            && source.highlights_enabled
        {
            let mut newest_highlight = None;
            let mut found_previous = false;
            // Bound catch-up work even after a long detached/background interval.
            for message in messages.iter().rev().take(128) {
                if message.id.as_ref() == previous {
                    found_previous = true;
                    break;
                }
                if newest_highlight.is_none() && is_personal_highlight(message) {
                    newest_highlight = Some(message);
                }
            }
            if found_previous
                && let Some(message) = newest_highlight
                && claim_chat_announcement(window_id, &message.id)
            {
                self.announcement_serial = self.announcement_serial.wrapping_add(1);
                status.set_label(format!(
                    "Highlight {} in #{}: {}",
                    self.announcement_serial,
                    source.channel_name,
                    message_label(message, false),
                ));
            }
        }
        self.last_tail = tail;
    }
}

fn is_personal_highlight(message: &ChatMessage) -> bool {
    !message.flags.is_system
        && !message.flags.is_deleted
        && matches!(
            message.flags.highlight_type,
            Some(
                HighlightType::Mention
                    | HighlightType::Whisper
                    | HighlightType::User { .. }
                    | HighlightType::Phrase { .. }
                    | HighlightType::CustomPhrase { .. }
                    | HighlightType::CustomUser { .. }
                    | HighlightType::CustomBadge { .. }
                    | HighlightType::Badge { .. }
                    | HighlightType::ParticipatedReply { .. }
                    | HighlightType::HighlightedMessage
            )
        )
}

fn message_label(message: &ChatMessage, show_timestamp: bool) -> String {
    if let Some(date) = message.day_divider_date() {
        return date.to_owned();
    }
    let mut label = if show_timestamp {
        let mut timestamp = super::super::layout::get_cached_timestamp(message.timestamp);
        timestamp.push(' ');
        timestamp
    } else {
        String::new()
    };
    if !message.flags.is_system {
        label.push_str(if message.display_name.is_empty() {
            &message.sender
        } else {
            &message.display_name
        });
        label.push_str(": ");
    }
    label.push_str(&message.content);
    label
}

/// Delegates the original list element verbatim, preserving its focus and input dispatch.
pub(super) struct AccessibleChat {
    body: Stateful<Div>,
    source: Entity<CanvasMessageList>,
    stable_rows: bool,
}

impl AccessibleChat {
    pub(super) fn new(
        body: Stateful<Div>,
        source: Entity<CanvasMessageList>,
        stable_rows: bool,
    ) -> Self {
        Self {
            body,
            source,
            stable_rows,
        }
    }
}

impl IntoElement for AccessibleChat {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for AccessibleChat {
    type RequestLayoutState = <Stateful<Div> as Element>::RequestLayoutState;
    type PrepaintState = (
        <Stateful<Div> as Element>::PrepaintState,
        Option<Rc<RefCell<ChatAccessibilityState>>>,
    );

    fn id(&self) -> Option<ElementId> {
        Element::id(&self.body)
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        self.body.source_location()
    }
    fn a11y_role(&self) -> Option<Role> {
        self.body.a11y_role()
    }
    fn write_a11y_info(&self, node: &mut accesskit::Node) {
        self.body.write_a11y_info(node);
    }

    fn request_layout(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        self.body.request_layout(id, inspector, window, cx)
    }

    fn prepaint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        state: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let body = self.body.prepaint(id, inspector, bounds, state, window, cx);
        let cache = if window.is_a11y_active() {
            id.map(|id| {
                window.with_element_state(
                    id,
                    |cache: Option<Rc<RefCell<ChatAccessibilityState>>>, window| {
                        let cache = cache.unwrap_or_default();
                        cache.borrow_mut().update(
                            self.source.read(cx),
                            bounds,
                            window.content_mask().bounds,
                            window.scale_factor(),
                            self.stable_rows,
                            window.window_handle().window_id(),
                        );
                        (Rc::clone(&cache), cache)
                    },
                )
            })
        } else {
            None
        };
        (body, cache)
    }

    fn a11y_synthetic_children(
        &mut self,
        state: &mut Self::PrepaintState,
        builder: &mut A11ySubtreeBuilder,
    ) {
        Element::a11y_synthetic_children(&mut self.body, &mut state.0, builder);
        let Some(cache) = &state.1 else {
            return;
        };
        let cache = cache.borrow();
        // AccessKit owns each submitted frame; only these bounded node copies are unavoidable.
        for row in &cache.rows {
            let id = builder.synthetic_node_id(("message", row.message.id.as_ref()));
            builder.push_child(id, row.node.clone());
        }
        if let Some(status) = &cache.announcement {
            builder.push_child(builder.synthetic_node_id("highlights"), status.clone());
        }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        inspector: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        layout: &mut Self::RequestLayoutState,
        state: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body
            .paint(id, inspector, bounds, layout, &mut state.0, window, cx);
    }
}
