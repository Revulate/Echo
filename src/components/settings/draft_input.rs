//! Settings draft fields edited through Echo's shared text input.
//!
//! Settings editors keep their drafts in process-wide editor state, but a text
//! input needs an `Entity<InputState>` that lives across frames. Each settings
//! host keeps one input per field key here. The draft stays the source of
//! truth: whenever it differs from the input (a rule loaded for editing, a
//! reset after saving) it is pushed back into the input.

use crate::components::settings::widgets::SettingsDisclosureState;
use crate::components::ui::input::{
    Escape, Input, InputEvent, InputState, normalize_single_line_text,
};
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::any::Any;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

type Callback<T> = Rc<dyn Fn(&mut Context<T>)>;
type ChangeCallback<T> = Rc<dyn Fn(String, &mut Context<T>)>;
type WindowCallback<T> = Rc<dyn Fn(&mut Window, &mut Context<T>)>;

struct Handlers<T> {
    on_change: Option<ChangeCallback<T>>,
    on_enter: Option<Callback<T>>,
    on_focus: Option<Callback<T>>,
    on_blur: Option<Callback<T>>,
}

impl<T> Clone for Handlers<T> {
    fn clone(&self) -> Self {
        Self {
            on_change: self.on_change.clone(),
            on_enter: self.on_enter.clone(),
            on_focus: self.on_focus.clone(),
            on_blur: self.on_blur.clone(),
        }
    }
}

struct DraftInputEntry {
    owner: AnyWeakEntity,
    input: Entity<InputState>,
    /// The text last pushed into or reported by the input, so the `Change`
    /// that `set_value` emits for a sync is not mistaken for an edit.
    synced: Rc<RefCell<String>>,
    /// `Rc<RefCell<Handlers<T>>>`, refreshed every render.
    handlers: Rc<dyn Any>,
    _subscription: Subscription,
}

thread_local! {
    static DRAFT_INPUTS: RefCell<HashMap<(EntityId, &'static str), DraftInputEntry>> =
        RefCell::new(HashMap::new());
    static FOCUS_REQUESTS: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

/// Focus the field with `key` the next time it is prepared, for actions such
/// as Edit that load a draft and expect typing to land in it.
pub fn request_focus(key: &'static str) {
    FOCUS_REQUESTS.with(|requests| requests.borrow_mut().insert(key));
}

/// A single-line settings field backed by Echo's text input: selection,
/// clipboard, word movement, undo and IME all come from `InputState`.
pub struct DraftInput<T> {
    key: &'static str,
    value: String,
    placeholder: &'static str,
    label: &'static str,
    monospace: bool,
    handlers: Handlers<T>,
    on_escape: Option<WindowCallback<T>>,
    escape_focus: Option<SettingsDisclosureState>,
}

impl<T: 'static> DraftInput<T> {
    /// `key` must be unique within the settings window; `value` is the draft.
    pub fn new(key: &'static str, value: &str, placeholder: &'static str) -> Self {
        Self {
            key,
            value: value.to_string(),
            placeholder,
            label: placeholder,
            monospace: false,
            handlers: Handlers {
                on_change: None,
                on_enter: None,
                on_focus: None,
                on_blur: None,
            },
            on_escape: None,
            escape_focus: None,
        }
    }

    pub fn label(mut self, label: &'static str) -> Self {
        self.label = label;
        self
    }

    pub fn monospace(mut self, monospace: bool) -> Self {
        self.monospace = monospace;
        self
    }

    /// Called with the input's text after the user edits it.
    pub fn on_change(mut self, on_change: impl Fn(String, &mut Context<T>) + 'static) -> Self {
        self.handlers.on_change = Some(Rc::new(on_change));
        self
    }

    pub fn on_enter(mut self, on_enter: impl Fn(&mut Context<T>) + 'static) -> Self {
        self.handlers.on_enter = Some(Rc::new(on_enter));
        self
    }

    pub fn on_focus(mut self, on_focus: impl Fn(&mut Context<T>) + 'static) -> Self {
        self.handlers.on_focus = Some(Rc::new(on_focus));
        self
    }

    pub fn on_blur(mut self, on_blur: impl Fn(&mut Context<T>) + 'static) -> Self {
        self.handlers.on_blur = Some(Rc::new(on_blur));
        self
    }

    /// Escape runs this and stops there instead of reaching the settings host.
    pub fn on_escape(mut self, on_escape: impl Fn(&mut Window, &mut Context<T>) + 'static) -> Self {
        self.on_escape = Some(Rc::new(on_escape));
        self
    }

    /// Escape moves focus to this disclosure's header, so a second Escape
    /// reaches the settings window again.
    pub fn escape_focus(mut self, disclosure: SettingsDisclosureState) -> Self {
        self.escape_focus = Some(disclosure);
        self
    }

    /// Create or sync the input. Needs the window, so call it before handing
    /// the field to a closure that only gets `cx`.
    pub fn prepare(self, window: &mut Window, cx: &mut Context<T>) -> DraftField<T> {
        let input = ensure_draft_input(self.key, self.placeholder, self.handlers, window, cx);
        sync_draft_input(self.key, &input, &self.value, window, cx);
        if input.read(cx).placeholder() != self.placeholder {
            let placeholder = self.placeholder;
            input.update(cx, |input, cx| input.set_placeholder(placeholder, cx));
        }
        if FOCUS_REQUESTS.with(|requests| requests.borrow_mut().remove(self.key)) {
            let focus = input.read(cx).focus_handle().clone();
            window.focus(&focus, cx);
        }
        DraftField {
            input,
            label: self.label,
            monospace: self.monospace,
            on_escape: self.on_escape,
            escape_focus: self.escape_focus,
        }
    }
}

/// A prepared draft input, ready to draw.
pub struct DraftField<T> {
    input: Entity<InputState>,
    label: &'static str,
    monospace: bool,
    on_escape: Option<WindowCallback<T>>,
    escape_focus: Option<SettingsDisclosureState>,
}

impl<T: 'static> DraftField<T> {
    pub fn render(self, cx: &mut Context<T>) -> AnyElement {
        let input = self.input.clone();
        div()
            .w_full()
            .min_w_0()
            .when(self.monospace, |element| element.font_family("monospace"))
            .when(
                self.on_escape.is_some() || self.escape_focus.is_some(),
                |element| {
                    let on_escape = self.on_escape;
                    let escape_focus = self.escape_focus;
                    element.capture_action(cx.listener(move |_this, _: &Escape, window, cx| {
                        // Escape first cancels an open IME composition.
                        if input.read(cx).marked_range().is_some() {
                            return;
                        }
                        if let Some(on_escape) = &on_escape {
                            on_escape(window, cx);
                        }
                        if let Some(escape_focus) = &escape_focus {
                            escape_focus.focus_trigger(window, cx);
                        }
                        cx.stop_propagation();
                        cx.notify();
                    }))
                },
            )
            .child(Input::new(&self.input).accessibility_label(self.label))
            .into_any_element()
    }
}

fn ensure_draft_input<T: 'static>(
    key: &'static str,
    placeholder: &'static str,
    handlers: Handlers<T>,
    window: &mut Window,
    cx: &mut Context<T>,
) -> Entity<InputState> {
    let owner = cx.entity_id();
    let existing = DRAFT_INPUTS.with(|inputs| {
        let mut inputs = inputs.borrow_mut();
        inputs.retain(|_, entry| entry.owner.upgrade().is_some());
        inputs.get(&(owner, key)).map(|entry| {
            if let Some(current) = entry.handlers.downcast_ref::<RefCell<Handlers<T>>>() {
                *current.borrow_mut() = handlers.clone();
            }
            entry.input.clone()
        })
    });
    if let Some(input) = existing {
        return input;
    }

    let input = cx.new(|cx| InputState::new(window, cx).with_placeholder(placeholder));
    let synced = Rc::new(RefCell::new(String::new()));
    let shared_handlers = Rc::new(RefCell::new(handlers));
    let subscription = cx.subscribe_in(&input, window, {
        let synced = Rc::clone(&synced);
        let shared_handlers = Rc::clone(&shared_handlers);
        move |_this, input, event: &InputEvent, _window, cx| {
            let handlers = shared_handlers.borrow().clone();
            match event {
                InputEvent::Change => {
                    let text = input.read(cx).value().to_string();
                    if *synced.borrow() == text {
                        return;
                    }
                    *synced.borrow_mut() = text.clone();
                    if let Some(on_change) = handlers.on_change {
                        on_change(text, cx);
                    }
                }
                InputEvent::PressEnter => {
                    if let Some(on_enter) = handlers.on_enter {
                        on_enter(cx);
                    }
                }
                InputEvent::Focus => {
                    if let Some(on_focus) = handlers.on_focus {
                        on_focus(cx);
                    }
                }
                InputEvent::Blur => {
                    if let Some(on_blur) = handlers.on_blur {
                        on_blur(cx);
                    }
                }
            }
            cx.notify();
        }
    });
    DRAFT_INPUTS.with(|inputs| {
        inputs.borrow_mut().insert(
            (owner, key),
            DraftInputEntry {
                owner: cx.weak_entity().into(),
                input: input.clone(),
                synced,
                handlers: shared_handlers,
                _subscription: subscription,
            },
        );
    });
    input
}

fn sync_draft_input<T: 'static>(
    key: &'static str,
    input: &Entity<InputState>,
    value: &str,
    window: &mut Window,
    cx: &mut Context<T>,
) {
    let value = normalize_single_line_text(value);
    if input.read(cx).value() == value {
        return;
    }
    let owner = cx.entity_id();
    DRAFT_INPUTS.with(|inputs| {
        if let Some(entry) = inputs.borrow().get(&(owner, key)) {
            *entry.synced.borrow_mut() = value.clone();
        }
    });
    input.update(cx, |input, cx| input.set_value(value, window, cx));
}
