//! Download and decode for the image preview window.
//!
//! The window shows one image at its full resolution, which the chat image
//! pipeline never keeps (its stills stop at 768 px). This path is separate
//! from it: nothing here enters the chat's memory, disk or URI caches or is
//! charged to their budgets. The window owns the one decoded image and
//! releases it when it closes or shows another.
//!
//! Every request goes through the link-preview fetch policy: public addresses
//! only, after DNS and on every redirect. Loads run one at a time, and a load
//! whose window no longer wants it stops before its next request or decode.

use echo_media_cache::full_resolution::{FullResolutionError, FullResolutionImage};
use std::sync::LazyLock;
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

/// Largest response body downloaded for the window (compressed size).
const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;
/// Whole-request deadline; large originals take longer than a thumbnail.
const TOTAL_TIMEOUT: Duration = Duration::from_secs(60);

static HTTP_CLIENT: LazyLock<crate::backend::http::ClientCache> =
    LazyLock::new(|| crate::backend::http::ClientCache::new(build_http_client));

/// One load at a time: a superseded load waits here and then gives up.
static LOAD_SLOT: Semaphore = Semaphore::const_new(1);

fn build_http_client() -> reqwest::Client {
    super::loader::media_http_client_builder()
        .timeout(TOTAL_TIMEOUT)
        .redirect(crate::backend::embeds::fetchers::embed_redirect_policy())
        .dns_resolver(crate::backend::embeds::fetchers::PublicOnlyResolver)
        .build()
        .unwrap_or_else(|error| {
            crate::log_cache!(
                error,
                "Failed to create the image preview HTTP client: {error}"
            );
            crate::backend::embeds::fetchers::blocked_embed_client()
        })
}

/// A decoded preview image and which candidate URL produced it.
pub struct InspectorImage {
    pub decoded: FullResolutionImage,
    /// Index into the candidate list passed to [`load`].
    pub candidate_index: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InspectorLoadError {
    /// Not a public http(s) address.
    Blocked,
    /// The server refused, the connection failed, or it timed out.
    Network,
    /// The download is over [`MAX_BODY_BYTES`].
    TooLarge,
    Decode(FullResolutionError),
}

/// Fetch and decode the first candidate that works, best first. The error is
/// the last candidate's, which is the URL the card itself advertised.
pub fn load(
    candidates: Vec<String>,
) -> oneshot::Receiver<Result<InspectorImage, InspectorLoadError>> {
    let (sender, receiver) = oneshot::channel();
    crate::backend::bridge::get_runtime().spawn(async move {
        let Ok(_slot) = LOAD_SLOT.acquire().await else {
            return;
        };
        let mut result = Err(InspectorLoadError::Network);
        for (candidate_index, url) in candidates.iter().enumerate() {
            if sender.is_closed() {
                return;
            }
            result = load_one(url, &sender).await.map(|decoded| InspectorImage {
                decoded,
                candidate_index,
            });
            if result.is_ok() {
                break;
            }
        }
        let _ = sender.send(result);
    });
    receiver
}

async fn load_one<T>(
    url: &str,
    sender: &oneshot::Sender<T>,
) -> Result<FullResolutionImage, InspectorLoadError> {
    let host = url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_default();
    if !crate::backend::embeds::fetchers::is_public_fetch_url(url) {
        return Err(InspectorLoadError::Blocked);
    }
    let bytes = fetch(url).await.inspect_err(|error| {
        crate::log_cache!(debug, host = %host, ?error, "Image preview download failed");
    })?;
    if sender.is_closed() {
        return Err(InspectorLoadError::Network);
    }
    let decoded = tokio::task::spawn_blocking(move || {
        echo_media_cache::full_resolution::decode_full_resolution(&bytes)
    })
    .await
    .map_err(|_| InspectorLoadError::Decode(FullResolutionError::Corrupt))?
    .map_err(InspectorLoadError::Decode);
    match &decoded {
        Ok(image) => crate::log_cache!(
            debug,
            host = %host,
            source_width = image.source_size.0,
            source_height = image.source_size.1,
            decoded_width = image.shown_size.0,
            decoded_height = image.shown_size.1,
            frames = image.frame_count,
            "Image preview decoded"
        ),
        Err(error) => crate::log_cache!(debug, host = %host, ?error, "Image preview decode failed"),
    }
    decoded
}

async fn fetch(url: &str) -> Result<Vec<u8>, InspectorLoadError> {
    let mut response = HTTP_CLIENT
        .get(url)
        .send()
        .await
        .map_err(|_| InspectorLoadError::Network)?;
    if !response.status().is_success() {
        return Err(InspectorLoadError::Network);
    }
    if response
        .content_length()
        .is_some_and(|length| length > MAX_BODY_BYTES as u64)
    {
        return Err(InspectorLoadError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| InspectorLoadError::Network)?
    {
        if chunk.len() > MAX_BODY_BYTES - body.len() {
            return Err(InspectorLoadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
