use crate::EchoApp;
use crate::components::ui::focus_ring;
use crate::managers::split_layout_manager::DividerResize;
use crate::state::{SplitNode, SplitOrientation, is_placeholder};
use crate::theme::colors;
use gpui::prelude::FluentBuilder;
use gpui::*;
use std::rc::Rc;

/// Add keyboard and AccessKit behavior to an existing divider without changing its hitbox.
pub fn accessible_split_divider(
    divider: Stateful<Div>,
    app: &EchoApp,
    path: &[usize],
    child_idx: usize,
    band: Option<usize>,
    cx: &mut Context<EchoApp>,
) -> Stateful<Div> {
    let tab = app.app_state.read(cx).active_tab();
    let Some(node @ SplitNode::Split { children, .. }) = tab.layout.node_at_path(path) else {
        return divider;
    };
    let grid_bands = if band.is_none() && child_idx == 0 {
        app.split_layout
            .read(cx)
            .divider_band_extents(&tab.layout, path)
    } else {
        None
    };
    if let Some((orientation, extents)) = grid_bands {
        let bands = extents
            .into_iter()
            .enumerate()
            .map(|(band, extent)| {
                let offset = if band == 0 { 0.0 } else { extents[0] };
                let band_div = div().id(("keyboard-grid-band", band)).absolute();
                let band_div = match orientation {
                    SplitOrientation::Vertical => {
                        band_div.top(px(offset)).left_0().w_full().h(px(extent))
                    }
                    SplitOrientation::Horizontal => {
                        band_div.left(px(offset)).top_0().h_full().w(px(extent))
                    }
                };
                accessible_split_divider(band_div, app, path, child_idx, Some(band), cx)
                    .into_any_element()
            })
            .collect::<Vec<_>>();
        return divider.relative().children(bands);
    }
    let state = if let Some(band) = band {
        app.split_layout
            .read(cx)
            .divider_band_state(&tab.layout, path, band)
    } else {
        app.split_layout
            .read(cx)
            .divider_state(&tab.layout, path, child_idx)
    };
    let Some(state) = state else {
        return divider;
    };
    let pair = if let Some(band) = band {
        children
            .first()
            .zip(children.get(1))
            .and_then(|(first, second)| match (first, second) {
                (
                    SplitNode::Split {
                        children: first, ..
                    },
                    SplitNode::Split {
                        children: second, ..
                    },
                ) => first.get(band).zip(second.get(band)),
                _ => None,
            })
    } else {
        children.get(child_idx).zip(children.get(child_idx + 1))
    };
    let Some((first, second)) = pair else {
        return divider;
    };
    let Some(pair_ids) = first
        .leftmost_leaf_split_id()
        .zip(second.leftmost_leaf_split_id())
    else {
        return divider;
    };
    let source_orientation = state.orientation.clone();
    let (first_side, second_side, orientation) = match state.orientation {
        SplitOrientation::Vertical => ("left", "right", accesskit::Orientation::Vertical),
        SplitOrientation::Horizontal => ("top", "bottom", accesskit::Orientation::Horizontal),
    };
    let label = format!(
        "Resize {first_side} {} and {second_side} {}",
        pane_label(first),
        pane_label(second),
    );
    let locked = tab.is_locked;
    let tab_id = tab.id.clone();
    let node_id = node.split_id();
    let focus = app.split_layout.update(cx, |layout, cx| {
        layout.divider_focus(pair_ids.0, pair_ids.1, cx)
    });
    let path = path.to_vec();
    let app = cx.entity().downgrade();
    let resize = Rc::new(move |action, cx: &mut App| {
        let _ = app.update(cx, |app, cx| {
            let target_is_current = {
                let tab = app.app_state.read(cx).active_tab();
                tab.id == tab_id
                    && !tab.is_locked
                    && tab.layout.node_at_path(&path).is_some_and(|node| {
                        node.split_id() == node_id
                            && matches!(node, SplitNode::Split { orientation, .. } if *orientation == source_orientation)
                    })
            };
            if !target_is_current {
                return;
            }
            app.cancel_pointer_drags(cx);
            let changed = app.app_state.update(cx, |state, cx| {
                if let Some(band) = band {
                    app.split_layout.update(cx, |layout, _| {
                        layout.resize_grid_band(state.active_tab_mut(), &path, band, action)
                    })
                } else {
                    app.split_layout.read(cx).resize_divider(
                        std::sync::Arc::make_mut(&mut state.active_tab_mut().layout),
                        &path,
                        child_idx,
                        action,
                    )
                }
            });
            if changed {
                app.save_tabs(cx);
                cx.notify();
            }
        });
    });
    let keyboard_resize = resize.clone();
    let increment = resize.clone();
    let decrement = resize.clone();
    let ring = focus_ring(colors::bg_void());
    divider
        .role(Role::Splitter)
        .aria_label(label)
        .aria_description(
            "Arrow keys resize. Home and End select the limits. Enter equalizes the split.",
        )
        .aria_orientation(orientation)
        .aria_numeric_value(f64::from(state.value_percent))
        .aria_min_numeric_value(f64::from(state.min_percent))
        .aria_max_numeric_value(f64::from(state.max_percent))
        .aria_numeric_value_step(3.0)
        .when(locked, |divider| {
            divider.a11y_synthetic_children(|builder| {
                builder.parent_node().set_disabled();
            })
        })
        .when(!locked, |divider| {
            divider
                .track_focus(&focus)
                .tab_index(0)
                .focus_visible(move |style| style.bg(colors::accent()).shadow(ring))
                .capture_any_mouse_down(|event, window, _| {
                    if event.button == MouseButton::Left {
                        window.prevent_default();
                    }
                })
                .on_key_down(move |event, _, cx| {
                    let modifiers = event.keystroke.modifiers;
                    if modifiers.control || modifiers.alt || modifiers.platform || modifiers.shift {
                        return;
                    }
                    let action = match (orientation, event.keystroke.key.as_str()) {
                        (accesskit::Orientation::Vertical, "left")
                        | (accesskit::Orientation::Horizontal, "up") => DividerResize::Decrease,
                        (accesskit::Orientation::Vertical, "right")
                        | (accesskit::Orientation::Horizontal, "down") => DividerResize::Increase,
                        (_, "home") => DividerResize::Minimum,
                        (_, "end") => DividerResize::Maximum,
                        (_, "enter") => DividerResize::Equalize,
                        _ => return,
                    };
                    cx.stop_propagation();
                    keyboard_resize(action, cx);
                })
                .on_a11y_action(accesskit::Action::Increment, move |_, _, cx| {
                    increment(DividerResize::Increase, cx);
                })
                .on_a11y_action(accesskit::Action::Decrement, move |_, _, cx| {
                    decrement(DividerResize::Decrease, cx);
                })
                .on_a11y_action(accesskit::Action::SetValue, move |data, _, cx| {
                    if let Some(accesskit::ActionData::NumericValue(value)) = data {
                        resize(DividerResize::SetPercentage(*value as f32), cx);
                    }
                })
        })
}

fn pane_label(node: &SplitNode) -> String {
    match node {
        SplitNode::Leaf { channel, .. } if is_placeholder(channel) => "empty split".into(),
        SplitNode::Leaf { channel, .. }
            if crate::backend::browser_watching::is_watching_channel(channel) =>
        {
            "Watching".into()
        }
        SplitNode::Leaf { channel, .. } => channel.to_string(),
        SplitNode::Split { children, .. } => children.first().map_or_else(
            || "split group".into(),
            |child| format!("{} group", pane_label(child)),
        ),
    }
}
