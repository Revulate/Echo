//! KDE Plasma window-manager integration through KWin's scripting D-Bus API.
//!
//! Wayland gives a client no way to keep its window above others or to flag
//! it for attention, and GPUI exposes neither, but KWin (Plasma 5 and 6, on
//! Wayland and X11) runs a short script on request. Each request writes a
//! snippet that finds Echo's main window by process id, app id, and title,
//! then loads, runs, and unloads it through `org.kde.KWin`. Outside a KWin
//! session the first request fails, is logged once, and later requests do
//! nothing.

use std::os::unix::fs::DirBuilderExt;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::time::Duration;

const KWIN_SERVICE: &str = "org.kde.KWin";
const SCRIPTING_PATH: &str = "/Scripting";
const SCRIPTING_INTERFACE: &str = "org.kde.kwin.Scripting";
const SCRIPT_INTERFACE: &str = "org.kde.kwin.Script";
/// KWin answers `run` only after evaluating the script, and never when the
/// script disappears first; no request may hold the connection forever.
const DBUS_METHOD_TIMEOUT: Duration = Duration::from_secs(5);
/// Mentions arrive in bursts, and KWin keeps the flag until Echo is focused.
const ATTENTION_INTERVAL_MS: u64 = 3_000;
/// Always-on-top restored at startup is requested before KWin has mapped the
/// window; repeating the same state later covers a slow first frame.
const STARTUP_KEEP_ABOVE_DELAYS: [Duration; 2] = [Duration::from_secs(1), Duration::from_secs(4)];

const KEEP_ABOVE_UNSET: u8 = 0;
const KEEP_ABOVE_OFF: u8 = 1;
const KEEP_ABOVE_ON: u8 = 2;

static KWIN_UNAVAILABLE: AtomicBool = AtomicBool::new(false);
static KEEP_ABOVE: AtomicU8 = AtomicU8::new(KEEP_ABOVE_UNSET);
static LAST_ATTENTION_MS: AtomicU64 = AtomicU64::new(0);
/// The session-bus connection, opened on first use. Holding the lock for a
/// whole request keeps one script loaded at a time.
static CONNECTION: LazyLock<tokio::sync::Mutex<Option<zbus::Connection>>> =
    LazyLock::new(|| tokio::sync::Mutex::new(None));

/// Keep Echo's main window above other windows, or stop doing so.
pub fn set_main_window_keep_above(enabled: bool) {
    let state = if enabled {
        KEEP_ABOVE_ON
    } else {
        KEEP_ABOVE_OFF
    };
    let previous = KEEP_ABOVE.swap(state, Ordering::AcqRel);
    // A new window is not kept above, so there is nothing to undo yet.
    if previous == KEEP_ABOVE_UNSET && !enabled {
        return;
    }
    let delays: &[Duration] = if previous == KEEP_ABOVE_UNSET {
        &STARTUP_KEEP_ABOVE_DELAYS
    } else {
        &[Duration::ZERO]
    };
    // Each script reads the newest state when it runs, so rapid toggles and
    // the delayed startup attempts settle on the last one.
    for &delay in delays {
        run_main_window_script("keep-above", delay, || {
            let enabled = KEEP_ABOVE.load(Ordering::Acquire) == KEEP_ABOVE_ON;
            format!("w.keepAbove = {enabled};")
        });
    }
}

/// Flag Echo's main window in the task manager, as `FlashWindowEx` does on
/// Windows. KWin clears the flag when the window is activated.
pub fn demand_main_window_attention() {
    let now = crate::utils::unix_now_ms();
    let last = LAST_ATTENTION_MS.load(Ordering::Relaxed);
    if now.saturating_sub(last) < ATTENTION_INTERVAL_MS
        || LAST_ATTENTION_MS
            .compare_exchange(last, now, Ordering::Relaxed, Ordering::Relaxed)
            .is_err()
    {
        return;
    }
    run_main_window_script("attention", Duration::ZERO, || {
        "if (!w.active) { w.demandsAttention = true; }".to_string()
    });
}

fn run_main_window_script(
    kind: &'static str,
    delay: Duration,
    action: impl FnOnce() -> String + Send + 'static,
) {
    if KWIN_UNAVAILABLE.load(Ordering::Relaxed) {
        return;
    }
    crate::backend::bridge::get_runtime().spawn(async move {
        if !delay.is_zero() {
            tokio::time::sleep(delay).await;
        }
        let mut connection = CONNECTION.lock().await;
        if KWIN_UNAVAILABLE.load(Ordering::Relaxed) {
            return;
        }
        // Per-process names: two Echo processes under one KWin (a benchmark
        // profile beside the user's Echo) never unload each other's scripts.
        let plugin = format!("echo-{}-{kind}", std::process::id());
        let script = main_window_script(&action());
        match run_script(&mut connection, &plugin, &script).await {
            Ok(()) => {}
            Err(ScriptError::Unavailable(error)) => {
                if !KWIN_UNAVAILABLE.swap(true, Ordering::Relaxed) {
                    crate::log_app!(info, "KWin window integration is unavailable: {error}");
                }
            }
            Err(ScriptError::Failed(error)) => {
                crate::log_app!(warn, "KWin {kind} script failed: {error}");
            }
        }
    });
}

/// A KWin script that applies `action` to every window `w` that is Echo's
/// main window. Main-window titles are "Echo" or "(N) Echo" (unread
/// mentions); KWin appends " <2>" when two windows share a caption.
fn main_window_script(action: &str) -> String {
    let pid = std::process::id();
    let app_id = crate::identity::APP_ID;
    format!(
        r#"(function () {{
    const mainTitle = /^(\(\d+\+?\) )?Echo( <\d+>)?$/;
    const windows = typeof workspace.windowList === "function"
        ? workspace.windowList()
        : workspace.clientList();
    for (let i = 0; i < windows.length; i++) {{
        const w = windows[i];
        if (w.pid === {pid}
            && String(w.resourceClass) === "{app_id}"
            && mainTitle.test(String(w.caption))) {{
            {action}
        }}
    }}
}})();
"#
    )
}

enum ScriptError {
    /// No session bus, or no KWin on it.
    Unavailable(String),
    Failed(String),
}

async fn run_script(
    slot: &mut Option<zbus::Connection>,
    plugin: &str,
    script: &str,
) -> Result<(), ScriptError> {
    let path = write_script(plugin, script).map_err(ScriptError::Failed)?;
    let connection = match slot.as_ref() {
        Some(connection) => connection.clone(),
        None => {
            let connection = zbus::connection::Builder::session()
                .map(|builder| builder.method_timeout(DBUS_METHOD_TIMEOUT))
                .map_err(|error| ScriptError::Unavailable(format!("no session bus: {error}")))?
                .build()
                .await
                .map_err(|error| ScriptError::Unavailable(format!("no session bus: {error}")))?;
            slot.insert(connection).clone()
        }
    };
    let result = load_and_run(&connection, plugin, path.as_str()).await;
    // KWin numbers scripts by the length of its script list and, once one
    // is unloaded, reuses that number and its object path. A script left
    // loaded would make a later load collide with it, and `run` would then
    // reach the old script. `run` replies after evaluation, so unloading now
    // is safe, and it also removes a script a failed run left behind.
    let unloaded = call(
        &connection,
        SCRIPTING_PATH,
        SCRIPTING_INTERFACE,
        "unloadScript",
        &(plugin,),
    )
    .await;
    let _ = std::fs::remove_file(&path);
    let run_broke_connection = result.as_ref().err().is_some_and(connection_failed);
    if run_broke_connection || unloaded.as_ref().is_err_and(is_connection_error) {
        *slot = None;
    }
    result.map_err(|error| match error {
        RunError::KWinMissing(error) => ScriptError::Unavailable(error.to_string()),
        RunError::Rejected(error) => ScriptError::Failed(error),
        RunError::Dbus(error) => ScriptError::Failed(error.to_string()),
    })
}

enum RunError {
    KWinMissing(zbus::Error),
    Rejected(String),
    Dbus(zbus::Error),
}

fn connection_failed(error: &RunError) -> bool {
    matches!(error, RunError::Dbus(error) if is_connection_error(error))
}

async fn load_and_run(
    connection: &zbus::Connection,
    plugin: &str,
    path: &str,
) -> Result<(), RunError> {
    // A script a crashed Echo with this pid left loaded would block the name.
    let _ = call(
        connection,
        SCRIPTING_PATH,
        SCRIPTING_INTERFACE,
        "unloadScript",
        &(plugin,),
    )
    .await;
    let reply = call(
        connection,
        SCRIPTING_PATH,
        SCRIPTING_INTERFACE,
        "loadScript",
        &(path, plugin),
    )
    .await
    .map_err(|error| {
        if is_error_named(
            &error,
            &[
                "org.freedesktop.DBus.Error.ServiceUnknown",
                "org.freedesktop.DBus.Error.NameHasNoOwner",
            ],
        ) {
            RunError::KWinMissing(error)
        } else {
            RunError::Dbus(error)
        }
    })?;
    let id: i32 = reply
        .body()
        .deserialize()
        .map_err(|error| RunError::Rejected(format!("unexpected loadScript reply: {error}")))?;
    if id < 0 {
        return Err(RunError::Rejected("KWin did not load the script".into()));
    }
    // Plasma 6 exports a loaded script at /Scripting/Script<id>, Plasma 5 at /<id>.
    let plasma6_path = format!("/Scripting/Script{id}");
    match call(connection, &plasma6_path, SCRIPT_INTERFACE, "run", &()).await {
        Ok(_) => Ok(()),
        Err(error)
            if is_error_named(
                &error,
                &[
                    "org.freedesktop.DBus.Error.UnknownObject",
                    "org.freedesktop.DBus.Error.UnknownInterface",
                    "org.freedesktop.DBus.Error.UnknownMethod",
                ],
            ) =>
        {
            call(connection, &format!("/{id}"), SCRIPT_INTERFACE, "run", &())
                .await
                .map(drop)
                .map_err(RunError::Dbus)
        }
        Err(error) => Err(RunError::Dbus(error)),
    }
}

fn is_error_named(error: &zbus::Error, names: &[&str]) -> bool {
    matches!(error, zbus::Error::MethodError(name, _, _) if names.contains(&name.as_str()))
}

/// Anything but a reply from KWin means the connection itself is suspect.
fn is_connection_error(error: &zbus::Error) -> bool {
    !matches!(error, zbus::Error::MethodError(..))
}

async fn call<B>(
    connection: &zbus::Connection,
    path: &str,
    interface: &str,
    method: &str,
    body: &B,
) -> zbus::Result<zbus::Message>
where
    B: serde::Serialize + zbus::zvariant::DynamicType,
{
    connection
        .call_method(Some(KWIN_SERVICE), path, Some(interface), method, body)
        .await
}

/// KWin loads scripts from a file path, so the snippet goes in Echo's private
/// runtime directory until it has run.
fn write_script(plugin: &str, script: &str) -> Result<String, String> {
    let root = crate::storage_roots::runtime_root()
        .ok_or_else(|| "Echo's runtime directory is unavailable".to_string())?;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
        .map_err(|error| format!("could not create {}: {error}", root.display()))?;
    let path = root.join(format!("{plugin}.js"));
    std::fs::write(&path, script)
        .map_err(|error| format!("could not write {}: {error}", path.display()))?;
    path.into_os_string()
        .into_string()
        .map_err(|_| "Echo's runtime directory is not valid UTF-8".to_string())
}
