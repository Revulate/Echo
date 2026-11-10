//! Shared CLEARMSG display-mode decisions.

use super::settings::DeletedMessageMode;

pub const DELETED_MESSAGE_PLACEHOLDER: &str = "<message deleted>";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeletedMessageDisplayAction {
    Ignore,
    RemoveOriginal,
    ReplaceWithPlaceholder,
    MarkDeleted,
}

pub fn action_for_deleted_message_mode(mode: DeletedMessageMode) -> DeletedMessageDisplayAction {
    match mode {
        DeletedMessageMode::Disable => DeletedMessageDisplayAction::Ignore,
        DeletedMessageMode::Hide => DeletedMessageDisplayAction::RemoveOriginal,
        DeletedMessageMode::Replace => DeletedMessageDisplayAction::ReplaceWithPlaceholder,
        DeletedMessageMode::Dim | DeletedMessageMode::Strikethrough => {
            DeletedMessageDisplayAction::MarkDeleted
        }
    }
}
