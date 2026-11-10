//! FrankerFaceZ global and room emotes, and the room's VIP and moderator badges.

use super::*;

#[derive(Deserialize)]
struct FFZGlobalResponse {
    #[serde(default)]
    default_sets: Vec<i64>,
    #[serde(default)]
    sets: HashMap<String, FFZEmoteSet>,
}

#[derive(Deserialize)]
struct FFZRoomResponse {
    room: Option<FFZRoom>,
    sets: HashMap<String, FFZEmoteSet>,
}

/// FFZ sends `null` (not an omitted key) for a room without custom badge
/// artwork, e.g. `"mod_urls": null` beside a custom `vip_badge`.
#[derive(Deserialize)]
struct FFZRoom {
    twitch_id: i64,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    mod_urls: HashMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    vip_badge: HashMap<String, String>,
    #[serde(default, deserialize_with = "deserialize_null_as_default")]
    user_badge_ids: HashMap<String, Vec<serde_json::Value>>,
}

fn deserialize_null_as_default<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de> + Default,
{
    Ok(Option::<T>::deserialize(deserializer)?.unwrap_or_default())
}

#[derive(Deserialize, Default)]
struct FFZEmoteSet {
    #[serde(default)]
    emoticons: Vec<FFZEmote>,
}

#[derive(Deserialize)]
struct FFZEmote {
    #[serde(default)]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    urls: HashMap<String, String>,
    /// FFZ exposes animated variants in a parallel URL map. When present,
    /// these are the actual animated assets; the regular `urls` map may still
    /// contain static fallbacks.
    #[serde(default, deserialize_with = "deserialize_ffz_animated_urls")]
    animated: HashMap<String, String>,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

pub(super) async fn fetch_ffz_global() -> Result<Vec<Emote>, String> {
    FFZ_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        api::ffz::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&FFZ_LIMITER),
    )
    .await?;

    if !resp.status().is_success() {
        return Err(format!("FFZ API returned status: {}", resp.status()));
    }

    let body = resp
        .text()
        .await
        .map_err(|e| format!("Failed to read FFZ response: {}", e))?;

    let response: FFZGlobalResponse = serde_json::from_str(&body).map_err(|e| {
        crate::log_emotes!(
            debug,
            "FFZ parse error: {}. Response preview: {}",
            e,
            truncate_for_log(&body, 500)
        );
        format!("Failed to parse FFZ response: {}", e)
    })?;

    let total_emotes: usize = response
        .default_sets
        .iter()
        .filter_map(|id| response.sets.get(&id.to_string()))
        .map(|set| set.emoticons.len())
        .sum();
    let mut emotes = Vec::with_capacity(total_emotes);
    for set_id in &response.default_sets {
        if let Some(set) = response.sets.get(&set_id.to_string()) {
            emotes.extend(convert_ffz_emotes(&set.emoticons, None));
        }
    }
    crate::log_emotes!(info, "Loaded {} global FFZ emotes", emotes.len());
    Ok(emotes)
}

/// Fetch FFZ channel data and return both emotes and Twitch user ID
pub(super) async fn fetch_ffz_channel_with_id(
    channel_login: &str,
) -> Result<(Vec<Emote>, Option<String>), String> {
    let Some(response) = fetch_ffz_room(channel_login).await? else {
        publish_ffz_room_badges(channel_login, None);
        return Ok((vec![], None));
    };

    publish_ffz_room_badges(channel_login, response.room.as_ref());
    let twitch_id = response
        .room
        .as_ref()
        .map(|room| room.twitch_id.to_string());

    let total_emotes: usize = response.sets.values().map(|set| set.emoticons.len()).sum();
    let mut emotes = Vec::with_capacity(total_emotes);
    for set in response.sets.values() {
        emotes.extend(convert_ffz_emotes(&set.emoticons, Some(channel_login)));
    }
    Ok((emotes, twitch_id))
}

async fn fetch_ffz_room(channel_login: &str) -> Result<Option<FFZRoomResponse>, String> {
    FFZ_LIMITER.until_ready().await;

    let url = api::ffz::room(channel_login);
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&FFZ_LIMITER),
    )
    .await?;

    if !resp.status().is_success() {
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(None);
        }
        return Err(format!("FFZ room API returned status: {}", resp.status()));
    }

    let response: FFZRoomResponse = resp.json().await.map_err(|e| e.to_string())?;
    Ok(Some(response))
}

fn publish_ffz_room_badges(channel_login: &str, room: Option<&FFZRoom>) {
    let moderator_url = room.and_then(|room| preferred_ffz_room_badge_url(&room.mod_urls));
    let vip_url = room.and_then(|room| preferred_ffz_room_badge_url(&room.vip_badge));
    let badges_by_user_id = room
        .map(|room| invert_ffz_channel_badge_users(&room.user_badge_ids))
        .unwrap_or_default();

    crate::backend::badges::TWITCH_BADGE_CACHE.publish_ffz_channel_badges(
        channel_login,
        moderator_url,
        vip_url,
        badges_by_user_id,
    );
}

pub(super) fn schedule_ffz_room_badge_hydration(channel_login: &str) {
    if !crate::backend::settings::get_show_ffz_badges()
        || crate::backend::badges::TWITCH_BADGE_CACHE
            .has_ffz_channel_badge_state_sync(channel_login)
    {
        return;
    }

    let channel_login = normalize_channel_key(channel_login.trim());
    if channel_login.is_empty() || !FFZ_ROOM_BADGE_HYDRATIONS.insert(channel_login.clone()) {
        return;
    }

    crate::backend::bridge::RUNTIME.spawn(async move {
        match fetch_ffz_room(&channel_login).await {
            Ok(response) => {
                publish_ffz_room_badges(
                    &channel_login,
                    response
                        .as_ref()
                        .and_then(|response| response.room.as_ref()),
                );
            }
            Err(error) => {
                crate::log_cache!(
                    warn,
                    channel = channel_login,
                    %error,
                    "Failed to hydrate FFZ room badges beside fresh channel emote cache"
                );
            }
        }
        FFZ_ROOM_BADGE_HYDRATIONS.remove(&channel_login);
    });
}

pub(super) async fn fetch_ffz_channel(channel_login: &str) -> Result<Vec<Emote>, String> {
    let (emotes, _) = fetch_ffz_channel_with_id(channel_login).await?;
    Ok(emotes)
}

/// Normalize FFZ URL (often protocol-relative like "//cdn.frankerfacez.com/...").
fn normalize_ffz_url(urls: &HashMap<String, String>, key: &str) -> Option<String> {
    urls.get(key).map(|u| {
        if u.starts_with("//") {
            format!("https:{}", u)
        } else {
            u.clone()
        }
    })
}

fn preferred_ffz_room_badge_url(urls: &HashMap<String, String>) -> Option<String> {
    normalize_ffz_url(urls, "4")
        .or_else(|| normalize_ffz_url(urls, "2"))
        .or_else(|| normalize_ffz_url(urls, "1"))
}

fn invert_ffz_channel_badge_users(
    assignments: &HashMap<String, Vec<serde_json::Value>>,
) -> HashMap<String, Vec<u64>> {
    let mut by_user_id: HashMap<String, Vec<u64>> = HashMap::new();
    for (badge_id, users) in assignments {
        let Ok(badge_id) = badge_id.parse::<u64>() else {
            continue;
        };
        for user in users {
            let user_id = user
                .as_str()
                .map(str::to_string)
                .or_else(|| user.as_u64().map(|id| id.to_string()));
            let Some(user_id) = user_id.filter(|id| !id.is_empty()) else {
                continue;
            };
            let badges = by_user_id.entry(user_id).or_default();
            if !badges.contains(&badge_id) {
                badges.push(badge_id);
            }
        }
    }
    for badges in by_user_id.values_mut() {
        badges.sort_unstable();
    }
    by_user_id
}

#[inline]
fn url_looks_animated(url: &str) -> bool {
    #[inline]
    fn contains_ascii_case_insensitive(haystack: &[u8], needle: &[u8]) -> bool {
        haystack
            .windows(needle.len())
            .any(|window| window.eq_ignore_ascii_case(needle))
    }

    let bytes = url.as_bytes();
    bytes
        .get(bytes.len().saturating_sub(4)..)
        .is_some_and(|suffix| suffix.eq_ignore_ascii_case(b".gif"))
        || contains_ascii_case_insensitive(bytes, b"/animated/")
        || contains_ascii_case_insensitive(bytes, b"_animated.")
        || contains_ascii_case_insensitive(bytes, b"format=gif")
}

fn deserialize_ffz_animated_urls<'de, D>(
    deserializer: D,
) -> Result<HashMap<String, String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let value = serde_json::Value::deserialize(deserializer)?;
    Ok(value
        .as_object()
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|url| (key.clone(), url.to_owned())))
                .collect()
        })
        .unwrap_or_default())
}

fn convert_ffz_emotes(emotes: &[FFZEmote], channel: Option<&str>) -> Vec<Emote> {
    emotes
        .iter()
        .filter_map(|e| {
            let has_animated_map = !e.animated.is_empty();
            let animated_variant = |key: &str| {
                normalize_ffz_url(&e.animated, key)
                    .or_else(|| normalize_ffz_url(&e.animated, "2"))
                    .or_else(|| normalize_ffz_url(&e.animated, "1"))
                    .or_else(|| normalize_ffz_url(&e.animated, "4"))
            };
            let url_1x = animated_variant("1").or_else(|| normalize_ffz_url(&e.urls, "1"));
            let url_2x = animated_variant("2").or_else(|| normalize_ffz_url(&e.urls, "2"));
            let url_4x = animated_variant("4").or_else(|| normalize_ffz_url(&e.urls, "4"));

            let default_url = url_1x.clone().or(url_2x.clone()).or(url_4x.clone())?;

            let is_animated = has_animated_map
                || [url_1x.as_deref(), url_2x.as_deref(), url_4x.as_deref()]
                    .into_iter()
                    .flatten()
                    .any(url_looks_animated)
                || url_looks_animated(&default_url);
            let (width, height) = match (e.width, e.height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    (Some(width), Some(height))
                }
                _ => (None, None),
            };
            Some(Emote {
                id: Arc::from(e.id.to_string()),
                name: Arc::from(e.name.clone()),
                url: Arc::from(default_url),
                provider: EmoteProvider::FFZ,
                channel: channel.map(Arc::from),
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: url_1x.map(Arc::from),
                url_2x: url_2x.map(Arc::from),
                url_4x: url_4x.map(Arc::from),
                zero_width: false,
                is_animated,
                width,
                height,
            })
        })
        .collect()
}
