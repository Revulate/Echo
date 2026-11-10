//! Echo's storage roots: every Echo-owned config, data, cache, log and runtime
//! path is resolved here (`scripts/check-storage-roots.ps1` enforces it).
//!
//! `ECHO_BENCH_PROFILE_ROOT` redirects all of them under one absolute
//! directory so an observation never touches the user's normal state. It moves
//! only Echo-owned state and never changes OS-wide discovery paths:
//! integrations that inspect another application's data are disabled by their
//! callers while a profile is active.

use std::{env, fs, io, path::PathBuf, sync::OnceLock};

pub(crate) const PROFILE_ROOT_ENV: &str = "ECHO_BENCH_PROFILE_ROOT";

/// The directories of an isolated profile.
#[derive(Clone, Debug)]
pub(crate) struct ProfileRoots {
    pub(crate) profile: PathBuf,
    pub(crate) config: PathBuf,
    pub(crate) data: PathBuf,
    pub(crate) cache: PathBuf,
    pub(crate) logs: PathBuf,
}

impl ProfileRoots {
    fn from_profile(profile: PathBuf) -> Self {
        Self {
            config: profile.join("config"),
            data: profile.join("data"),
            cache: profile.join("cache"),
            logs: profile.join("logs"),
            profile,
        }
    }

    fn create_all(&self) -> io::Result<()> {
        for root in [
            &self.profile,
            &self.config,
            &self.data,
            &self.cache,
            &self.logs,
        ] {
            fs::create_dir_all(root)?;
        }
        for directory in ["cache", "history", "logs", "native-messaging"] {
            fs::create_dir_all(self.config.join(directory))?;
        }
        fs::create_dir_all(self.cache.join("updates"))?;
        Ok(())
    }
}

static PROFILE_ROOTS: OnceLock<Option<ProfileRoots>> = OnceLock::new();

/// Resolve the optional isolated profile before normal application startup
/// and create its directories.
///
/// A configured profile root is required to be absolute. This fails closed:
/// an invalid profile cannot fall through to a user's normal settings,
/// tokens, or caches.
pub fn initialize() -> Result<(), String> {
    let roots = PROFILE_ROOTS.get_or_init(|| {
        let raw = echo_core::env::os_value(PROFILE_ROOT_ENV)?;
        Some(ProfileRoots::from_profile(PathBuf::from(raw)))
    });
    let Some(roots) = roots.as_ref() else {
        return Ok(());
    };
    if !roots.profile.is_absolute() {
        return Err(format!("{PROFILE_ROOT_ENV} must be an absolute path"));
    }
    roots.create_all().map_err(|error| {
        format!(
            "failed to create benchmark profile roots under '{}': {error}",
            roots.profile.display()
        )
    })
}

/// Whether this process is isolated from the user's normal Echo storage.
pub fn is_isolated() -> bool {
    profile_roots().is_some() || echo_core::env::os_value(PROFILE_ROOT_ENV).is_some()
}

pub fn profile_root() -> Option<PathBuf> {
    profile_roots().map(|roots| roots.profile.clone())
}

pub(crate) fn profile_roots() -> Option<&'static ProfileRoots> {
    PROFILE_ROOTS.get().and_then(Option::as_ref)
}
pub fn config_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.config.clone())
        .or_else(|| dirs::config_dir().map(|path| path.join("echo")))
}

pub fn config_root_or_current_dir() -> PathBuf {
    config_root().unwrap_or_else(|| {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join("echo")
    })
}

/// Preserve the one historical caller whose fallback was the working directory
/// itself rather than a dedicated `echo` directory.
pub fn config_root_or_dot() -> PathBuf {
    config_root().unwrap_or_else(|| PathBuf::from("."))
}

pub fn data_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.data.clone())
        .or_else(|| dirs::data_local_dir().map(|path| path.join("echo")))
}

pub fn cache_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.cache.clone())
        .or_else(|| dirs::cache_dir().map(|path| path.join("echo")))
}

/// Runtime log root. In a profile it is separate from all other state;
/// ordinary installs retain their existing local-data log location.
pub fn log_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.logs.clone())
        .or_else(data_root)
}

/// Per-user runtime directory for the Linux single-instance lock, its
/// activation socket, and short-lived desktop-integration scripts. A profile
/// keeps its own, so a campaign never collides with (or raises) the user's
/// Echo; ordinary installs use `XDG_RUNTIME_DIR`, falling back to the
/// local-data root where it is unset.
pub fn runtime_root() -> Option<PathBuf> {
    profile_roots()
        .map(|roots| roots.data.join("runtime"))
        .or_else(|| dirs::runtime_dir().map(|path| path.join("echo")))
        .or_else(data_root)
}

/// The OS configuration root for *other applications*. Benchmark profiles must
/// not point OBS/Discord discovery at synthetic Echo directories.
pub fn external_config_dir() -> Option<PathBuf> {
    (!is_isolated()).then(dirs::config_dir).flatten()
}

/// The OS per-user application data root for *other applications*
/// (`%APPDATA%`, `~/.local/share`, `~/Library/Application Support`), where the
/// settings importer looks for the install it imports from. `None` while a
/// benchmark profile is active, so a campaign never reads the user's real
/// settings.
pub fn external_data_dir() -> Option<PathBuf> {
    (!is_isolated()).then(dirs::data_dir).flatten()
}

/// A historical cwd file is readable only outside a benchmark profile.
pub fn legacy_cwd_file(name: &str) -> Option<PathBuf> {
    (!is_isolated()).then(|| PathBuf::from(name))
}

/// Emote preferences predate the main settings store and historically lived
/// under `HOME/.config/echo`; preserve that normal location while isolating a
/// campaign in its config root.
pub fn emote_preferences_path() -> PathBuf {
    if is_isolated() {
        return config_root_or_current_dir().join("emote_preferences.json");
    }
    let home = env::var("HOME")
        .or_else(|_| env::var("USERPROFILE"))
        .unwrap_or_else(|_| ".".to_string());
    PathBuf::from(home)
        .join(".config")
        .join("echo")
        .join("emote_preferences.json")
}

/// Sound settings live in the config root; `SoundStore` migrates the
/// historical working-directory file via `legacy_cwd_file("sounds.json")`.
pub fn sound_settings_path() -> PathBuf {
    config_root_or_current_dir().join("sounds.json")
}

/// Profiling artifacts use the profile log root when isolation is enabled and
/// retain their cwd filename in ordinary runs.
pub fn profiling_artifact_path(name: &str) -> PathBuf {
    if is_isolated() {
        log_root().unwrap_or_else(|| PathBuf::from(".")).join(name)
    } else {
        PathBuf::from(name)
    }
}
