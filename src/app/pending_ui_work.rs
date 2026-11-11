//! The pending-UI-work pass: each backend queue that needs EchoApp-owned
//! entities is drained by its own step, and [`EchoApp::process_pending_ui_work`]
//! runs the steps in a fixed order.

use super::{EchoApp, TabCloseScope, find_open_split_channel_target, find_open_tab_channel_target};
use crate::backend;
use crate::backend::lock_utils::MutexExt;
use crate::components::chat::{ChatInputRenderer, ChatView, ChatViewDirtyReason};
use crate::state::SplitId;
use gpui::*;

impl EchoApp {
    /// Apply `edit` to a split's draft. When it reports a change, reset the
    /// composer's completion affinity and history navigation and return the
    /// new draft text.
    fn edit_composer(
        &mut self,
        split_id: SplitId,
        cx: &mut Context<Self>,
        edit: impl FnOnce(&mut ChatInputRenderer, &mut Context<ChatInputRenderer>) -> bool,
    ) -> Option<String> {
        self.chat_input_renderer.update(cx, |renderer, cx| {
            if !edit(renderer, cx) {
                return None;
            }
            renderer.reset_composer_affinity(split_id);
            renderer.input_manager.end_history_navigation(split_id);
            renderer.input_manager.draft(split_id).cloned()
        })
    }

    /// Scroll a chat view to `message_id`; returns whether the message is laid out.
    fn jump_chat_view(chat_view: &Entity<ChatView>, message_id: &str, cx: &mut App) -> bool {
        chat_view.update(cx, |view, cx| {
            let found = view
                .canvas_list
                .update(cx, |list, cx| list.scroll_to_message(message_id, cx));
            view.mark_dirty_for(ChatViewDirtyReason::SearchJump);
            cx.notify();
            found
        })
    }

    /// Drain every pending UI queue that needs EchoApp-owned entities, in a
    /// fixed order. Returns whether any of them did work.
    pub(super) fn process_pending_ui_work(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        processed |= self.drain_open_login_request(cx);
        processed |= self.drain_composer_edits(cx);
        processed |= self.drain_message_composer_inserts(cx);
        processed |= self.process_picker_actions(cx);
        processed |= self.drain_confirmed_file_uploads(cx);
        processed |= self.drain_completed_sends(cx);
        processed |= self.drain_failed_sends(cx);
        processed |= self.drain_uploaded_links(cx);
        processed |= self.drain_mentions(cx);
        processed |= self.drain_open_new_tab_request(cx);
        processed |= self.drain_followed_channel_opens(cx);
        processed |= self.process_pending_chatterino_import(cx);
        processed |= self.drain_bulk_tab_closes(cx);
        processed |= self.offer_closed_tab_undo(cx);
        processed |= self.drain_reopen_closed_tab_requests(cx);
        processed |= self.drain_open_whispers_split_request(cx);
        processed |= self.drain_refocus_active_input_request(cx);
        processed |= self.drain_whisper_composes(cx);
        processed |= self.drain_quick_switches(cx);
        processed |= self.drain_replies(cx);
        processed |= self.drain_thread_views(cx);
        processed |= self.drain_split_message_jumps(cx);
        processed |= self.drain_search_jumps(cx);
        processed
    }

    /// Open the login overlay when a toast or command asked for it.
    fn drain_open_login_request(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        if backend::ui_actions::take_pending_open_login() {
            self.overlay_manager.update(cx, |overlays, cx| {
                overlays.open_login(cx);
            });
            processed = true;
        }

        processed
    }

    /// Apply context-menu composer edits (undo, redo, paste, cut, select all).
    fn drain_composer_edits(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for mut edit in backend::ui_actions::drain_pending_composer_edits() {
            if matches!(
                edit.action,
                backend::ui_actions::ComposerEditAction::PasteClipboard
            ) {
                match crate::utils::read_text(cx) {
                    Some(text) if !text.is_empty() => {
                        edit.action = backend::ui_actions::ComposerEditAction::Paste(text);
                    }
                    _ => continue,
                }
            }
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_channel_target(app_state, edit.split_id, "")
            };
            let Some((tab_index, path, channel)) = target else {
                continue;
            };
            processed = true;
            let is_paste = matches!(
                &edit.action,
                backend::ui_actions::ComposerEditAction::Paste(_)
            );
            if let backend::ui_actions::ComposerEditAction::Paste(text) = &edit.action {
                let offered = self.chat_input_renderer.update(cx, |renderer, cx| {
                    renderer.offer_multiline_paste(edit.split_id, &channel, text, cx)
                });
                if offered {
                    self.notify_split_leaf(edit.split_id, cx);
                    continue;
                }
            }

            let mut copied_text = None;
            let current_text = self.edit_composer(edit.split_id, cx, |renderer, cx| {
                use backend::ui_actions::ComposerEditAction;
                match edit.action {
                    ComposerEditAction::Undo => renderer.input_manager.undo(edit.split_id),
                    ComposerEditAction::Redo => renderer.input_manager.redo(edit.split_id),
                    ComposerEditAction::Paste(text) => {
                        renderer.insert_composer_paste(edit.split_id, &channel, &text, cx)
                    }
                    // Resolved into `Paste` when the edit was drained.
                    ComposerEditAction::PasteClipboard => false,
                    ComposerEditAction::DeleteSelection {
                        range,
                        expected_text,
                        copy,
                    } => {
                        let removed = renderer.input_manager.delete_exact_selection(
                            edit.split_id,
                            range,
                            &expected_text,
                        );
                        if copy {
                            copied_text = removed.clone();
                        }
                        removed.is_some()
                    }
                    ComposerEditAction::SelectAll => {
                        renderer.input_manager.select_all(edit.split_id)
                    }
                }
            });
            if let Some(copied_text) = copied_text {
                crate::utils::copy_text(&copied_text);
            }
            if let Some(current_text) = current_text {
                self.notify_split_leaf(edit.split_id, cx);
                let is_active = tab_index == self.app_state.read(cx).active_tab_idx
                    && self.split_layout.read(cx).get_active_split_path() == path;
                if is_active {
                    self.update_autocomplete(&channel, &path, Some(&current_text), cx);
                }
            } else if is_paste {
                self.notify_split_leaf(edit.split_id, cx);
            }
        }

        processed
    }

    /// Insert message text into the composer of its source split or channel.
    fn drain_message_composer_inserts(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for insertion in backend::drain_pending_message_composer_inserts() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                insertion.source_split_id.map_or_else(
                    || find_open_tab_channel_target(app_state, &insertion.channel),
                    |split_id| {
                        find_open_split_channel_target(app_state, split_id, &insertion.channel)
                    },
                )
            };
            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    info,
                    channel = %insertion.channel,
                    source_split_id = ?insertion.source_split_id,
                    "Skipped message-text insertion because its source or matching composer is no longer open"
                );
                continue;
            };

            self.activate_tab_split(tab_index, path.clone(), cx);
            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&path, cx) else {
                continue;
            };

            let current_text = self.edit_composer(split_id, cx, |renderer, _| {
                renderer
                    .input_manager
                    .insert_text_at_cursor(split_id, &insertion.text)
            });

            if let Some(current_text) = current_text {
                self.focus_split_input(split_id, cx);
                self.update_autocomplete(&channel, &path, Some(&current_text), cx);
                crate::log_ui!(
                    debug,
                    split_id = %split_id,
                    channel = %channel,
                    "Inserted message text into matching composer draft"
                );
            }
        }

        processed
    }

    /// Start uploads the user confirmed, unless their composer target changed.
    fn drain_confirmed_file_uploads(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for upload in backend::ui_actions::drain_confirmed_file_uploads() {
            processed = true;
            if upload.account == backend::ui_actions::composer_account()
                && self.composer_target_is_current(upload.split_id, &upload.channel, cx)
            {
                backend::upload_image_files_async(
                    upload.channel,
                    upload.split_id,
                    upload.paths,
                    upload.source,
                );
            } else {
                self.cancelled_upload_target(cx);
            }
        }

        processed
    }

    /// Finish composer sends the backend delivered.
    fn drain_completed_sends(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for completed in backend::ui_actions::drain_pending_completed_sends() {
            processed = true;
            let split_id = completed.origin.split_id;
            let still_open = self.composer_target_is_current(split_id, &completed.channel, cx);
            let matched = self.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.finish_send(
                    &completed.origin,
                    &completed.channel,
                    completed.notice.clone(),
                    cx,
                )
            });
            if matched && still_open {
                self.notify_split_leaf(split_id, cx);
            }
            self.advance_pasted_send(
                &completed.origin,
                matched && still_open && completed.notice.is_none(),
                completed.notice.as_deref().unwrap_or("Target changed"),
                cx,
            );
        }

        processed
    }

    /// Report failed sends and restore the text into an empty draft.
    fn drain_failed_sends(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for failed in backend::ui_actions::drain_pending_failed_sends() {
            processed = true;
            let split_id = failed.origin.split_id;
            let notice = if failed.delivery_unknown {
                "Not confirmed — check chat".to_string()
            } else {
                backend::ui_actions::composer_send::rejection_reason("", &failed.error)
            };
            let matched = self.chat_input_renderer.update(cx, |renderer, cx| {
                renderer.finish_send(&failed.origin, &failed.channel, Some(notice), cx)
            });
            let still_open = self.composer_target_is_current(split_id, &failed.channel, cx);
            if !matched || !still_open {
                if failed.origin.account != backend::ui_actions::composer_account() {
                    let title = if failed.delivery_unknown {
                        "Previous-account send not confirmed"
                    } else {
                        "Previous-account message was not sent"
                    };
                    self.overlay_manager.update(cx, |manager, cx| {
                        manager.push_toast(
                            crate::components::overlays::Toast::warning(title).with_detail(
                                format!(
                                    "To #{}: {}. The text remains in that channel’s input history.",
                                    failed.channel,
                                    backend::ui_actions::composer_send::rejection_reason(
                                        "",
                                        &failed.error
                                    )
                                ),
                            ),
                            cx,
                        )
                    });
                    if still_open {
                        backend::bridge::publish_failed_send_row(
                            &failed.channel,
                            "Previous-account submission",
                            &failed.error,
                            true,
                            failed.delivery_unknown,
                        );
                    }
                }
                self.advance_pasted_send(&failed.origin, false, "Target changed", cx);
                continue;
            }
            let origin = failed.origin.clone();
            self.notify_split_leaf(split_id, cx);
            let restored = still_open
                && !failed.delivery_unknown
                && self.chat_input_renderer.update(cx, |renderer, _| {
                    let draft_empty = renderer
                        .input_manager
                        .draft(split_id)
                        .is_none_or(|text| text.is_empty());
                    if !draft_empty {
                        return false;
                    }
                    let restored = renderer
                        .input_manager
                        .insert_text_at_cursor(split_id, &failed.origin.text);
                    if restored {
                        renderer.reset_composer_affinity(split_id);
                    }
                    restored
                });
            if restored {
                if let Some(reply) = failed.origin.reply
                    && let Some(pane) = self.split_panes.get(&split_id).cloned()
                {
                    pane.update(cx, |pane, _| {
                        if pane.reply_to.is_none() {
                            pane.reply_to = Some(reply);
                        }
                    });
                }
                self.notify_split_leaf(split_id, cx);
            }
            backend::bridge::publish_failed_send_row(
                &failed.channel,
                failed.action,
                &failed.error,
                !restored,
                failed.delivery_unknown,
            );
            self.advance_pasted_send(
                &origin,
                false,
                if failed.delivery_unknown {
                    "Not confirmed — check chat"
                } else {
                    &failed.error
                },
                cx,
            );
        }

        processed
    }

    /// Insert finished image-upload links into the originating split draft.
    fn drain_uploaded_links(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for upload in backend::drain_pending_uploaded_links() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_channel_target(app_state, upload.split_id, &upload.channel)
            };
            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    info,
                    split_id = %upload.split_id,
                    channel = %upload.channel,
                    "Skipped uploaded-link insertion because the originating split no longer matches"
                );
                continue;
            };

            let inserted_text = format!("{} ", upload.url);
            let current_text = self.edit_composer(upload.split_id, cx, |renderer, _| {
                renderer
                    .input_manager
                    .insert_text_at_cursor(upload.split_id, &inserted_text)
            });

            if current_text.is_some() {
                let is_active_origin = tab_index == self.app_state.read(cx).active_tab_idx
                    && self.split_layout.read(cx).get_active_split_path() == path;
                if is_active_origin {
                    self.focus_split_input(upload.split_id, cx);
                    self.notify_split_leaf(upload.split_id, cx);
                    self.update_autocomplete(&channel, &path, current_text.as_deref(), cx);
                }
                crate::log_ui!(
                    debug,
                    split_id = %upload.split_id,
                    channel = %channel,
                    "Inserted uploaded image link into originating split draft"
                );
            }
        }

        processed
    }

    /// Insert @mentions into the source split, or the active one.
    fn drain_mentions(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for mention in backend::drain_pending_mentions() {
            processed = true;
            let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
            let fallback_channel = self
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .channel_at_path(&active_path)
                .map(|s| s.to_string())
                .unwrap_or_else(|| mention.channel.clone());
            let target = {
                let app_state = self.app_state.read(cx);
                mention.source_split_id.map_or_else(
                    || find_open_tab_channel_target(app_state, &mention.channel),
                    |split_id| {
                        find_open_split_channel_target(app_state, split_id, &mention.channel)
                    },
                )
            };
            if target.is_none()
                && (mention.source_split_id.is_some() || !mention.channel.trim().is_empty())
            {
                crate::log_ui!(
                    info,
                    source_split_id = ?mention.source_split_id,
                    channel = %mention.channel,
                    "Skipped mention insertion because its source split is stale"
                );
                continue;
            }
            let (target_channel, target_path) = if let Some((tab_index, path, channel)) = target {
                self.activate_tab_split(tab_index, path.clone(), cx);
                (channel, path)
            } else {
                (fallback_channel, active_path)
            };

            if !target_channel.is_empty() {
                let Some((_resolved_channel, split_id)) =
                    self.split_target_for_path(&target_path, cx)
                else {
                    continue;
                };
                let inserted = self.chat_input_renderer.update(cx, |renderer, _| {
                    let before = renderer.input_manager.draft(split_id).cloned();
                    renderer
                        .input_manager
                        .insert_mention(split_id, &mention.username);
                    renderer.reset_composer_affinity(split_id);
                    renderer.input_manager.draft(split_id).cloned() != before
                });
                if inserted {
                    self.focus_split_input(split_id, cx);
                    let current_text = self
                        .chat_input_renderer
                        .read(cx)
                        .input_manager
                        .draft(split_id)
                        .cloned();
                    self.update_autocomplete(
                        &target_channel,
                        &target_path,
                        current_text.as_deref(),
                        cx,
                    );
                    self.notify_split_leaf(split_id, cx);
                }
                crate::log_ui!(
                    debug,
                    "Inserted @mention '{}' into channel '{}'",
                    mention.username,
                    target_channel
                );
            }
        }

        processed
    }

    /// Open a new tab when a command asked for one.
    fn drain_open_new_tab_request(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        if backend::take_pending_open_new_tab() {
            processed = true;
            self.create_new_tab(cx);
        }

        processed
    }

    /// Open channels picked from the followed-live list.
    fn drain_followed_channel_opens(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for open in backend::ui_actions::drain_pending_followed_channel_opens() {
            processed = true;
            self.open_followed_channel(open.login, open.fill_split, cx);
        }

        processed
    }

    /// Close the tabs to the right of, or other than, a tab.
    fn drain_bulk_tab_closes(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for close in backend::ui_actions::drain_pending_bulk_tab_closes() {
            let scope = if close.to_right {
                TabCloseScope::ToRight
            } else {
                TabCloseScope::Others
            };
            processed |= self.close_tabs_by_id(&close.tab_id, scope, cx);
        }

        processed
    }

    /// Show an Undo toast after tabs were closed.
    fn offer_closed_tab_undo(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        let closed_tab_generation = self.app_state.read(cx).closed_tab_generation();
        if closed_tab_generation != self.seen_closed_tab_generation {
            self.seen_closed_tab_generation = closed_tab_generation;
            processed = true;
            let count = self.app_state.read(cx).last_closed_tab_count();
            if count > 0 {
                let message = if count == 1 {
                    "Tab closed".to_string()
                } else {
                    format!("{count} tabs closed")
                };
                let requests = self.undo_closed_tab_requests.clone();
                let toast = crate::components::overlays::Toast::info(message).with_action(
                    "Undo",
                    move || {
                        requests.lock_or_recover().push(closed_tab_generation);
                        backend::ui_actions::push_pending_reopen_closed_tab();
                    },
                );
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
        }

        processed
    }

    /// Reopen the tab batches whose Undo was clicked.
    fn drain_reopen_closed_tab_requests(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        if backend::ui_actions::take_pending_reopen_closed_tab() {
            processed = true;
            let generations = std::mem::take(&mut *self.undo_closed_tab_requests.lock_or_recover());
            for generation in generations {
                let reopened = self.app_state.update(cx, |state, cx| {
                    state.reopen_closed_tab_batch(generation, cx)
                });
                self.finish_reopen_closed_tabs(reopened, cx);
            }
        }

        processed
    }

    /// Open or focus the Whispers split when asked.
    fn drain_open_whispers_split_request(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        if backend::take_pending_open_whispers_split() {
            processed = true;
            let _ = self.open_or_focus_whispers_split(cx);
        }

        processed
    }

    /// Return keyboard focus to the active composer when asked.
    fn drain_refocus_active_input_request(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        if backend::ui_actions::take_pending_refocus_active_input() {
            processed = true;
            self.refocus_active_input(cx);
        }

        processed
    }

    /// Prepare whisper drafts in the Whispers split.
    fn drain_whisper_composes(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for compose in backend::drain_pending_whisper_composes() {
            processed = true;
            let Some(target_path) = self.open_or_focus_whispers_split(cx) else {
                continue;
            };
            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&target_path, cx)
            else {
                continue;
            };

            let inserted = self.chat_input_renderer.update(cx, |renderer, _| {
                let inserted = renderer
                    .input_manager
                    .compose_whisper(split_id, &compose.username);
                if inserted {
                    renderer.reset_composer_affinity(split_id);
                }
                inserted
            });

            if inserted {
                backend::mark_whisper_thread_read(&compose.username);
                self.app_state.update(cx, |state, _| {
                    state.refresh_sidebar_caches();
                });
                crate::log_ui!(
                    debug,
                    "Prepared whisper draft for '{}' in the Whispers split (requested from {:?})",
                    compose.username,
                    compose.channel
                );
            } else {
                let toast =
                    crate::components::overlays::Toast::warning("Whisper draft not inserted")
                        .with_detail("Clear the target input or existing whisper draft first.");
                self.overlay_manager.update(cx, |manager, cx| {
                    manager.push_toast(toast, cx);
                });
            }
        }

        processed
    }

    /// Activate the splits picked in the quick switcher.
    fn drain_quick_switches(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for selection in backend::drain_pending_quick_switches() {
            processed = true;
            if self
                .app_state
                .read(cx)
                .tabs
                .get(selection.tab_index)
                .is_some()
            {
                self.activate_tab_split(selection.tab_index, selection.split_path, cx);
                self.refocus_active_input(cx);
            }
        }

        processed
    }

    /// Start replies in the source split, or the active one.
    fn drain_replies(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for reply in backend::drain_pending_replies() {
            processed = true;
            let active_path = self.split_layout.read(cx).get_active_split_path().to_vec();
            let target = {
                let app_state = self.app_state.read(cx);
                reply.source_split_id.map_or_else(
                    || {
                        find_open_tab_channel_target(app_state, &reply.channel)
                            .map(|(tab_index, path, _)| (tab_index, path))
                    },
                    |split_id| {
                        find_open_split_channel_target(app_state, split_id, &reply.channel)
                            .map(|(tab_index, path, _)| (tab_index, path))
                    },
                )
            };
            if target.is_none()
                && (reply.source_split_id.is_some() || !reply.channel.trim().is_empty())
            {
                crate::log_ui!(
                    info,
                    source_split_id = ?reply.source_split_id,
                    channel = %reply.channel,
                    "Skipped reply activation because its source split is stale"
                );
                continue;
            }
            let target_path = if let Some((tab_index, path)) = target {
                self.activate_tab_split(tab_index, path.clone(), cx);
                path
            } else {
                active_path
            };
            let split_id = self
                .app_state
                .read(cx)
                .active_tab()
                .layout
                .split_id_at_path(&target_path);

            if let Some(split_id) = split_id {
                if let Some(pane) = self.split_panes.get(&split_id) {
                    pane.update(cx, |p, _| {
                        p.reply_to = Some(crate::state::ReplyState {
                            message_id: reply.message_id.clone(),
                            username: reply.sender.clone(),
                            preview: reply.content.clone(),
                        });
                    });
                }

                self.focus_split_input(split_id, cx);
                self.notify_split_leaf(split_id, cx);

                crate::log_ui!(
                    debug,
                    "Activated reply in split: channel='{}' msg_id='{}' display_name='{}'",
                    reply.channel,
                    reply.message_id,
                    reply.display_name
                );
            }
        }

        processed
    }

    /// Open the thread viewer for requested messages.
    fn drain_thread_views(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for view in backend::drain_pending_thread_views() {
            processed = true;
            self.overlay_manager.update(cx, |manager, cx| {
                manager.open_thread_viewer(
                    view.message_id.clone(),
                    view.channel.clone(),
                    view.x,
                    view.y,
                    cx,
                );
                cx.notify();
            });
        }

        processed
    }

    /// Scroll a split to a message, waiting for its chat view if needed.
    fn drain_split_message_jumps(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for jump in backend::drain_pending_split_message_jumps() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_split_channel_target(app_state, jump.split_id, &jump.channel)
            };

            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    debug,
                    "Split-local message jump dropped because its source split is gone: split_id={} channel={}",
                    jump.split_id,
                    jump.channel
                );
                continue;
            };

            self.activate_tab_split(tab_index, path, cx);

            if let Some(chat_view) = self.chat_views.get(&jump.split_id).filter(|chat_view| {
                chat_view
                    .read(cx)
                    .channel
                    .trim_start_matches('#')
                    .eq_ignore_ascii_case(channel.trim_start_matches('#'))
            }) {
                let found = Self::jump_chat_view(chat_view, &jump.message_id, cx);
                if !found {
                    self.report_missing_jump_target(&channel, &jump.message_id, cx);
                }
            } else {
                crate::log_ui!(
                    debug,
                    "Split-local message jump waiting for a matching ChatView: split_id={} channel={}",
                    jump.split_id,
                    channel
                );
                backend::push_pending_split_message_jump(
                    jump.split_id,
                    jump.channel,
                    jump.message_id,
                );
            }
        }

        processed
    }

    /// Scroll to search results in the first open split for their channel.
    fn drain_search_jumps(&mut self, cx: &mut Context<Self>) -> bool {
        let mut processed = false;
        for jump in backend::drain_pending_search_jumps() {
            processed = true;
            let target = {
                let app_state = self.app_state.read(cx);
                find_open_tab_channel_target(app_state, &jump.channel)
            };

            let Some((tab_index, path, channel)) = target else {
                crate::log_ui!(
                    warn,
                    "Search result target channel is not visible in any open tab: {}",
                    jump.channel
                );
                self.overlay_manager.update(cx, |overlays, cx| {
                    overlays.toast_info(
                        format!(
                            "#{} is no longer open",
                            jump.channel.trim_start_matches('#')
                        ),
                        cx,
                    );
                });
                continue;
            };

            self.activate_tab_split(tab_index, path.clone(), cx);

            let Some((_resolved_channel, split_id)) = self.split_target_for_path(&path, cx) else {
                backend::push_pending_search_jump(channel.clone(), jump.message_id.clone());
                continue;
            };

            if let Some(chat_view) = self.chat_views.get(&split_id) {
                let found = Self::jump_chat_view(chat_view, &jump.message_id, cx);
                if !found {
                    self.report_missing_jump_target(&channel, &jump.message_id, cx);
                }
            } else {
                crate::log_ui!(
                    debug,
                    "Search result target split has no chat view yet, retrying after tab/split activation: channel='{}' path={:?}",
                    channel,
                    path
                );
                backend::push_pending_search_jump(channel.clone(), jump.message_id.clone());
            }
        }

        processed
    }
}
