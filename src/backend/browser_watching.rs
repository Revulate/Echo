//! Chatterino-compatible browser Watching channel integration.
//!
//! The official Chatterino extension launches a native-messaging host named
//! `com.chatterino.chatterino` and sends `select` messages for the focused
//! Twitch channel. Echo registers a transparent proxy for that host name on
//! Windows: every native frame still reaches Chatterino's original host while
//! Echo observes Twitch selections over a local named pipe. When no extension
//! selection has been received, Windows global media-session metadata provides
//! a best-effort fallback for a user who explicitly added a Watching split.

use crate::backend::lock_utils::RwLockExt;
#[cfg(target_os = "windows")]
use serde::Deserialize;
#[cfg(target_os = "windows")]
use std::io::{self, Read, Write};
use std::sync::{LazyLock, RwLock};

#[cfg(target_os = "windows")]
use std::sync::Mutex;

#[cfg(target_os = "windows")]
use std::future::IntoFuture;
#[cfg(target_os = "windows")]
use std::path::{Path, PathBuf};
#[cfg(target_os = "windows")]
use std::process::{Child, ChildStdin, Command, Stdio};
#[cfg(target_os = "windows")]
use std::thread::JoinHandle;
#[cfg(target_os = "windows")]
use std::time::{Duration, Instant};

/// Persisted split-layout sentinel. It cannot collide with a Twitch login.
pub const WATCHING_CHANNEL: &str = "__echo_watching__";

#[cfg(target_os = "windows")]
const CHROME_EXTENSION_ID: &str = "glknmaideaikkmemifbfkhnomoknepka";
#[cfg(target_os = "windows")]
const FIREFOX_EXTENSION_ID: &str = "chatterino_native@chatterino.com";
#[cfg(target_os = "windows")]
const NATIVE_HOST_NAME: &str = "com.chatterino.chatterino";
#[cfg(target_os = "windows")]
const MAX_NATIVE_MESSAGE_BYTES: usize = 64 * 1024;

/// The GUI owns this mutex for its complete process lifetime. Browser-host
/// helpers only observe it; they never participate in the single-instance
/// decision.
#[cfg(target_os = "windows")]
pub const GUI_INSTANCE_MUTEX_NAME: &str = "Echo-Twitch-Chat-Client-Single-Instance";

/// Safety-net poll cadence for the media-session monitor. Real changes arrive
/// through the WinRT change handlers; the heartbeat only catches events those
/// handlers might miss, so it does not need to be tight.
#[cfg(target_os = "windows")]
const MEDIA_SESSION_HEARTBEAT: Duration = Duration::from_secs(15);
#[cfg(target_os = "windows")]
const MEDIA_SESSION_DEBOUNCE: Duration = Duration::from_millis(100);
#[cfg(target_os = "windows")]
const MEDIA_SESSION_STALE_GRACE: Duration = Duration::from_secs(5);
/// Self-heal cadence for the registry registration. Registration is asserted
/// synchronously at startup; the heartbeat only has to restore it after a
/// browser or Chatterino reinstall re-points the keys, so 30s of drift is
/// acceptable in exchange for not hammering the registry every 2s forever.
#[cfg(target_os = "windows")]
const NATIVE_HOST_REGISTRATION_HEARTBEAT: Duration = Duration::from_secs(30);

#[cfg(target_os = "windows")]
const WATCHING_PIPE_NAME: &str = r"\\.\pipe\Echo-Browser-Watching-v1";

#[cfg(target_os = "windows")]
#[derive(Debug, Default, serde::Serialize, Deserialize)]
struct NativeHostBridgeConfig {
    upstream_host: Option<PathBuf>,
}

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
struct NativeHostManifest {
    name: String,
    path: PathBuf,
}

#[cfg(target_os = "windows")]
struct UpstreamNativeHost {
    child: Child,
    input: Option<ChildStdin>,
    output_relay: Option<JoinHandle<io::Result<u64>>>,
}

#[cfg(target_os = "windows")]
impl UpstreamNativeHost {
    fn forward(&mut self, length: [u8; 4], payload: &[u8]) -> io::Result<()> {
        let Some(input) = self.input.as_mut() else {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Chatterino native-host input is closed",
            ));
        };
        input.write_all(&length)?;
        input.write_all(payload)?;
        input.flush()
    }

    fn finish(mut self) {
        drop(self.input.take());
        let _ = self.child.wait();
        if let Some(relay) = self.output_relay.take() {
            let _ = relay.join();
        }
    }
}

static INTEGRATION_ERROR: LazyLock<RwLock<Option<String>>> = LazyLock::new(|| RwLock::new(None));

#[cfg(target_os = "windows")]
static REGISTRATION_LOCK: LazyLock<Mutex<()>> = LazyLock::new(|| Mutex::new(()));

#[cfg(target_os = "windows")]
static REGISTRATION_STOPPED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// The authority that selected the concrete channel behind a Watching split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchingChannelSource {
    Extension,
    MediaSession,
}

impl WatchingChannelSource {
    pub fn label(self) -> &'static str {
        match self {
            Self::Extension => "browser extension",
            Self::MediaSession => "active media",
        }
    }
}

/// User-facing state of the best-effort Windows active-media fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaSessionStatus {
    Starting,
    Waiting,
    Active {
        channel: String,
        source_app: String,
        paused: bool,
    },
    Ambiguous {
        candidate_count: usize,
    },
    Unavailable,
    Unsupported,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchingStatus {
    pub channel: Option<String>,
    pub source: Option<WatchingChannelSource>,
    pub media_session: MediaSessionStatus,
}

#[derive(Debug, Clone)]
struct WatchingState {
    channel: Option<String>,
    source: Option<WatchingChannelSource>,
    extension_authoritative: bool,
    media_session: MediaSessionStatus,
}

impl Default for WatchingState {
    fn default() -> Self {
        Self {
            channel: None,
            source: None,
            extension_authoritative: false,
            media_session: if cfg!(target_os = "windows") {
                MediaSessionStatus::Starting
            } else {
                MediaSessionStatus::Unsupported
            },
        }
    }
}

#[cfg(target_os = "windows")]
impl WatchingState {
    fn apply_media_session(
        &mut self,
        status: MediaSessionStatus,
        update: MediaChannelUpdate,
    ) -> Option<(bool, Option<String>)> {
        let status_changed = self.media_session != status;
        self.media_session = status;
        if self.extension_authoritative {
            return None;
        }

        let previous_channel = self.channel.clone();
        let previous_source = self.source;
        match update {
            MediaChannelUpdate::Keep => {}
            MediaChannelUpdate::Set(channel) => {
                self.channel = Some(channel);
                self.source = Some(WatchingChannelSource::MediaSession);
            }
            MediaChannelUpdate::Clear => {
                if self.source == Some(WatchingChannelSource::MediaSession) {
                    self.channel = None;
                    self.source = None;
                }
            }
        }
        let channel_changed = previous_channel != self.channel || previous_source != self.source;
        Some((status_changed || channel_changed, self.channel.clone()))
    }

    fn select_extension_channel(&mut self, channel: &str) -> (bool, bool) {
        let channel_changed = self.channel.as_deref() != Some(channel);
        let source_changed = self.source != Some(WatchingChannelSource::Extension);
        self.extension_authoritative = true;
        self.channel = Some(channel.to_string());
        self.source = Some(WatchingChannelSource::Extension);
        (channel_changed || source_changed, channel_changed)
    }
}

static WATCHING_STATE: LazyLock<RwLock<WatchingState>> =
    LazyLock::new(|| RwLock::new(WatchingState::default()));

/// Opaque restore token for the no-persistence UI acceptance fixture.
#[doc(hidden)]
pub struct WatchingBenchmarkRestore(WatchingState);

#[cfg(target_os = "windows")]
#[derive(Debug, Deserialize)]
struct NativeMessage {
    action: String,
    #[serde(default, rename = "type")]
    channel_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
}

/// Whether a persisted layout leaf is the indirect Watching channel.
pub fn is_watching_channel(channel: &str) -> bool {
    channel == WATCHING_CHANNEL
}

/// The normalized Twitch login currently selected by the extension or active-media fallback.
pub fn current_channel() -> Option<String> {
    WATCHING_STATE.read_or_recover().channel.clone()
}

/// Temporarily provide a deterministic concrete channel to a disposable
/// Watching split. Ordinary sessions cannot enter this path.
#[doc(hidden)]
pub fn set_current_channel_for_benchmark(channel: &str) -> Option<WatchingBenchmarkRestore> {
    if !crate::benchmark_ui_fixture::is_enabled() {
        return None;
    }
    let mut state = WATCHING_STATE.write_or_recover();
    let restore = WatchingBenchmarkRestore(state.clone());
    state.channel = Some(channel.to_string());
    state.source = Some(WatchingChannelSource::Extension);
    state.extension_authoritative = true;
    Some(restore)
}

/// Restore the Watching state captured by `set_current_channel_for_benchmark`.
#[doc(hidden)]
pub fn restore_current_channel_after_benchmark(restore: WatchingBenchmarkRestore) -> bool {
    if !crate::benchmark_ui_fixture::is_enabled() {
        return false;
    }
    let mut state = WATCHING_STATE.write_or_recover();
    *state = restore.0;
    true
}

pub fn current_source() -> Option<WatchingChannelSource> {
    WATCHING_STATE.read_or_recover().source
}

pub fn status() -> WatchingStatus {
    let state = WATCHING_STATE.read_or_recover();
    WatchingStatus {
        channel: state.channel.clone(),
        source: state.source,
        media_session: state.media_session.clone(),
    }
}

/// Resolve a persisted layout channel to the concrete Twitch channel used by chat.
pub fn resolve_layout_channel(channel: &str) -> Option<String> {
    if is_watching_channel(channel) {
        current_channel()
    } else {
        Some(channel.to_string())
    }
}

/// A concise registration failure for the empty Watching pane, if setup failed.
pub fn integration_error() -> Option<String> {
    INTEGRATION_ERROR.read_or_recover().clone()
}

/// Register the browser native-host proxy and start the local GUI receiver.
pub fn initialize() {
    #[cfg(target_os = "windows")]
    {
        if crate::benchmark_profile::is_enabled() {
            crate::log_ui!(
                info,
                "Browser Watching registry integration and pipe are disabled in benchmark profiles"
            );
            *INTEGRATION_ERROR.write_or_recover() = None;
        } else {
            REGISTRATION_STOPPED.store(false, std::sync::atomic::Ordering::Release);
            if let Err(error) = ensure_native_messaging_proxy_registration() {
                crate::log_ui!(warn, "Could not register browser Watching proxy: {error}");
                *INTEGRATION_ERROR.write_or_recover() = Some(error);
            } else {
                *INTEGRATION_ERROR.write_or_recover() = None;
            }
            start_native_host_registration_monitor();
            start_pipe_server();
        }

        start_media_session_monitor();
    }
}

/// Stop asserting Echo's browser proxy and hand native-messaging registration
/// back to Chatterino before the GUI process disappears.
pub fn shutdown() {
    #[cfg(target_os = "windows")]
    {
        REGISTRATION_STOPPED.store(true, std::sync::atomic::Ordering::Release);
        if crate::benchmark_profile::is_enabled() {
            return;
        }
        if let Err(error) = restore_upstream_native_messaging_registration() {
            crate::log_ui!(
                warn,
                "Could not restore browser Watching registration: {error}"
            );
        }
    }
}

/// Chrome passes the extension origin as the first native-host argument.
pub fn is_native_messaging_invocation(args: &[String]) -> bool {
    #[cfg(target_os = "windows")]
    {
        args.iter().any(|argument| {
            argument
                .strip_prefix("chrome-extension://")
                .is_some_and(|origin| origin.starts_with(CHROME_EXTENSION_ID))
                || argument.starts_with("moz-extension://")
                || argument == FIREFOX_EXTENSION_ID
        })
    }

    #[cfg(not(target_os = "windows"))]
    {
        let _ = args;
        false
    }
}

/// Read Chrome/Firefox native-messaging frames and forward Twitch selections.
pub fn run_native_messaging_host() -> Result<(), String> {
    #[cfg(not(target_os = "windows"))]
    {
        Err("Browser Watching integration is currently available on Windows only".into())
    }

    #[cfg(target_os = "windows")]
    {
        let native_arguments = std::env::args().skip(1).collect::<Vec<_>>();
        let gui_is_running = start_gui_lifetime_monitor();
        if !gui_is_running {
            let _ = restore_upstream_native_messaging_registration();
        }
        let mut upstream = spawn_upstream_native_host(&native_arguments);
        let stdin = io::stdin();
        let mut input = stdin.lock();

        loop {
            let mut length_bytes = [0u8; 4];
            match input.read_exact(&mut length_bytes) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(error) => return Err(format!("Could not read native message length: {error}")),
            }

            let length = u32::from_le_bytes(length_bytes) as usize;
            if length == 0 || length > MAX_NATIVE_MESSAGE_BYTES {
                return Err(format!(
                    "Native message length {length} is outside the accepted range"
                ));
            }

            let mut payload = vec![0u8; length];
            input
                .read_exact(&mut payload)
                .map_err(|error| format!("Could not read native message: {error}"))?;

            if let Some(upstream) = upstream.as_mut()
                && let Err(error) = upstream.forward(length_bytes, &payload)
            {
                eprintln!("Echo could not forward a native message to Chatterino: {error}");
            }

            if let Ok(message) = serde_json::from_slice::<NativeMessage>(&payload)
                && message.action == "select"
                && message.channel_type.as_deref() == Some("twitch")
                && let Some(channel) = message.name.as_deref().and_then(normalize_channel_login)
                && let Err(error) = send_channel_to_gui(&channel)
            {
                eprintln!("Echo Watching receiver is unavailable: {error}");
            }

            if !gui_is_running {
                break;
            }
        }

        if let Some(upstream) = upstream {
            upstream.finish();
        }
        Ok(())
    }
}

#[cfg(target_os = "windows")]
fn start_gui_lifetime_monitor() -> bool {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{CloseHandle, HANDLE};
    use windows::Win32::System::Threading::{
        INFINITE, OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE, WaitForSingleObject,
    };
    use windows::core::PCWSTR;

    let mutex_name = std::ffi::OsStr::new(GUI_INSTANCE_MUTEX_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: `mutex_name` is a NUL-terminated UTF-16 buffer that outlives the call.
    let Ok(gui_mutex) = (unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            PCWSTR(mutex_name.as_ptr()),
        )
    }) else {
        return false;
    };

    let gui_mutex_value = gui_mutex.0 as usize;
    let spawned = std::thread::Builder::new()
        .name("echo-gui-lifetime".to_string())
        .spawn(move || {
            let gui_mutex = HANDLE(gui_mutex_value as *mut std::ffi::c_void);
            // SAFETY: `gui_mutex` is the handle opened above and moved into this thread, which waits on it and closes it exactly once.
            let _ = unsafe { WaitForSingleObject(gui_mutex, INFINITE) };
            let _ = unsafe { CloseHandle(gui_mutex) };
            let _ = restore_upstream_native_messaging_registration();
            std::process::exit(0);
        });
    if spawned.is_err() {
        // SAFETY: the thread that would have owned `gui_mutex` never started, so this is its only close.
        let _ = unsafe { CloseHandle(gui_mutex) };
        return false;
    }
    true
}

#[cfg(target_os = "windows")]
fn normalize_channel_login(channel: &str) -> Option<String> {
    let channel = channel.trim().trim_start_matches('#').to_ascii_lowercase();
    if channel.is_empty()
        || channel.len() > 25
        || !channel
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
    {
        return None;
    }
    Some(channel)
}

#[cfg(target_os = "windows")]
fn send_channel_to_gui(channel: &str) -> Result<(), String> {
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_PIPE_BUSY};

    const PIPE_BUSY_ATTEMPTS: usize = 5;

    let mut last_error = None;
    for _ in 0..PIPE_BUSY_ATTEMPTS {
        match std::fs::OpenOptions::new()
            .write(true)
            .open(WATCHING_PIPE_NAME)
        {
            Ok(mut pipe) => {
                pipe.write_all(channel.as_bytes())
                    .and_then(|_| pipe.flush())
                    .map_err(|error| format!("Could not forward the watched channel: {error}"))?;
                return Ok(());
            }
            Err(error) => {
                let code = error.raw_os_error().map(|raw| raw as u32);
                let pipe_absent = code.is_some_and(|code| {
                    code == ERROR_FILE_NOT_FOUND.0 || code == ERROR_PATH_NOT_FOUND.0
                });
                let pipe_busy = code.is_some_and(|code| code == ERROR_PIPE_BUSY.0);
                last_error = Some(error);
                if pipe_absent || !pipe_busy {
                    break;
                }
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }

    Err(format!(
        "Could not reach the running Echo instance: {}",
        last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "named pipe unavailable".to_string())
    ))
}

#[cfg(target_os = "windows")]
fn spawn_upstream_native_host(arguments: &[String]) -> Option<UpstreamNativeHost> {
    let config = load_native_host_bridge_config().ok()?;
    let upstream = config.upstream_host?;
    let current = std::env::current_exe().ok()?;
    if !valid_upstream_executable(&upstream, &current) {
        return None;
    }

    let mut child = match Command::new(&upstream)
        .args(arguments)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            eprintln!(
                "Echo could not start Chatterino's native host {}: {error}",
                upstream.display()
            );
            return None;
        }
    };

    let input = child.stdin.take()?;
    let mut output = child.stdout.take()?;
    let output_relay = std::thread::Builder::new()
        .name("chatterino-native-output".to_string())
        .spawn(move || {
            let stdout = io::stdout();
            let mut browser_output = stdout.lock();
            let copied = io::copy(&mut output, &mut browser_output)?;
            browser_output.flush()?;
            Ok(copied)
        })
        .ok()?;

    Some(UpstreamNativeHost {
        child,
        input: Some(input),
        output_relay: Some(output_relay),
    })
}

#[cfg(target_os = "windows")]
fn start_native_host_registration_monitor() {
    let spawn = std::thread::Builder::new()
        .name("native-host-registration".to_string())
        .spawn(|| {
            loop {
                std::thread::sleep(NATIVE_HOST_REGISTRATION_HEARTBEAT);
                if REGISTRATION_STOPPED.load(std::sync::atomic::Ordering::Acquire) {
                    break;
                }
                if let Err(error) = ensure_native_messaging_proxy_registration() {
                    crate::log_ui!(warn, "Could not refresh browser Watching proxy: {error}");
                }
            }
        });
    if let Err(error) = spawn {
        crate::log_ui!(
            warn,
            "Could not start browser Watching registration monitor: {error}"
        );
    }
}

#[cfg(target_os = "windows")]
fn start_pipe_server() {
    let spawn = std::thread::Builder::new()
        .name("browser-watching".to_string())
        .spawn(pipe_server_loop);
    if let Err(error) = spawn {
        let detail = format!("Could not start browser Watching receiver: {error}");
        crate::log_ui!(warn, "{detail}");
        *INTEGRATION_ERROR.write_or_recover() = Some(detail);
    }
}

#[cfg(target_os = "windows")]
fn start_media_session_monitor() {
    let spawn_result = std::thread::Builder::new()
        .name("echo-media-session-watching".to_string())
        .spawn(|| {
            use windows::Win32::System::WinRT::{
                RO_INIT_MULTITHREADED, RoInitialize, RoUninitialize,
            };

            // SAFETY: initializes WinRT for this dedicated thread; balanced by RoUninitialize on every exit path below.
            if let Err(error) = unsafe { RoInitialize(RO_INIT_MULTITHREADED) } {
                crate::log_ui!(
                    warn,
                    "Could not initialize the active-media Watching thread: {error}"
                );
                update_media_session_state(
                    MediaSessionStatus::Unavailable,
                    MediaChannelUpdate::Clear,
                );
                return;
            }

            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    crate::log_ui!(
                        warn,
                        "Could not start the active-media Watching runtime: {error}"
                    );
                    update_media_session_state(
                        MediaSessionStatus::Unavailable,
                        MediaChannelUpdate::Clear,
                    );
                    // SAFETY: balances the successful RoInitialize on this thread.
                    unsafe { RoUninitialize() };
                    return;
                }
            };

            if let Err(error) = runtime.block_on(media_session_monitor_loop()) {
                crate::log_ui!(
                    warn,
                    "Active-media Watching fallback is unavailable: {error}"
                );
                update_media_session_state(
                    MediaSessionStatus::Unavailable,
                    MediaChannelUpdate::Clear,
                );
            }
            // SAFETY: balances the successful RoInitialize on this thread.
            unsafe { RoUninitialize() };
        });

    if let Err(error) = spawn_result {
        crate::log_ui!(
            warn,
            "Could not create the active-media Watching thread: {error}"
        );
        update_media_session_state(MediaSessionStatus::Unavailable, MediaChannelUpdate::Clear);
    }
}

#[cfg(target_os = "windows")]
#[derive(Debug, Clone)]
struct MediaCandidate {
    channel: String,
    source_app: String,
    paused: bool,
}

#[cfg(target_os = "windows")]
enum MediaObservation {
    Selected(MediaCandidate),
    Ambiguous(usize),
    Waiting,
}

#[cfg(target_os = "windows")]
enum MediaChannelUpdate {
    Keep,
    Set(String),
    Clear,
}

#[cfg(target_os = "windows")]
struct CurrentSessionSubscriptions {
    session: windows::Media::Control::GlobalSystemMediaTransportControlsSession,
    media_properties_token: Option<i64>,
    playback_info_token: Option<i64>,
}

#[cfg(target_os = "windows")]
impl CurrentSessionSubscriptions {
    fn remove(self) {
        if let Some(token) = self.media_properties_token {
            let _ = self.session.RemoveMediaPropertiesChanged(token);
        }
        if let Some(token) = self.playback_info_token {
            let _ = self.session.RemovePlaybackInfoChanged(token);
        }
    }
}

#[cfg(target_os = "windows")]
async fn media_session_monitor_loop() -> windows::core::Result<()> {
    use windows::Foundation::TypedEventHandler;
    use windows::Media::Control::{
        CurrentSessionChangedEventArgs, GlobalSystemMediaTransportControlsSessionManager,
        SessionsChangedEventArgs,
    };

    let manager = GlobalSystemMediaTransportControlsSessionManager::RequestAsync()?
        .into_future()
        .await?;
    let (signal_tx, mut signal_rx) = tokio::sync::mpsc::channel::<()>(1);

    let current_signal_tx = signal_tx.clone();
    let current_handler = TypedEventHandler::<
        GlobalSystemMediaTransportControlsSessionManager,
        CurrentSessionChangedEventArgs,
    >::new(move |_sender, _args| {
        let _ = current_signal_tx.try_send(());
        Ok(())
    });
    let current_token = manager.CurrentSessionChanged(&current_handler)?;

    let sessions_signal_tx = signal_tx.clone();
    let sessions_handler = TypedEventHandler::<
        GlobalSystemMediaTransportControlsSessionManager,
        SessionsChangedEventArgs,
    >::new(move |_sender, _args| {
        let _ = sessions_signal_tx.try_send(());
        Ok(())
    });
    let sessions_token = manager.SessionsChanged(&sessions_handler)?;

    let _manager_handlers = (
        current_handler,
        sessions_handler,
        current_token,
        sessions_token,
    );
    let mut current_session_subscriptions: Option<CurrentSessionSubscriptions> = None;
    let mut missing_since = None;
    let mut resubscribe = true;

    loop {
        if resubscribe {
            if let Some(subscriptions) = current_session_subscriptions.take() {
                subscriptions.remove();
            }
            current_session_subscriptions =
                subscribe_to_current_session(&manager, signal_tx.clone());
        }

        let previous_media_channel = {
            let state = WATCHING_STATE.read_or_recover();
            (state.source == Some(WatchingChannelSource::MediaSession))
                .then(|| state.channel.clone())
                .flatten()
        };
        match observe_media_sessions(&manager, previous_media_channel.as_deref()).await {
            Ok(MediaObservation::Selected(candidate)) => {
                missing_since = None;
                update_media_session_state(
                    MediaSessionStatus::Active {
                        channel: candidate.channel.clone(),
                        source_app: candidate.source_app,
                        paused: candidate.paused,
                    },
                    MediaChannelUpdate::Set(candidate.channel),
                );
            }
            Ok(MediaObservation::Ambiguous(candidate_count)) => {
                apply_missing_media_observation(
                    MediaSessionStatus::Ambiguous { candidate_count },
                    &mut missing_since,
                );
            }
            Ok(MediaObservation::Waiting) => {
                apply_missing_media_observation(MediaSessionStatus::Waiting, &mut missing_since);
            }
            Err(error) => {
                crate::log_ui!(debug, "Could not refresh active media metadata: {error}");
                apply_missing_media_observation(
                    MediaSessionStatus::Unavailable,
                    &mut missing_since,
                );
            }
        }

        resubscribe = tokio::select! {
            signal = signal_rx.recv() => {
                if signal.is_none() {
                    break;
                }
                tokio::time::sleep(MEDIA_SESSION_DEBOUNCE).await;
                while signal_rx.try_recv().is_ok() {}
                true
            }
            _ = tokio::time::sleep(MEDIA_SESSION_HEARTBEAT) => false,
        };
    }

    Ok(())
}

#[cfg(target_os = "windows")]
fn subscribe_to_current_session(
    manager: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager,
    signal_tx: tokio::sync::mpsc::Sender<()>,
) -> Option<CurrentSessionSubscriptions> {
    use windows::Foundation::TypedEventHandler;
    use windows::Media::Control::{
        GlobalSystemMediaTransportControlsSession, MediaPropertiesChangedEventArgs,
        PlaybackInfoChangedEventArgs,
    };

    let session = manager.GetCurrentSession().ok()?;
    let media_signal_tx = signal_tx.clone();
    let media_handler = TypedEventHandler::<
        GlobalSystemMediaTransportControlsSession,
        MediaPropertiesChangedEventArgs,
    >::new(move |_sender, _args| {
        let _ = media_signal_tx.try_send(());
        Ok(())
    });
    let playback_handler = TypedEventHandler::<
        GlobalSystemMediaTransportControlsSession,
        PlaybackInfoChangedEventArgs,
    >::new(move |_sender, _args| {
        let _ = signal_tx.try_send(());
        Ok(())
    });

    Some(CurrentSessionSubscriptions {
        media_properties_token: session.MediaPropertiesChanged(&media_handler).ok(),
        playback_info_token: session.PlaybackInfoChanged(&playback_handler).ok(),
        session,
    })
}

#[cfg(target_os = "windows")]
async fn observe_media_sessions(
    manager: &windows::Media::Control::GlobalSystemMediaTransportControlsSessionManager,
    previous_channel: Option<&str>,
) -> windows::core::Result<MediaObservation> {
    let current_candidate = match manager.GetCurrentSession() {
        Ok(session) => read_media_candidate(&session).await,
        Err(_) => None,
    };
    if current_candidate
        .as_ref()
        .is_some_and(|candidate| !candidate.paused)
    {
        return Ok(MediaObservation::Selected(current_candidate.unwrap()));
    }

    let sessions = manager.GetSessions()?;
    let mut candidates: Vec<MediaCandidate> = Vec::new();
    for session in sessions {
        let Some(candidate) = read_media_candidate(&session).await else {
            continue;
        };
        if let Some(index) = candidates
            .iter()
            .position(|existing| existing.channel == candidate.channel)
        {
            if candidates[index].paused && !candidate.paused {
                candidates[index] = candidate;
            }
        } else {
            candidates.push(candidate);
        }
    }

    let playing = candidates
        .iter()
        .filter(|candidate| !candidate.paused)
        .collect::<Vec<_>>();
    if playing.len() == 1 {
        return Ok(MediaObservation::Selected((*playing[0]).clone()));
    }
    if playing.len() > 1 {
        if let Some(previous) = previous_channel.and_then(|previous| {
            playing
                .iter()
                .find(|candidate| candidate.channel == previous)
        }) {
            return Ok(MediaObservation::Selected((**previous).clone()));
        }
        return Ok(MediaObservation::Ambiguous(playing.len()));
    }

    if let Some(current) = current_candidate {
        return Ok(MediaObservation::Selected(current));
    }
    if candidates.len() == 1 {
        return Ok(MediaObservation::Selected(candidates.remove(0)));
    }
    if candidates.len() > 1 {
        if let Some(previous) = previous_channel.and_then(|previous| {
            candidates
                .iter()
                .find(|candidate| candidate.channel == previous)
                .cloned()
        }) {
            return Ok(MediaObservation::Selected(previous));
        }
        return Ok(MediaObservation::Ambiguous(candidates.len()));
    }

    Ok(MediaObservation::Waiting)
}

#[cfg(target_os = "windows")]
async fn read_media_candidate(
    session: &windows::Media::Control::GlobalSystemMediaTransportControlsSession,
) -> Option<MediaCandidate> {
    use windows::Media::Control::GlobalSystemMediaTransportControlsSessionPlaybackStatus;

    let source_app = session.SourceAppUserModelId().ok()?.to_string();
    if !is_chromium_source(&source_app) {
        return None;
    }
    let playback_status = session.GetPlaybackInfo().ok()?.PlaybackStatus().ok()?;
    if playback_status != GlobalSystemMediaTransportControlsSessionPlaybackStatus::Playing
        && playback_status != GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused
    {
        return None;
    }

    let properties = session
        .TryGetMediaPropertiesAsync()
        .ok()?
        .into_future()
        .await
        .ok()?;
    let title = properties.Title().ok()?.to_string();
    let artist = properties.Artist().ok()?.to_string();
    let album_artist = properties.AlbumArtist().ok()?.to_string();
    let album_title = properties.AlbumTitle().ok()?.to_string();
    if title.trim().is_empty()
        || title.eq_ignore_ascii_case("A site is playing media")
        || !album_artist.trim().is_empty()
        || !album_title.trim().is_empty()
    {
        return None;
    }

    Some(MediaCandidate {
        channel: normalize_channel_login(&artist)?,
        source_app,
        paused: playback_status == GlobalSystemMediaTransportControlsSessionPlaybackStatus::Paused,
    })
}

#[cfg(target_os = "windows")]
fn is_chromium_source(source_app: &str) -> bool {
    let source_app = source_app.to_ascii_lowercase();
    source_app.contains("brave")
        || source_app.contains("chrome")
        || source_app.contains("edge")
        || source_app.contains("chromium")
        || source_app.contains("vivaldi")
}

#[cfg(target_os = "windows")]
fn apply_missing_media_observation(
    status: MediaSessionStatus,
    missing_since: &mut Option<Instant>,
) {
    let first_missing = missing_since.get_or_insert_with(Instant::now);
    let update = if first_missing.elapsed() >= MEDIA_SESSION_STALE_GRACE {
        MediaChannelUpdate::Clear
    } else {
        MediaChannelUpdate::Keep
    };
    update_media_session_state(status, update);
}

#[cfg(target_os = "windows")]
fn update_media_session_state(status: MediaSessionStatus, update: MediaChannelUpdate) {
    let result = {
        let mut state = WATCHING_STATE.write_or_recover();
        state.apply_media_session(status, update)
    };

    if let Some((true, channel)) = result {
        emit_watching_change(channel);
    }
}

#[cfg(target_os = "windows")]
fn pipe_server_loop() {
    use std::os::windows::ffi::OsStrExt;
    use std::os::windows::io::FromRawHandle;
    use windows::Win32::Foundation::{ERROR_PIPE_CONNECTED, GetLastError};
    use windows::Win32::Storage::FileSystem::PIPE_ACCESS_INBOUND;
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, PIPE_READMODE_MESSAGE, PIPE_REJECT_REMOTE_CLIENTS,
        PIPE_TYPE_MESSAGE, PIPE_WAIT,
    };
    use windows::core::PCWSTR;

    let pipe_name = std::ffi::OsStr::new(WATCHING_PIPE_NAME)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();

    loop {
        // SAFETY: `pipe_name` is a NUL-terminated UTF-16 buffer that outlives the call; the handle is checked for validity below.
        let handle = unsafe {
            CreateNamedPipeW(
                PCWSTR(pipe_name.as_ptr()),
                PIPE_ACCESS_INBOUND,
                PIPE_TYPE_MESSAGE | PIPE_READMODE_MESSAGE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                0,
                256,
                0,
                None,
            )
        };
        if handle.is_invalid() {
            let detail = format!(
                "Could not create browser Watching pipe: {}",
                io::Error::last_os_error()
            );
            crate::log_ui!(warn, "{detail}");
            *INTEGRATION_ERROR.write_or_recover() = Some(detail);
            return;
        }

        // SAFETY: `handle` is the valid pipe instance created above.
        let connected = unsafe {
            ConnectNamedPipe(handle, None).is_ok() || GetLastError() == ERROR_PIPE_CONNECTED
        };
        // SAFETY: `handle` is a valid, owned pipe handle; the File takes sole ownership and closes it on drop.
        let mut pipe = unsafe { std::fs::File::from_raw_handle(handle.0) };
        if !connected {
            continue;
        }

        let mut bytes = [0u8; 128];
        let Ok(read) = pipe.read(&mut bytes) else {
            continue;
        };
        let Ok(channel) = std::str::from_utf8(&bytes[..read]) else {
            continue;
        };
        let Some(channel) = normalize_channel_login(channel) else {
            continue;
        };
        publish_extension_channel(channel);
    }
}

#[cfg(target_os = "windows")]
fn publish_extension_channel(channel: String) {
    let (notify, channel_changed) = {
        let mut state = WATCHING_STATE.write_or_recover();
        state.select_extension_channel(&channel)
    };
    if !notify {
        return;
    }

    crate::log_ui!(
        debug,
        channel = %channel,
        channel_changed,
        "Browser extension became authoritative for Watching"
    );
    emit_watching_change(Some(channel));
}

#[cfg(target_os = "windows")]
fn emit_watching_change(channel: Option<String>) {
    crate::backend::push_ui_action(crate::backend::UiAction::WatchingChannelChanged { channel });
}

/// Filesystem locations the registration works with. `current_exe` and the
/// config directory cannot change within a process run, so they are resolved
/// once instead of on every heartbeat.
#[cfg(target_os = "windows")]
struct RegistrationPaths {
    executable: PathBuf,
    directory: PathBuf,
    chrome_manifest: PathBuf,
    firefox_manifest: PathBuf,
    upstream_chrome_manifest: PathBuf,
    upstream_firefox_manifest: PathBuf,
    bridge: PathBuf,
}

#[cfg(target_os = "windows")]
fn registration_paths() -> Result<&'static RegistrationPaths, String> {
    static PATHS: std::sync::OnceLock<Result<RegistrationPaths, String>> =
        std::sync::OnceLock::new();
    PATHS
        .get_or_init(|| {
            let executable = std::env::current_exe()
                .map_err(|error| format!("Could not resolve Echo's executable: {error}"))?;
            let directory = crate::benchmark_profile::config_root()
                .ok_or_else(|| "Windows did not provide a configuration directory".to_string())?
                .join("native-messaging");
            Ok(RegistrationPaths {
                chrome_manifest: directory.join("native-messaging-manifest-chrome.json"),
                firefox_manifest: directory.join("native-messaging-manifest-firefox.json"),
                upstream_chrome_manifest: directory.join("native-messaging-upstream-chrome.json"),
                upstream_firefox_manifest: directory.join("native-messaging-upstream-firefox.json"),
                bridge: directory.join("upstream-host.json"),
                executable,
                directory,
            })
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// Set once a full registration pass has succeeded in this process. While
/// set, the heartbeat only re-verifies the registry keys (the one surface
/// another program can clobber) and skips the manifest/bridge filesystem
/// rebuild entirely.
#[cfg(target_os = "windows")]
static REGISTRATION_VERIFIED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

#[cfg(target_os = "windows")]
fn ensure_native_messaging_proxy_registration() -> Result<(), String> {
    use std::sync::atomic::Ordering;

    let _registration_guard = REGISTRATION_LOCK
        .lock()
        .map_err(|_| "Browser Watching registration lock was poisoned".to_string())?;
    if REGISTRATION_STOPPED.load(Ordering::Acquire) {
        return Ok(());
    }

    let paths = registration_paths()?;
    let registrations = [
        (
            r"Software\Google\Chrome\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
        ),
        (
            r"Software\Microsoft\Edge\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
        ),
        (
            r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
        ),
        (
            r"Software\Vivaldi\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
        ),
        (
            r"Software\Mozilla\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.firefox_manifest,
        ),
    ];

    if REGISTRATION_VERIFIED.load(Ordering::Acquire) {
        let mut intact = true;
        for (key, echo_manifest) in registrations {
            let registered = read_registry_manifest(key)?;
            if !registered
                .as_deref()
                .is_some_and(|manifest| same_windows_path(manifest, echo_manifest))
            {
                intact = false;
                break;
            }
        }
        if intact {
            return Ok(());
        }
        REGISTRATION_VERIFIED.store(false, Ordering::Release);
    }

    let executable = &paths.executable;
    std::fs::create_dir_all(&paths.directory)
        .map_err(|error| format!("Could not create native-messaging directory: {error}"))?;

    let chrome_path = &paths.chrome_manifest;
    let firefox_path = &paths.firefox_manifest;
    let bridge_path = &paths.bridge;

    let mut rebuilt = false;
    let mut bridge = match load_native_host_bridge_config() {
        Ok(bridge) => bridge,
        Err(error) => {
            crate::log_persist!(error, "{error}; rebuilding it from the registry");
            if !crate::utils::preserve_unloadable_file(
                bridge_path,
                "native-messaging bridge configuration",
            ) {
                return Err(error);
            }
            rebuilt = true;
            // The upstream manifests Echo wrote on an earlier run still name
            // the upstream host when the registry already points at Echo.
            NativeHostBridgeConfig {
                upstream_host: [
                    &paths.upstream_chrome_manifest,
                    &paths.upstream_firefox_manifest,
                ]
                .into_iter()
                .find_map(|manifest| upstream_from_manifest(manifest, executable)),
            }
        }
    };
    if bridge
        .upstream_host
        .as_deref()
        .is_some_and(|upstream| !valid_upstream_executable(upstream, executable))
    {
        bridge.upstream_host = None;
    }
    let mut registered_to_echo = false;
    for (key, echo_manifest) in registrations {
        let Some(registered_manifest) = read_registry_manifest(key)? else {
            continue;
        };
        if same_windows_path(&registered_manifest, echo_manifest) {
            registered_to_echo = true;
            continue;
        }
        if let Some(upstream) = upstream_from_manifest(&registered_manifest, executable) {
            bridge.upstream_host = Some(upstream);
        }
    }
    if rebuilt && registered_to_echo && bridge.upstream_host.is_none() {
        // The upstream host is unknown, not absent: a config without one would
        // make the exit restore delete the browser registration. Leaving the
        // unreadable config in place makes the restore leave it untouched.
        crate::log_persist!(
            warn,
            "Browser native-messaging upstream host is unknown after rebuilding its \
             configuration; the registration will be left unchanged at exit"
        );
    } else {
        write_json_file(
            bridge_path,
            &bridge,
            "native-messaging bridge configuration",
        )?;
    }
    if let Some(upstream) = bridge.upstream_host.as_deref() {
        write_upstream_native_messaging_manifests(paths, upstream)?;
    }

    let chrome_manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Echo Watching proxy for Chatterino Native Host",
        "path": executable,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{CHROME_EXTENSION_ID}/")],
    });
    let firefox_manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Echo Watching proxy for Chatterino Native Host",
        "path": executable,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_EXTENSION_ID],
    });
    write_manifest(chrome_path, &chrome_manifest)?;
    write_manifest(firefox_path, &firefox_manifest)?;

    for (key, expected_manifest) in registrations {
        if read_registry_manifest(key)?
            .as_deref()
            .is_some_and(|registered| same_windows_path(registered, expected_manifest))
        {
            continue;
        }
        set_registry_manifest(key, expected_manifest)?;
    }
    REGISTRATION_VERIFIED.store(true, Ordering::Release);
    Ok(())
}

#[cfg(target_os = "windows")]
fn restore_upstream_native_messaging_registration() -> Result<(), String> {
    use std::sync::atomic::Ordering;

    REGISTRATION_STOPPED.store(true, Ordering::Release);
    let _registration_guard = REGISTRATION_LOCK
        .lock()
        .map_err(|_| "Browser Watching registration lock was poisoned".to_string())?;
    let paths = registration_paths()?;
    // Without the saved upstream path, restoring would delete Chatterino's
    // own registration; leave the registry as it is instead.
    let bridge = load_native_host_bridge_config().inspect_err(|error| {
        crate::log_persist!(
            error,
            "{error}; leaving the browser native-messaging registration unchanged"
        );
    })?;
    let upstream = bridge
        .upstream_host
        .as_deref()
        .filter(|path| valid_upstream_executable(path, &paths.executable));

    if let Some(upstream) = upstream {
        write_upstream_native_messaging_manifests(paths, upstream)?;
    }

    let registrations = [
        (
            r"Software\Google\Chrome\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
            &paths.upstream_chrome_manifest,
        ),
        (
            r"Software\Microsoft\Edge\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
            &paths.upstream_chrome_manifest,
        ),
        (
            r"Software\BraveSoftware\Brave-Browser\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
            &paths.upstream_chrome_manifest,
        ),
        (
            r"Software\Vivaldi\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.chrome_manifest,
            &paths.upstream_chrome_manifest,
        ),
        (
            r"Software\Mozilla\NativeMessagingHosts\com.chatterino.chatterino",
            &paths.firefox_manifest,
            &paths.upstream_firefox_manifest,
        ),
    ];

    let mut failures = Vec::new();
    for (key, echo_manifest, upstream_manifest) in registrations {
        let registered = match read_registry_manifest(key) {
            Ok(registered) => registered,
            Err(error) => {
                failures.push(error);
                continue;
            }
        };
        if !registered
            .as_deref()
            .is_some_and(|registered| same_windows_path(registered, echo_manifest))
        {
            continue;
        }

        let result = if upstream.is_some() {
            set_registry_manifest(key, upstream_manifest)
        } else {
            delete_registry_registration(key)
        };
        if let Err(error) = result {
            failures.push(error);
        }
    }
    REGISTRATION_VERIFIED.store(false, Ordering::Release);

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("; "))
    }
}

#[cfg(target_os = "windows")]
fn write_upstream_native_messaging_manifests(
    paths: &RegistrationPaths,
    upstream: &Path,
) -> Result<(), String> {
    let chrome_manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Chatterino Native Host (restored by Echo)",
        "path": upstream,
        "type": "stdio",
        "allowed_origins": [format!("chrome-extension://{CHROME_EXTENSION_ID}/")],
    });
    let firefox_manifest = serde_json::json!({
        "name": NATIVE_HOST_NAME,
        "description": "Chatterino Native Host (restored by Echo)",
        "path": upstream,
        "type": "stdio",
        "allowed_extensions": [FIREFOX_EXTENSION_ID],
    });
    write_manifest(&paths.upstream_chrome_manifest, &chrome_manifest)?;
    write_manifest(&paths.upstream_firefox_manifest, &firefox_manifest)
}

#[cfg(target_os = "windows")]
fn load_native_host_bridge_config() -> Result<NativeHostBridgeConfig, String> {
    let path = crate::benchmark_profile::config_root()
        .ok_or_else(|| "Windows did not provide a configuration directory".to_string())?
        .join("native-messaging")
        .join("upstream-host.json");
    match std::fs::read(&path) {
        Ok(bytes) => serde_json::from_slice(&bytes)
            .map_err(|error| format!("Could not parse {}: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            Ok(NativeHostBridgeConfig::default())
        }
        Err(error) => Err(format!("Could not read {}: {error}", path.display())),
    }
}

#[cfg(target_os = "windows")]
fn upstream_from_manifest(manifest_path: &Path, current_executable: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(manifest_path).ok()?;
    let manifest = serde_json::from_slice::<NativeHostManifest>(&bytes).ok()?;
    (manifest.name == NATIVE_HOST_NAME
        && valid_upstream_executable(&manifest.path, current_executable))
    .then_some(manifest.path)
}

#[cfg(target_os = "windows")]
fn valid_upstream_executable(path: &Path, current_executable: &Path) -> bool {
    path.is_absolute()
        && path.is_file()
        && !same_windows_path(path, current_executable)
        && !path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.eq_ignore_ascii_case("echo.exe"))
}

#[cfg(target_os = "windows")]
fn same_windows_path(left: &Path, right: &Path) -> bool {
    let left = left.as_os_str().to_string_lossy();
    let right = right.as_os_str().to_string_lossy();
    left.len() == right.len()
        && left.bytes().zip(right.bytes()).all(|(l, r)| {
            let l = if l == b'/' { b'\\' } else { l };
            let r = if r == b'/' { b'\\' } else { r };
            l.eq_ignore_ascii_case(&r)
        })
}

#[cfg(target_os = "windows")]
fn write_manifest(path: &Path, value: &serde_json::Value) -> Result<(), String> {
    write_json_file(path, value, "native-messaging manifest")
}

#[cfg(target_os = "windows")]
fn write_json_file<T: serde::Serialize>(
    path: &Path,
    value: &T,
    description: &str,
) -> Result<(), String> {
    let bytes = serde_json::to_vec_pretty(value)
        .map_err(|error| format!("Could not serialize {description}: {error}"))?;
    if std::fs::read(path).ok().as_deref() == Some(bytes.as_slice()) {
        return Ok(());
    }
    let temporary = path.with_extension(format!("json.{}.tmp", std::process::id()));
    std::fs::write(&temporary, bytes)
        .map_err(|error| format!("Could not write {description}: {error}"))?;
    crate::utils::replace_file(&temporary, path).map_err(|error| {
        let _ = std::fs::remove_file(&temporary);
        format!("Could not publish {description}: {error}")
    })
}

#[cfg(target_os = "windows")]
fn read_registry_manifest(key_path: &str) -> Result<Option<PathBuf>, String> {
    use std::ffi::OsString;
    use std::os::windows::ffi::{OsStrExt, OsStringExt};
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_SZ, RegGetValueW};
    use windows::core::PCWSTR;

    let key_path = std::ffi::OsStr::new(key_path)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let mut byte_len = 0u32;
    // SAFETY: `key_path` is NUL-terminated, and a null data pointer asks only for the value size.
    let sized = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            None,
            Some(&mut byte_len),
        )
    };
    if sized == ERROR_FILE_NOT_FOUND || sized == ERROR_PATH_NOT_FOUND {
        return Ok(None);
    }
    if sized != ERROR_SUCCESS {
        return Err(format!(
            "Could not read native-host registry value size (error {})",
            sized.0
        ));
    }
    if byte_len < std::mem::size_of::<u16>() as u32 {
        return Ok(None);
    }

    let mut buffer = vec![0u16; (byte_len as usize).div_ceil(std::mem::size_of::<u16>())];
    let mut actual_byte_len = byte_len;
    // SAFETY: `buffer` holds at least `actual_byte_len` bytes, and `key_path` is NUL-terminated.
    let read = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            PCWSTR::null(),
            RRF_RT_REG_SZ,
            None,
            Some(buffer.as_mut_ptr().cast()),
            Some(&mut actual_byte_len),
        )
    };
    if read != ERROR_SUCCESS {
        return Err(format!(
            "Could not read native-host registry value (error {})",
            read.0
        ));
    }
    let utf16_len = (actual_byte_len as usize / std::mem::size_of::<u16>()).min(buffer.len());
    buffer.truncate(utf16_len);
    if buffer.last() == Some(&0) {
        buffer.pop();
    }
    if buffer.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(OsString::from_wide(&buffer))))
}

#[cfg(target_os = "windows")]
fn set_registry_manifest(key_path: &str, manifest_path: &std::path::Path) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_SET_VALUE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
        RegCreateKeyExW, RegSetValueExW,
    };
    use windows::core::PCWSTR;

    let key_path = std::ffi::OsStr::new(key_path)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let manifest_path = manifest_path
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: reinterprets the live `manifest_path` UTF-16 buffer as its exact byte length; u8 has no alignment requirement.
    let manifest_bytes = unsafe {
        std::slice::from_raw_parts(
            manifest_path.as_ptr().cast::<u8>(),
            manifest_path.len() * std::mem::size_of::<u16>(),
        )
    };

    let mut key = HKEY::default();
    // SAFETY: `key_path` is NUL-terminated, and `key` is a valid out-parameter.
    let created = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_path.as_ptr()),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_SET_VALUE,
            None,
            &mut key,
            None,
        )
    };
    if created != ERROR_SUCCESS {
        return Err(format!(
            "Could not create native-host registry key (error {})",
            created.0
        ));
    }

    // SAFETY: `key` was opened by RegCreateKeyExW above and is closed exactly once here; `manifest_bytes` borrows a live buffer.
    let written =
        unsafe { RegSetValueExW(key, PCWSTR::null(), None, REG_SZ, Some(manifest_bytes)) };
    // SAFETY: `key` is the valid handle opened by RegCreateKeyExW above, not yet closed, and
    // closed exactly once here regardless of whether the write succeeded.
    unsafe {
        let _ = RegCloseKey(key);
    }
    if written != ERROR_SUCCESS {
        return Err(format!(
            "Could not write native-host registry value (error {})",
            written.0
        ));
    }
    Ok(())
}

#[cfg(target_os = "windows")]
fn delete_registry_registration(key_path: &str) -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_PATH_NOT_FOUND, ERROR_SUCCESS};
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RegDeleteTreeW};
    use windows::core::PCWSTR;

    let key_path = std::ffi::OsStr::new(key_path)
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // SAFETY: `key_path` is a NUL-terminated UTF-16 buffer that outlives the call.
    let deleted = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, PCWSTR(key_path.as_ptr())) };
    if deleted == ERROR_SUCCESS
        || deleted == ERROR_FILE_NOT_FOUND
        || deleted == ERROR_PATH_NOT_FOUND
    {
        Ok(())
    } else {
        Err(format!(
            "Could not remove Echo's native-host registry key (error {})",
            deleted.0
        ))
    }
}

#[cfg(target_os = "windows")]
#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    #[test]
    fn media_session_updates_channel_until_extension_is_authoritative() {
        let mut state = WatchingState::default();
        assert_eq!(
            state.apply_media_session(
                MediaSessionStatus::Active {
                    channel: "alpha".to_string(),
                    source_app: "browser".to_string(),
                    paused: false,
                },
                MediaChannelUpdate::Set("alpha".to_string()),
            ),
            Some((true, Some("alpha".to_string())))
        );

        assert_eq!(state.select_extension_channel("beta"), (true, true));
        assert_eq!(
            state.apply_media_session(
                MediaSessionStatus::Active {
                    channel: "gamma".to_string(),
                    source_app: "browser".to_string(),
                    paused: false,
                },
                MediaChannelUpdate::Set("gamma".to_string()),
            ),
            None
        );
        assert_eq!(state.channel.as_deref(), Some("beta"));
        assert_eq!(state.source, Some(WatchingChannelSource::Extension));
        assert!(matches!(
            state.media_session,
            MediaSessionStatus::Active { ref channel, .. } if channel == "gamma"
        ));
    }

    #[test]
    fn extension_selection_notifies_only_for_channel_or_source_changes() {
        let mut state = WatchingState {
            channel: Some("alpha".to_string()),
            source: Some(WatchingChannelSource::MediaSession),
            extension_authoritative: false,
            media_session: MediaSessionStatus::Waiting,
        };

        assert_eq!(state.select_extension_channel("alpha"), (true, false));
        assert_eq!(state.select_extension_channel("alpha"), (false, false));
        assert_eq!(state.select_extension_channel("beta"), (true, true));
    }
}
