//! Text for unread/mention count badges.

/// Counts above this render as `"99+"` so badges keep a fixed maximum width.
const MAX_DISPLAYED_COUNT: usize = 99;

/// Badge text for an unread or mention count: the number itself, or `"99+"`
/// once it exceeds two digits.
pub fn capped_count_label(count: usize) -> String {
    if count > MAX_DISPLAYED_COUNT {
        format!("{MAX_DISPLAYED_COUNT}+")
    } else {
        count.to_string()
    }
}
