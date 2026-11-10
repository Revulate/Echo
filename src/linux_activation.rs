//! Linux single-instance lock and main-window activation.
//!
//! The first Echo holds an advisory lock on `instance.lock` for its whole
//! lifetime and listens on `instance.sock` beside it. A second launch finds
//! the lock taken and forwards its launcher's `XDG_ACTIVATION_TOKEN` over the
//! socket; once the first Echo acknowledges, the second exits and the first
//! activates its main window with that token, which lets KWin raise it instead
//! of only flagging it for attention. A first Echo that no longer acknowledges
//! (it is shutting down) is waited for, so the relaunch then starts normally.
//! Both files live under [`echo_gpui::benchmark_profile::runtime_root`], so a
//! benchmark profile never collides with the user's own Echo.

use std::fs::{File, OpenOptions, TryLockError};
use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const LOCK_FILE: &str = "instance.lock";
const SOCKET_FILE: &str = "instance.sock";
const ACTIVATE_REQUEST: &str = "activate";
const ACTIVATE_ACK: &[u8] = b"ok\n";
const MAX_REQUEST_BYTES: u64 = 4096;
/// How long one activation attempt waits for the running Echo to answer. A
/// running Echo still starting up answers once its main window exists.
const ACK_TIMEOUT: Duration = Duration::from_secs(1);
/// How long a relaunch waits for a running Echo to answer or to exit.
const HANDOFF_DEADLINE: Duration = Duration::from_secs(10);

/// The lock file stays open, and so locked, for the process lifetime.
static INSTANCE_LOCK: OnceLock<File> = OnceLock::new();
/// The bound activation socket, waiting for the main window to exist.
static LISTENER: Mutex<Option<UnixListener>> = Mutex::new(None);

/// Returns `false` when another Echo owns this profile, after that Echo has
/// accepted the activation request. Any failure to create or lock the files
/// fails open, so a broken runtime directory never stops Echo starting.
pub fn acquire_single_instance() -> bool {
    let Some(root) = echo_gpui::benchmark_profile::runtime_root() else {
        return true;
    };
    if let Err(error) = std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&root)
    {
        eprintln!(
            "Echo could not create its runtime directory {}: {error}",
            root.display()
        );
        return true;
    }
    let lock = match OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(root.join(LOCK_FILE))
    {
        Ok(lock) => lock,
        Err(error) => {
            eprintln!("Echo could not open its single-instance lock: {error}");
            return true;
        }
    };
    let socket = root.join(SOCKET_FILE);
    let deadline = Instant::now() + HANDOFF_DEADLINE;
    loop {
        match lock.try_lock() {
            Ok(()) => break,
            Err(TryLockError::WouldBlock) => {
                if activation_acknowledged(&socket) {
                    return false;
                }
                if Instant::now() >= deadline {
                    eprintln!(
                        "Another Echo is running but did not answer; not starting a second one"
                    );
                    return false;
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(TryLockError::Error(error)) => {
                eprintln!("Echo could not take its single-instance lock: {error}");
                return true;
            }
        }
    }
    let _ = INSTANCE_LOCK.set(lock);

    // Holding the lock proves any socket file left here is stale.
    let _ = std::fs::remove_file(&socket);
    match UnixListener::bind(&socket) {
        Ok(listener) => {
            *LISTENER.lock().unwrap_or_else(|error| error.into_inner()) = Some(listener);
        }
        Err(error) => eprintln!(
            "Echo could not listen for activation requests at {}: {error}",
            socket.display()
        ),
    }
    true
}

/// Serve activation requests for `window` once it exists.
pub fn install(window: &gpui::Window, cx: &mut gpui::App) {
    let Some(listener) = LISTENER
        .lock()
        .unwrap_or_else(|error| error.into_inner())
        .take()
    else {
        return;
    };
    let (sender, mut receiver) = tokio::sync::mpsc::unbounded_channel::<Option<String>>();
    if let Err(error) = std::thread::Builder::new()
        .name("echo-activation".into())
        .spawn(move || serve(listener, sender))
    {
        echo_gpui::log_app!(warn, "Could not start the activation listener: {error}");
        return;
    }
    let handle = window.window_handle();
    cx.spawn(async move |cx| {
        while let Some(token) = receiver.recv().await {
            let activated = handle.update(cx, |_, window, _| match token.as_deref() {
                Some(token) => window.activate_window_with_token(token),
                None => window.activate_window(),
            });
            if activated.is_err() {
                return;
            }
        }
    })
    .detach();
}

fn serve(listener: UnixListener, sender: tokio::sync::mpsc::UnboundedSender<Option<String>>) {
    for stream in listener.incoming() {
        let Ok(stream) = stream else {
            continue;
        };
        let _ = stream.set_read_timeout(Some(Duration::from_secs(1)));
        let mut request = String::new();
        if (&stream)
            .take(MAX_REQUEST_BYTES)
            .read_to_string(&mut request)
            .is_err()
        {
            continue;
        }
        let mut lines = request.lines();
        if lines.next() != Some(ACTIVATE_REQUEST) {
            continue;
        }
        let token = lines
            .next()
            .map(str::trim)
            .filter(|token| is_activation_token(token))
            .map(str::to_owned);
        // Once GPUI has stopped, Echo is quitting: close without an answer so
        // the relaunch waits for the lock instead.
        if sender.send(token).is_err() {
            return;
        }
        let _ = (&stream).write_all(ACTIVATE_ACK);
    }
}

/// Activation tokens are opaque, but every compositor issues short printable
/// ASCII; anything else is not forwarded to the compositor.
fn is_activation_token(token: &str) -> bool {
    !token.is_empty() && token.len() <= 512 && token.bytes().all(|byte| byte.is_ascii_graphic())
}

/// Ask the running Echo to activate its window; `true` once it has accepted.
fn activation_acknowledged(socket: &Path) -> bool {
    let Ok(mut stream) = UnixStream::connect(socket) else {
        return false;
    };
    let token = std::env::var("XDG_ACTIVATION_TOKEN")
        .ok()
        .filter(|token| is_activation_token(token));
    let request = match &token {
        Some(token) => format!("{ACTIVATE_REQUEST}\n{token}\n"),
        None => format!("{ACTIVATE_REQUEST}\n"),
    };
    if stream.write_all(request.as_bytes()).is_err() || stream.shutdown(Shutdown::Write).is_err() {
        return false;
    }
    let _ = stream.set_read_timeout(Some(ACK_TIMEOUT));
    let mut reply = Vec::new();
    stream.take(16).read_to_end(&mut reply).is_ok() && reply == ACTIVATE_ACK
}
