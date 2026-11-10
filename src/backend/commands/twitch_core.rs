//! Pure helpers for Twitch command argument parsing.

pub fn parse_banid_args(args: &str) -> Result<&str, &'static str> {
    single_token(args, "Usage: /banid <userID>")
}

pub fn parse_delete_args(args: &str) -> Result<&str, &'static str> {
    single_token(args, "Usage: /delete <message_id>")
}

const MAX_TIMEOUT_SECONDS: u64 = 1_209_600;

/// Parse the optional timeout duration token. `Ok(None)` means the token is
/// ordinary reason text; numeric-looking malformed tokens fail instead of
/// silently becoming a default-duration reason.
pub fn parse_timeout_duration_token(value: &str) -> Result<Option<u64>, &'static str> {
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if matches!(value.as_bytes()[0], b'-' | b'+' | b'.')
        && value.as_bytes().get(1).is_some_and(u8::is_ascii_digit)
    {
        return Err("Timeout duration must be a positive whole number");
    }

    let (amount_text, multiplier) = match value.as_bytes().last().copied() {
        Some(b's' | b'S') => (&value[..value.len() - 1], 1),
        Some(b'm' | b'M') => (&value[..value.len() - 1], 60),
        Some(b'h' | b'H') => (&value[..value.len() - 1], 60 * 60),
        Some(b'd' | b'D') => (&value[..value.len() - 1], 24 * 60 * 60),
        Some(last) if last.is_ascii_digit() => (value, 1),
        Some(_) if value.as_bytes()[0].is_ascii_digit() => {
            return Err("Timeout duration must be a number with optional s, m, h, or d suffix");
        }
        Some(_) | None => return Ok(None),
    };

    if amount_text.is_empty() || !amount_text.bytes().all(|byte| byte.is_ascii_digit()) {
        return if value.as_bytes()[0].is_ascii_digit() {
            Err("Timeout duration must be a whole number")
        } else {
            Ok(None)
        };
    }

    let amount = amount_text
        .parse::<u64>()
        .map_err(|_| "Timeout duration must be a whole number")?;
    let seconds = amount
        .checked_mul(multiplier)
        .filter(|seconds| (1..=MAX_TIMEOUT_SECONDS).contains(seconds))
        .ok_or("Timeout duration must be between 1 second and 14 days")?;
    Ok(Some(seconds))
}

pub fn parse_single_username<'a>(
    args: &'a str,
    usage: &'static str,
) -> Result<&'a str, &'static str> {
    let username = single_token(args, usage)?.trim_start_matches('@');
    if username.is_empty() {
        return Err(usage);
    }
    Ok(username)
}

fn single_token<'a>(args: &'a str, usage: &'static str) -> Result<&'a str, &'static str> {
    let mut parts = args.split_whitespace();
    let Some(value) = parts.next() else {
        return Err(usage);
    };
    if parts.next().is_some() {
        return Err(usage);
    }
    Ok(value)
}
