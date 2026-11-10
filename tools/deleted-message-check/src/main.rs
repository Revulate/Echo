mod settings {
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub enum DeletedMessageMode {
        Disable,
        Hide,
        Replace,
        Strikethrough,
        Dim,
    }
}

#[path = "../../../src/backend/deleted_message_display.rs"]
mod deleted_message_display;

use deleted_message_display::{
    DELETED_MESSAGE_PLACEHOLDER, DeletedMessageDisplayAction, action_for_deleted_message_mode,
};
use settings::DeletedMessageMode;

fn action_name(action: DeletedMessageDisplayAction) -> &'static str {
    match action {
        DeletedMessageDisplayAction::Ignore => "ignore",
        DeletedMessageDisplayAction::RemoveOriginal => "remove-original",
        DeletedMessageDisplayAction::ReplaceWithPlaceholder => "replace-with-placeholder",
        DeletedMessageDisplayAction::MarkDeleted => "mark-deleted",
    }
}

fn main() {
    let cases = [
        (DeletedMessageMode::Disable, "disable", "ignore"),
        (DeletedMessageMode::Hide, "hide", "remove-original"),
        (
            DeletedMessageMode::Replace,
            "replace",
            "replace-with-placeholder",
        ),
        (
            DeletedMessageMode::Strikethrough,
            "strikethrough",
            "mark-deleted",
        ),
        (DeletedMessageMode::Dim, "dim", "mark-deleted"),
    ];

    assert_eq!(DELETED_MESSAGE_PLACEHOLDER, "<message deleted>");

    println!("mode,action,placeholder");
    for (mode, mode_name, expected_action) in cases {
        let action = action_for_deleted_message_mode(mode);
        let action = action_name(action);
        assert_eq!(action, expected_action, "action mismatch for {mode_name}");
        println!("{mode_name},{action},{DELETED_MESSAGE_PLACEHOLDER}");
    }
}
