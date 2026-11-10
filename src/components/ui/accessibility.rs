//! Render-local help and focus reveal for compound native controls.

use gpui::*;
use std::{cell::RefCell, collections::VecDeque, rc::Rc, sync::Arc};

pub type FocusReveal = Rc<dyn Fn(&FocusHandle, Bounds<Pixels>, &mut Window, &mut App)>;

/// Synthetic nodes use physical client coordinates, just like GPUI's real nodes.
pub fn clipped_accessible_bounds(
    bounds: Bounds<Pixels>,
    clip: Bounds<Pixels>,
    scale: f32,
) -> Option<accesskit::Rect> {
    let bounds = bounds.intersect(&clip);
    let rect = accesskit::Rect {
        x0: f64::from(f32::from(bounds.left()) * scale),
        y0: f64::from(f32::from(bounds.top()) * scale),
        x1: f64::from(f32::from(bounds.right()) * scale),
        y1: f64::from(f32::from(bounds.bottom()) * scale),
    };
    (rect.x0.is_finite()
        && rect.y0.is_finite()
        && rect.x1.is_finite()
        && rect.y1.is_finite()
        && rect.x1 > rect.x0
        && rect.y1 > rect.y0)
        .then_some(rect)
}

thread_local! {
    static LABEL: RefCell<Option<SharedString>> = const { RefCell::new(None) };
    static DESCRIPTION: RefCell<Option<SharedString>> = const { RefCell::new(None) };
    static FOCUS_REVEAL: RefCell<Option<FocusReveal>> = const { RefCell::new(None) };
    static ACCESSIBLE_FOCUS: RefCell<Option<WeakFocusHandle>> = const { RefCell::new(None) };
    static ANNOUNCED_MESSAGES: RefCell<VecDeque<(WindowId, Arc<str>)>> = const { RefCell::new(VecDeque::new()) };
}

/// Mirrored channel splits should produce only one live-region event per window.
pub fn claim_chat_announcement(window: WindowId, message_id: &Arc<str>) -> bool {
    ANNOUNCED_MESSAGES.with(|messages| {
        let mut messages = messages.borrow_mut();
        if messages
            .iter()
            .any(|(id, message)| *id == window && message == message_id)
        {
            return false;
        }
        if messages.len() == 128 {
            messages.pop_front();
        }
        messages.push_back((window, Arc::clone(message_id)));
        true
    })
}

pub fn focus_from_accessibility(focus: &FocusHandle, window: &mut Window, cx: &mut App) {
    ACCESSIBLE_FOCUS.with(|value| *value.borrow_mut() = Some(focus.downgrade()));
    window.focus(focus, cx);
}

pub fn clear_accessible_focus() {
    ACCESSIBLE_FOCUS.with(|value| *value.borrow_mut() = None);
}

/// Consume an assistive-technology focus request once its control has been
/// revealed. A request only applies to the exact handle it targeted; later
/// controls must not inherit it.
pub fn take_accessible_focus(focus: &FocusHandle) -> bool {
    ACCESSIBLE_FOCUS.with(|value| {
        let mut value = value.borrow_mut();
        let targeted = value.as_ref().and_then(WeakFocusHandle::upgrade).as_ref() == Some(focus);
        if targeted {
            value.take();
        }
        targeted
    })
}

pub fn should_reveal_focus(focus: &FocusHandle, window: &Window) -> bool {
    if window.last_input_was_keyboard() {
        // A subsequent keyboard traversal supersedes a prior AT focus request.
        clear_accessible_focus();
        return true;
    }
    ACCESSIBLE_FOCUS.with(|value| {
        let mut value = value.borrow_mut();
        let Some(active) = value.as_ref().and_then(WeakFocusHandle::upgrade) else {
            value.take();
            return false;
        };
        active == focus.clone()
    })
}

pub fn apply_description(builder: &mut A11ySubtreeBuilder) {
    if builder.parent_node().label().is_none()
        && let Some(label) = LABEL.with(|value| value.borrow().clone())
    {
        builder.parent_node().set_label(label.to_string());
    }
    if builder.parent_node().description().is_none()
        && let Some(description) = DESCRIPTION.with(|value| value.borrow().clone())
    {
        builder
            .parent_node()
            .set_description(description.to_string());
    }
}

pub fn reveal_focus(
    focus: &FocusHandle,
    bounds: Bounds<Pixels>,
    window: &mut Window,
    cx: &mut App,
) {
    if focus.is_focused(window)
        && let Some(reveal) = FOCUS_REVEAL.with(|value| value.borrow().clone())
    {
        reveal(focus, bounds, window, cx);
    }
}

/// An absolute, noninteractive marker using its control's painted bounds.
pub fn focus_marker(focus: FocusHandle) -> impl IntoElement {
    canvas(
        move |bounds, window, cx| reveal_focus(&focus, bounds, window, cx),
        |_, _, _, _| {},
    )
    .absolute()
    .inset_0()
}

/// Attach one stable focus handle, an accessibility Focus action, and the
/// marker used by settings' minimal-scroll reveal. Settings controls should
/// use this instead of re-implementing the three pieces separately.
///
/// GPUI only exposes a focus handle to callers that retain or derive one
/// before render. Controls that currently rely on a Div's implicit handle
/// must first be assigned a stable handle by their owner.
pub fn accessible_focus(element: Stateful<Div>, focus: FocusHandle) -> Stateful<Div> {
    let marker = focus.clone();
    let accessible_focus = focus.clone();
    element
        .track_focus(&focus)
        .child(focus_marker(marker))
        .on_a11y_action(accesskit::Action::Focus, move |_, window, cx| {
            focus_from_accessibility(&accessible_focus, window, cx);
        })
}

pub fn described_control(
    body: impl IntoElement,
    label: &str,
    description: Option<&str>,
) -> ScopedElement {
    ScopedElement {
        body: body.into_any_element(),
        description: description.map(SharedString::from),
        label: Some(SharedString::from(label)),
        reveal: None,
        control: true,
    }
}

pub fn focus_reveal_scope(body: impl IntoElement, reveal: FocusReveal) -> ScopedElement {
    ScopedElement {
        body: body.into_any_element(),
        description: None,
        label: None,
        reveal: Some(reveal),
        control: false,
    }
}

pub struct ScopedElement {
    body: AnyElement,
    description: Option<SharedString>,
    label: Option<SharedString>,
    reveal: Option<FocusReveal>,
    control: bool,
}

struct ScopeGuard {
    label: Option<Option<SharedString>>,
    description: Option<Option<SharedString>>,
    reveal: Option<Option<FocusReveal>>,
}
impl Drop for ScopeGuard {
    fn drop(&mut self) {
        if let Some(old) = self.label.take() {
            LABEL.with(|value| *value.borrow_mut() = old);
        }
        if let Some(old) = self.description.take() {
            DESCRIPTION.with(|value| *value.borrow_mut() = old);
        }
        if let Some(old) = self.reveal.take() {
            FOCUS_REVEAL.with(|value| *value.borrow_mut() = old);
        }
    }
}

impl IntoElement for ScopedElement {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for ScopedElement {
    type RequestLayoutState = ();
    type PrepaintState = ();
    fn id(&self) -> Option<ElementId> {
        None
    }
    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.body.request_layout(window, cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let _guard = ScopeGuard {
            label: self
                .control
                .then(|| LABEL.with(|value| value.replace(self.label.clone()))),
            description: self
                .control
                .then(|| DESCRIPTION.with(|value| value.replace(self.description.clone()))),
            reveal: self
                .reveal
                .as_ref()
                .map(|reveal| FOCUS_REVEAL.with(|value| value.replace(Some(reveal.clone())))),
        };
        if let Some(focus) = self.body.prepaint(window, cx)
            && self.control
        {
            reveal_focus(&focus, bounds, window, cx);
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        self.body.paint(window, cx);
    }
}
