//! Twitch channel, global and subscriber (user) emotes from Helix.

use super::*;

#[derive(Deserialize)]
struct TwitchEmotesResponse {
    data: Vec<TwitchEmote>,
}

#[derive(Deserialize)]
struct TwitchEmote {
    id: String,
    name: String,
    images: TwitchEmoteImages,
    #[serde(default)]
    emote_type: String,
    #[serde(default)]
    format: Vec<String>,
}

#[derive(Deserialize, Debug)]
struct TwitchEmoteImages {
    #[serde(default)]
    url_1x: String,
    url_2x: String,
    #[serde(default)]
    url_4x: String,
}

const TWITCH_EMOTE_CDN_TEMPLATE: &str =
    "https://static-cdn.jtvnw.net/emoticons/v2/{{id}}/{{format}}/{{theme_mode}}/{{scale}}";

fn twitch_emote_variant_url(template: &str, id: &str, is_animated: bool, scale: &str) -> String {
    template
        .replace("{{id}}", id)
        .replace(
            "{{format}}",
            if is_animated { "animated" } else { "static" },
        )
        .replace("{{theme_mode}}", "dark")
        .replace("{{scale}}", scale)
}

/// Fetch Twitch channel emotes (subscriber emotes) using Helix API
/// Requires an OAuth token with user:read:emotes or channel:read:emotes scope
pub async fn fetch_twitch_channel_emotes(
    channel_id: &str,
    token: Option<&str>,
    channel_login: &str,
) -> Result<Vec<Emote>, String> {
    let Some(initial_token) = token else {
        return Ok(vec![]);
    };

    let url = api::twitch::channel_emotes(channel_id);
    let mut token_to_use = initial_token.to_string();

    let mut resp = retry_helix_get_str(
        &EMOTE_HTTP_CLIENT,
        &url,
        CLIENT_ID,
        &token_to_use,
        EMOTE_MAX_RETRY_AFTER,
    )
    .await?;

    if matches!(resp.status().as_u16(), 401 | 403) {
        crate::log_emotes!(
            warn,
            "Twitch channel emotes unauthorized for {} (status {}), attempting token refresh",
            channel_login,
            resp.status()
        );
        match crate::backend::auth::refresh_and_save_tokens().await {
            Ok(new_token) => {
                token_to_use = new_token;
                resp = retry_helix_get_str(
                    &EMOTE_HTTP_CLIENT,
                    &url,
                    CLIENT_ID,
                    &token_to_use,
                    EMOTE_MAX_RETRY_AFTER,
                )
                .await?;
            }
            Err(err) => {
                return Err(format!(
                    "token refresh failed after status {}: {}",
                    resp.status(),
                    err
                ));
            }
        }
    }

    if !resp.status().is_success() {
        return Err(format!(
            "Helix channel emotes returned status: {}",
            resp.status()
        ));
    }

    let response: TwitchEmotesResponse = resp.json().await.map_err(|e| e.to_string())?;

    let emotes: Vec<Emote> = response
        .data
        .into_iter()
        .map(|e| {
            let is_animated = e.format.iter().any(|f| f == "animated");
            let (url_1x, url_2x, url_4x) = if is_animated {
                (
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "1.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "2.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "3.0"),
                )
            } else {
                (e.images.url_1x, e.images.url_2x, e.images.url_4x)
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.name),
                url: Arc::from(url_1x.clone()),
                provider: EmoteProvider::Twitch,
                channel: Some(Arc::from(channel_login)),
                original_name: None,
                creator_name: None,
                twitch_owner_login: Some(Arc::from(channel_login)),
                twitch_owner_id: Some(Arc::from(channel_id)),
                twitch_emote_type: Some(Arc::from(if e.emote_type.is_empty() {
                    "subscriptions"
                } else {
                    e.emote_type.as_str()
                })),
                url_1x: Some(Arc::from(url_1x)),
                url_2x: Some(Arc::from(url_2x)),
                url_4x: Some(Arc::from(url_4x)),
                zero_width: false,
                is_animated,
                width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
                height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
            }
        })
        .collect();

    Ok(emotes)
}

#[derive(Deserialize, Debug)]
struct TwitchUserEmote {
    id: String,
    name: String,
    #[serde(default)]
    owner_id: String,
    #[serde(default)]
    emote_type: String,
    #[serde(default)]
    format: Vec<String>,
}

/// Fetch all emotes the user has access to (subscriptions, bits, follower emotes)
/// Fetch the user's Twitch emotes. The flag is `false` when pagination stopped
/// early after errors, so the caller can keep its previous set alongside the
/// partial result instead of replacing it.
pub async fn fetch_twitch_user_emotes(
    user_id: &str,
    token: &str,
) -> Result<(Vec<Emote>, bool), String> {
    crate::log_emotes!(debug, "Fetching user emotes for user_id: {}", user_id);
    let mut complete = true;
    let mut all_raw_emotes = Vec::new();
    let mut cursor: Option<String> = None;
    let mut template = String::new();
    let mut page_num = 0;

    let pagination_backoff = || {
        backon::ExponentialBuilder::default()
            .with_min_delay(std::time::Duration::from_millis(1000))
            .with_max_delay(std::time::Duration::from_secs(30))
            .with_max_times(7)
            .with_jitter()
    };

    let mut rate_limit_remaining: Option<u32> = None;
    let mut consecutive_errors = 0;
    let fetch_started = Instant::now();

    loop {
        if page_num > 0 {
            let adaptive_delay = if consecutive_errors > 0 {
                Some(std::time::Duration::from_millis(
                    2000 * (consecutive_errors as u64),
                ))
            } else if rate_limit_remaining.map(|r| r < 50).unwrap_or(false) {
                Some(std::time::Duration::from_millis(1000))
            } else {
                None
            };
            if let Some(delay) = adaptive_delay {
                tokio::time::sleep(delay).await;
            }
        }
        page_num += 1;

        let mut url = api::twitch::user_emotes(user_id);
        if let Some(ref c) = cursor {
            url.push_str(&format!("&after={}", urlencoding::encode(c)));
        }

        crate::log_emotes!(trace, "User emotes URL (page {}): {}", page_num, url);

        let resp = (|| {
            let url = url.clone();
            let token = token.to_string();
            async move { EMOTE_HTTP_CLIENT.get(&url).helix_auth(&token).send().await }
        })
        .retry(pagination_backoff())
        .when(is_reqwest_transient)
        .await;

        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                consecutive_errors += 1;
                crate::log_emotes!(
                    warn,
                    "Request error on page {} (attempt {}): {}",
                    page_num,
                    consecutive_errors,
                    e
                );
                if cursor.is_none() {
                    return Err(e.to_string());
                }
                if consecutive_errors < 3 {
                    crate::log_emotes!(info, "Retrying page {} after delay...", page_num);
                    page_num -= 1;
                    continue;
                }
                crate::log_emotes!(
                    warn,
                    "Pagination failed on page {} after {} errors, continuing with {} emotes from previous pages",
                    page_num,
                    consecutive_errors,
                    all_raw_emotes.len()
                );
                complete = false;
                break;
            }
        };

        if let Some(remaining) = resp.headers().get("ratelimit-remaining")
            && let Ok(val) = remaining.to_str().unwrap_or("").parse::<u32>()
        {
            rate_limit_remaining = Some(val);
            if val < 100 {
                crate::log_emotes!(debug, "Rate limit remaining: {} (page {})", val, page_num);
            }
        }

        let status = resp.status();
        crate::log_emotes!(trace, "User emotes response status: {}", status);

        if status.as_u16() == 429 {
            // Page 1 fails fast: the caller keeps the previous set.
            if cursor.is_none() {
                return Err(format!("Twitch API rate limited ({})", status));
            }
            consecutive_errors += 1;
            let wait = server_retry_wait(resp.headers()).unwrap_or(Duration::from_secs(5));
            let within_budget = wait <= USER_EMOTES_RATE_LIMIT_MAX_WAIT
                && fetch_started.elapsed() + wait <= USER_EMOTES_RATE_LIMIT_BUDGET;
            if consecutive_errors < 3 && within_budget {
                crate::log_emotes!(
                    warn,
                    "Rate limited on page {}, waiting {:?} before retry",
                    page_num,
                    wait
                );
                tokio::time::sleep(wait).await;
                page_num -= 1;
                continue;
            }
            crate::log_emotes!(
                warn,
                "Rate limited on page {} (wait {:?}), continuing with {} emotes",
                page_num,
                wait,
                all_raw_emotes.len()
            );
            complete = false;
            break;
        }

        if !status.is_success() {
            let body = crate::backend::http::error_body(resp).await;
            crate::log_emotes!(warn, "Failed to fetch user emotes: {} - {}", status, body);
            if cursor.is_none() {
                return Err(format!("Twitch API error: {} - {}", status, body));
            }
            complete = false;
            break;
        }

        consecutive_errors = 0;

        let body = match resp.text().await {
            Ok(b) => b,
            Err(e) => {
                crate::log_emotes!(warn, "Failed to read response body: {}", e);
                if cursor.is_none() {
                    return Err(e.to_string());
                }
                complete = false;
                break;
            }
        };

        if cursor.is_none() {
            crate::log_emotes!(
                trace,
                "Raw response sample: {}",
                truncate_for_log(&body, 800)
            );
        }

        #[derive(Deserialize)]
        struct PaginatedResponse {
            data: Vec<TwitchUserEmote>,
            #[serde(default)]
            template: String,
            #[serde(default)]
            pagination: Pagination,
        }
        #[derive(Deserialize, Default)]
        struct Pagination {
            cursor: Option<String>,
        }

        let response: PaginatedResponse = serde_json::from_str(&body).map_err(|e| {
            crate::log_emotes!(warn, "Failed to parse user emotes response: {}", e);
            crate::log_emotes!(debug, "Response body: {}", truncate_for_log(&body, 500));
            e.to_string()
        })?;

        crate::log_emotes!(
            trace,
            "Received {} user emotes in this page",
            response.data.len()
        );

        if cursor.is_none() && !response.data.is_empty() {
            let first = &response.data[0];
            crate::log_emotes!(
                trace,
                "First emote parsed: id={}, name={}, owner_id='{}'",
                first.id,
                first.name,
                first.owner_id
            );
        }

        if template.is_empty() && !response.template.is_empty() {
            template = response.template;
        }

        all_raw_emotes.extend(response.data);

        cursor = response.pagination.cursor;
        if cursor.is_none() {
            break;
        }
    }

    let emote_types: HashSet<&str> = all_raw_emotes
        .iter()
        .map(|e| e.emote_type.as_str())
        .collect();
    crate::log_emotes!(
        info,
        "User emotes API returned {} emotes from {} pages with types: {:?}",
        all_raw_emotes.len(),
        page_num,
        emote_types
    );

    let owner_ids: HashSet<String> = all_raw_emotes
        .iter()
        .filter(|e| {
            let is_channel_emote = matches!(
                e.emote_type.as_str(),
                "subscriptions" | "follower" | "bitstier"
            );
            is_channel_emote
                && !e.owner_id.is_empty()
                && e.owner_id != "0"
                && e.owner_id.chars().all(|c| c.is_ascii_digit())
        })
        .map(|e| e.owner_id.clone())
        .collect();

    let channel_emote_count = all_raw_emotes
        .iter()
        .filter(|e| {
            matches!(
                e.emote_type.as_str(),
                "subscriptions" | "follower" | "bitstier"
            )
        })
        .count();

    crate::log_emotes!(
        info,
        "Found {} channel-based emotes (sub/follower/bits) out of {} total, {} unique channel IDs to resolve",
        channel_emote_count,
        all_raw_emotes.len(),
        owner_ids.len()
    );

    if owner_ids.is_empty() && channel_emote_count > 0 {
        let sample: Vec<_> = all_raw_emotes
            .iter()
            .filter(|e| {
                matches!(
                    e.emote_type.as_str(),
                    "subscriptions" | "follower" | "bitstier"
                )
            })
            .take(3)
            .collect();
        for e in sample {
            crate::log_emotes!(
                debug,
                "Sample channel emote: name={}, owner_id='{}', emote_type={}",
                e.name,
                e.owner_id,
                e.emote_type
            );
        }
    }

    let owner_names = resolve_user_ids_to_names(&owner_ids, token).await;

    let page_template = if template.is_empty() {
        TWITCH_EMOTE_CDN_TEMPLATE
    } else {
        &template
    };

    let mut all_emotes = Vec::new();
    for e in all_raw_emotes {
        let is_animated = e.format.iter().any(|format| format == "animated");
        let url_1x = twitch_emote_variant_url(page_template, &e.id, is_animated, "1.0");
        let url_2x = twitch_emote_variant_url(page_template, &e.id, is_animated, "2.0");
        let url_4x = twitch_emote_variant_url(page_template, &e.id, is_animated, "3.0");

        let channel_value = match e.emote_type.as_str() {
            "subscriptions" | "follower" | "bitstier" => {
                let channel_name = if e.owner_id.is_empty() {
                    crate::log_emotes!(
                        debug,
                        "Keeping emote '{}' with missing Twitch owner id",
                        e.name
                    );
                    "unresolved".to_string()
                } else if e.owner_id.chars().all(|c| c.is_ascii_digit()) {
                    owner_names.get(&e.owner_id).cloned().unwrap_or_else(|| {
                        crate::log_emotes!(
                            debug,
                            "Keeping emote '{}' with unresolved Twitch owner id '{}'",
                            e.name,
                            e.owner_id
                        );
                        format!("unresolved:{}", e.owner_id)
                    })
                } else {
                    e.owner_id.clone()
                };

                format!(
                    "{}{}",
                    echo_emote_core::SUBSCRIPTION_CHANNEL_PREFIX,
                    channel_name
                )
            }
            "prime" => "twitch:prime".to_string(),
            "limitedtime" => "twitch:limited".to_string(),
            "hypetrain" => "twitch:hypetrain".to_string(),
            "rewards" => "twitch:rewards".to_string(),
            "turbo" => "twitch:turbo".to_string(),
            _ => continue,
        };

        all_emotes.push(Emote {
            id: Arc::from(e.id.clone()),
            name: Arc::from(e.name),
            url: Arc::from(url_1x.clone()),
            provider: EmoteProvider::Twitch,
            channel: Some(Arc::from(channel_value)),
            original_name: None,
            creator_name: None,
            twitch_owner_login: match e.emote_type.as_str() {
                "subscriptions" | "follower" | "bitstier" => {
                    if e.owner_id.chars().all(|c| c.is_ascii_digit()) {
                        owner_names.get(&e.owner_id).cloned().map(Arc::from)
                    } else if e.owner_id.is_empty() {
                        None
                    } else {
                        Some(Arc::from(e.owner_id.clone()))
                    }
                }
                _ => None,
            },
            twitch_owner_id: if e.owner_id.is_empty() {
                None
            } else {
                Some(Arc::from(e.owner_id.clone()))
            },
            twitch_emote_type: if e.emote_type.is_empty() {
                None
            } else {
                Some(Arc::from(e.emote_type.clone()))
            },
            url_1x: Some(Arc::from(url_1x)),
            url_2x: Some(Arc::from(url_2x)),
            url_4x: Some(Arc::from(url_4x)),
            zero_width: false,
            is_animated,
            width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
            height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
        });
    }

    crate::log_emotes!(
        info,
        "Total user (subscription) emotes after filtering: {}",
        all_emotes.len()
    );
    Ok((all_emotes, complete))
}

/// Resolve Twitch user IDs to display names
async fn resolve_user_ids_to_names(ids: &HashSet<String>, token: &str) -> HashMap<String, String> {
    let mut result = HashMap::new();

    if ids.is_empty() {
        return result;
    }

    let ids_vec: Vec<&String> = ids.iter().collect();
    for chunk in ids_vec.chunks(100) {
        let id_params: Vec<String> = chunk.iter().map(|id| format!("id={}", id)).collect();
        let url = api::twitch::users(&id_params);

        crate::log_emotes!(
            trace,
            "Resolving {} user IDs, URL: {}",
            chunk.len(),
            truncate_for_log(&url, 200)
        );

        let resp = retry_helix_get(
            &EMOTE_HTTP_CLIENT,
            &url,
            CLIENT_ID,
            token,
            EMOTE_MAX_RETRY_AFTER,
        )
        .await;

        match resp {
            Ok(resp) => {
                let status = resp.status();
                if status.is_success() {
                    #[derive(Deserialize)]
                    struct UsersResponse {
                        data: Vec<UserInfo>,
                    }
                    #[derive(Deserialize)]
                    struct UserInfo {
                        id: String,
                        display_name: String,
                    }

                    match resp.json::<UsersResponse>().await {
                        Ok(users_resp) => {
                            crate::log_emotes!(
                                info,
                                "Got {} users from API",
                                users_resp.data.len()
                            );
                            for user in users_resp.data {
                                result.insert(user.id, user.display_name);
                            }
                        }
                        Err(e) => {
                            crate::log_emotes!(warn, "Failed to parse users response: {}", e);
                        }
                    }
                } else {
                    let body = crate::backend::http::error_body(resp).await;
                    crate::log_emotes!(
                        warn,
                        "Users API failed: {} - {}",
                        status,
                        truncate_for_log(&body, 200)
                    );
                }
            }
            Err(e) => {
                crate::log_emotes!(warn, "Failed to fetch users: {}", e);
            }
        }
    }

    crate::log_emotes!(info, "Resolved {} user IDs to names", result.len());
    result
}

/// Fetch Twitch global emotes using Helix API
pub async fn fetch_twitch_global_emotes(token: Option<&str>) -> Result<Vec<Emote>, String> {
    let Some(token) = token else {
        return Ok(vec![]);
    };

    let resp = retry_helix_get_str(
        &EMOTE_HTTP_CLIENT,
        api::twitch::GLOBAL_EMOTES,
        CLIENT_ID,
        token,
        EMOTE_MAX_RETRY_AFTER,
    )
    .await?;

    if !resp.status().is_success() {
        return Err(format!(
            "Helix global emotes returned status: {}",
            resp.status()
        ));
    }

    let response: TwitchEmotesResponse = resp.json().await.map_err(|e| e.to_string())?;

    let emotes: Vec<Emote> = response
        .data
        .into_iter()
        .map(|e| {
            let is_animated = e.format.iter().any(|f| f == "animated");
            let (url_1x, url_2x, url_4x) = if is_animated {
                (
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "1.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "2.0"),
                    twitch_emote_variant_url(TWITCH_EMOTE_CDN_TEMPLATE, &e.id, true, "3.0"),
                )
            } else {
                (e.images.url_1x, e.images.url_2x, e.images.url_4x)
            };
            Emote {
                id: Arc::from(e.id.clone()),
                name: Arc::from(e.name),
                url: Arc::from(url_1x.clone()),
                provider: EmoteProvider::Twitch,
                channel: None,
                original_name: None,
                creator_name: None,
                twitch_owner_login: None,
                twitch_owner_id: None,
                twitch_emote_type: if e.emote_type.is_empty() {
                    None
                } else {
                    Some(Arc::from(e.emote_type))
                },
                url_1x: Some(Arc::from(url_1x)),
                url_2x: Some(Arc::from(url_2x)),
                url_4x: Some(Arc::from(url_4x)),
                zero_width: false,
                is_animated,
                width: Some(echo_emote_core::twitch_base_dimensions(&e.id).0),
                height: Some(echo_emote_core::twitch_base_dimensions(&e.id).1),
            }
        })
        .collect();

    Ok(emotes)
}
