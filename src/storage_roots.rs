//! Echo's storage roots: every Echo-owned config, data, cache, log and runtime
//! path is resolved here (`scripts/check-storage-roots.ps1` enforces it).
//!
//! `ECHO_BENCH_PROFILE_ROOT` redirects all of them under one absolute
//! directory so an observation never touches the user's normal state. It moves
//! only Echo-owned state and never changes OS-wide discovery paths:
//! integrations that inspect another application's data are disabled by their
//! callers while a profile is active.

use std::{env, ffi::OsString, fs, io, path::PathBuf, sync::OnceLock};

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

/// The profile resolved from `ECHO_BENCH_PROFILE_ROOT` on first use: `None`
/// when the variable is unset, an error when it names an unusable profile.
static PROFILE_ROOTS: OnceLock<Result<Option<ProfileRoots>, String>> = OnceLock::new();

/// Resolve a profile from the variable's value: validate it and create its
/// directories.
fn resolve_profile_roots(raw: Option<OsString>) -> Result<Option<ProfileRoots>, String> {
    let Some(raw) = raw else {
        return Ok(None);
    };
    let roots = ProfileRoots::from_profile(PathBuf::from(raw));
    if !roots.profile.is_absolute() {
        return Err(format!("{PROFILE_ROOT_ENV} must be an absolute path"));
    }
    roots.create_all().map_err(|error| {
        format!(
            "failed to create benchmark profile roots under '{}': {error}",
            roots.profile.display()
        )
    })?;
    Ok(Some(roots))
}

/// The roots every path lookup uses, given the resolution and whether the
/// variable is set now. A profile that could not be resolved, or a variable
/// set after the roots were resolved without it, is an error: the paths
/// would otherwise be the user's normal ones while the process believes it
/// is isolated.
fn roots_in_use(
    resolved: &Result<Option<ProfileRoots>, String>,
    variable_set_now: bool,
) -> Result<Option<&ProfileRoots>, String> {
    match resolved {
        Err(error) => Err(error.clone()),
        Ok(None) if variable_set_now => Err(format!(
            "{PROFILE_ROOT_ENV} was set after Echo's storage roots were resolved without it"
        )),
        Ok(roots) => Ok(roots.as_ref()),
    }
}

/// Resolve the optional isolated profile before normal application startup
/// and create its directories.
///
/// A configured profile root is required to be absolute. This fails closed:
/// an invalid profile cannot fall through to a user's normal settings,
/// tokens, or caches. Every path lookup resolves the profile the same way on
/// first use, so a library user (a bench, test or tool) that sets the
/// variable without calling this is isolated too; calling it first only
/// turns a bad profile into an error instead of a panic.
pub fn initialize() -> Result<(), String> {
    roots_in_use(resolved_profile_roots(), profile_variable_set()).map(|_| ())
}

fn resolved_profile_roots() -> &'static Result<Option<ProfileRoots>, String> {
    PROFILE_ROOTS.get_or_init(|| resolve_profile_roots(profile_variable()))
}

#[cfg(not(test))]
fn profile_variable() -> Option<OsString> {
    echo_core::env::os_value(PROFILE_ROOT_ENV)
}

/// The library's own tests always run on a profile of their own in the temp
/// directory (one per test process, left behind), so no test can read or
/// write the user's normal settings, caches or history.
#[cfg(test)]
fn profile_variable() -> Option<OsString> {
    echo_core::env::os_value(PROFILE_ROOT_ENV).or_else(|| {
        Some(
            env::temp_dir()
                .join(format!("echo-lib-test-profile-{}", std::process::id()))
                .into_os_string(),
        )
    })
}

fn profile_variable_set() -> bool {
    profile_variable().is_some()
}

/// Whether this process is isolated from the user's normal Echo storage.
pub fn is_isolated() -> bool {
    profile_roots().is_some()
}

pub fn profile_root() -> Option<PathBuf> {
    profile_roots().map(|roots| roots.profile.clone())
}

/// The isolated profile's roots, `None` for normal storage.
///
/// # Panics
///
/// When `ECHO_BENCH_PROFILE_ROOT` names an unusable profile, or was set after
/// the roots were resolved without it: no path may then fall back to the
/// user's normal storage.
pub(crate) fn profile_roots() -> Option<&'static ProfileRoots> {
    match roots_in_use(resolved_profile_roots(), profile_variable_set()) {
        Ok(roots) => roots,
        Err(error) => panic!("{error}; refusing to use the normal Echo storage"),
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use core::prelude::v1::test;

    fn temp_profile(name: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "echo-storage-roots-test-{name}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn the_library_tests_run_on_their_own_temp_profile() {
        let profile = profile_root().expect("tests are isolated");
        assert!(
            profile.starts_with(env::temp_dir()),
            "{}",
            profile.display()
        );
        assert!(config_root().is_some_and(|root| root.starts_with(&profile)));
    }

    #[test]
    fn an_unset_variable_resolves_to_normal_storage() {
        let resolved = resolve_profile_roots(None);
        assert!(matches!(resolved, Ok(None)));
        assert!(matches!(roots_in_use(&resolved, false), Ok(None)));
    }

    #[test]
    fn a_variable_set_before_first_use_isolates_every_root() {
        let profile = temp_profile("lazy");
        let resolved = resolve_profile_roots(Some(profile.clone().into_os_string()));
        let roots = roots_in_use(&resolved, true)
            .expect("an absolute profile resolves")
            .expect("the profile is in use");
        for root in [&roots.config, &roots.data, &roots.cache, &roots.logs] {
            assert!(
                root.starts_with(&profile) && root.is_dir(),
                "{}",
                root.display()
            );
        }
        fs::remove_dir_all(&profile).expect("temporary profile removes");
    }

    #[test]
    fn a_variable_set_after_resolution_without_it_is_refused() {
        let resolved = resolve_profile_roots(None);
        let error = roots_in_use(&resolved, true).expect_err("must not fall back");
        assert!(error.contains(PROFILE_ROOT_ENV), "{error}");
    }

    #[test]
    fn a_relative_profile_is_refused_on_every_lookup() {
        let resolved = resolve_profile_roots(Some(OsString::from("relative-profile")));
        assert!(roots_in_use(&resolved, true).is_err());
        assert!(roots_in_use(&resolved, false).is_err());
        assert!(!PathBuf::from("relative-profile").exists());
    }
}
