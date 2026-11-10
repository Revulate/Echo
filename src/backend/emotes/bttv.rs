//! BetterTTV global and channel emotes: response types, fetchers and conversion.

use super::*;

#[derive(Deserialize)]
struct BTTVEmote {
    id: String,
    code: String,
    #[serde(default)]
    animated: bool,
    #[serde(default)]
    width: Option<u32>,
    #[serde(default)]
    height: Option<u32>,
}

#[derive(Deserialize)]
struct BTTVChannelResponse {
    #[serde(rename = "channelEmotes")]
    channel_emotes: Vec<BTTVEmote>,
    #[serde(rename = "sharedEmotes")]
    shared_emotes: Vec<BTTVEmote>,
}

pub(super) async fn fetch_bttv_global() -> Result<Vec<Emote>, String> {
    BTTV_LIMITER.until_ready().await;
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        api::bttv::GLOBAL_EMOTES,
        EMOTE_MAX_RETRY_AFTER,
        Some(&BTTV_LIMITER),
    )
    .await?;
    if !resp.status().is_success() {
        return Err(format!(
            "BTTV global emotes returned status: {}",
            resp.status()
        ));
    }
    let emotes: Vec<BTTVEmote> = resp.json().await.map_err(|e| e.to_string())?;
    Ok(convert_bttv_emotes(emotes, None))
}

pub(super) async fn fetch_bttv_channel(
    channel_id: &str,
    channel_login: &str,
) -> Result<Vec<Emote>, String> {
    BTTV_LIMITER.until_ready().await;

    let url = api::bttv::channel(channel_id);
    let resp = fetch_with_retry_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        EMOTE_MAX_RETRY_AFTER,
        Some(&BTTV_LIMITER),
    )
    .await?;

    // BTTV answers 404 for a channel that has never set up BTTV emotes.
    if resp.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(vec![]);
    }
    if !resp.status().is_success() {
        return Err(format!(
            "BTTV channel API returned status: {}",
            resp.status()
        ));
    }

    let response: BTTVChannelResponse = resp.json().await.map_err(|e| e.to_string())?;

    let mut emotes = convert_bttv_emotes(response.channel_emotes, Some(channel_login));
    emotes.extend(convert_bttv_emotes(
        response.shared_emotes,
        Some(channel_login),
    ));
    Ok(emotes)
}

/// Build BTTV emote CDN URL for a given scale (1x, 2x, 3x).
#[inline]
fn bttv_emote_url(id: &str, scale: &str) -> String {
    const BTTV_CDN: &str = "https://cdn.betterttv.net/emote";
    format!("{BTTV_CDN}/{id}/{scale}")
}

fn convert_bttv_emotes(emotes: Vec<BTTVEmote>, channel: Option<&str>) -> Vec<Emote> {
    emotes
        .into_iter()
        .map(|e| {
            let is_animated = e.animated;
            let (width, height) = match (e.width, e.height) {
                (Some(width), Some(height)) if width > 0 && height > 0 => {
                    (Some(width), Some(height))
                }
                _ => (None, None),
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.code),
                url: Arc::from(bttv_emote_url(&e.id, "1x")),
                provider: EmoteProvider::BTTV,
                channel: channel.map(Arc::from),
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: None,
                url_1x: Some(Arc::from(bttv_emote_url(&e.id, "1x"))),
                url_2x: Some(Arc::from(bttv_emote_url(&e.id, "2x"))),
                url_4x: Some(Arc::from(bttv_emote_url(&e.id, "3x"))),
                zero_width: false,
                is_animated,
                width,
                height,
            }
        })
        .collect()
}
