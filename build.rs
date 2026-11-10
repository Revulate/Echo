use std::path::Path;

fn quoted_value<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    let start = line.find(key)? + key.len();
    let value = &line[start..];
    let quote = value.find('"')? + 1;
    let value = &value[quote..];
    Some(&value[..value.find('"')?])
}

fn package_version(manifest: &Path) -> String {
    let contents = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", manifest.display()));
    let mut in_package = false;
    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_package = trimmed == "[package]";
            continue;
        }
        if in_package && trimmed.starts_with("version") {
            return quoted_value(trimmed, "version")
                .unwrap_or_else(|| panic!("invalid package version in {}", manifest.display()))
                .to_owned();
        }
    }
    panic!("missing [package] version in {}", manifest.display());
}

fn gpui_revision(manifest: &Path) -> String {
    let contents = std::fs::read_to_string(manifest)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", manifest.display()));
    let dependency = contents
        .lines()
        .find(|line| line.trim_start().starts_with("gpui = {"))
        .unwrap_or_else(|| panic!("missing gpui dependency in {}", manifest.display()));
    quoted_value(dependency, "rev")
        .unwrap_or_else(|| panic!("missing gpui revision in {}", manifest.display()))
        .to_owned()
}

const API_PROXY_URL_VAR: &str = "ECHO_API_PROXY_URL";
const API_PROXY_VARS: [&str; 2] = [API_PROXY_URL_VAR, "ECHO_API_PROXY_KEY"];
const REQUIRE_API_PROXY_VAR: &str = "ECHO_REQUIRE_API_PROXY";

/// One `KEY=VALUE` line of `echo-private.env`. Blank and `#` lines are
/// skipped, an `export ` prefix is allowed, and matching single or double
/// quotes around the value are removed.
fn private_env_entry(line: &str) -> Option<(&str, &str)> {
    let line = line.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let line = line.strip_prefix("export ").unwrap_or(line);
    let (key, value) = line.split_once('=')?;
    let value = value.trim();
    let value = ['"', '\'']
        .into_iter()
        .find_map(|quote| value.strip_prefix(quote)?.strip_suffix(quote))
        .unwrap_or(value);
    Some((key.trim(), value.trim()))
}

/// Bakes the optional Echo API proxy into the binary. Each variable comes from
/// the environment, else from the gitignored `echo-private.env` at the repo
/// root. Builds without both have no proxy, which is an error only when
/// `ECHO_REQUIRE_API_PROXY=1`.
fn emit_api_proxy_env(manifest_dir: &Path) {
    let private_env = manifest_dir.join("echo-private.env");
    // Cargo reruns the build script, and rebuilds the crate, on every build
    // while a rerun-if-changed path is missing, so the file is only watched
    // once it exists. Touch build.rs after creating it.
    let file_contents = if private_env.is_file() {
        println!("cargo:rerun-if-changed={}", private_env.display());
        std::fs::read_to_string(&private_env).unwrap_or_default()
    } else {
        String::new()
    };
    let file_contents = file_contents
        .strip_prefix('\u{feff}')
        .unwrap_or(&file_contents);

    let mut missing = Vec::new();
    for name in API_PROXY_VARS {
        println!("cargo:rerun-if-env-changed={name}");
        let value = std::env::var(name)
            .ok()
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
            .or_else(|| {
                file_contents.lines().find_map(|line| {
                    let (key, value) = private_env_entry(line)?;
                    (key == name && !value.is_empty()).then(|| value.to_owned())
                })
            })
            .map(|value| match name {
                API_PROXY_URL_VAR => value.trim_end_matches('/').to_owned(),
                _ => value,
            })
            .filter(|value| !value.is_empty());
        match value {
            Some(value) => println!("cargo:rustc-env={name}={value}"),
            None => missing.push(name),
        }
    }

    println!("cargo:rerun-if-env-changed={REQUIRE_API_PROXY_VAR}");
    if missing.is_empty() {
        return;
    }
    let missing = missing.join(" and ");
    if std::env::var(REQUIRE_API_PROXY_VAR).is_ok_and(|value| value.trim() == "1") {
        panic!(
            "{REQUIRE_API_PROXY_VAR}=1 requires the Echo API proxy: set {missing} in the environment or echo-private.env"
        );
    }
    println!(
        "cargo:warning=Echo API proxy disabled for this build: {missing} not set in the environment or echo-private.env"
    );
}

#[cfg(windows)]
fn windows_version(version: &str) -> u64 {
    let mut fields = version
        .split(['.', '-'])
        .take(3)
        .map(|field| field.parse::<u16>().unwrap_or(0));
    let major = u64::from(fields.next().unwrap_or(0));
    let minor = u64::from(fields.next().unwrap_or(0));
    let patch = u64::from(fields.next().unwrap_or(0));
    (major << 48) | (minor << 32) | (patch << 16)
}

fn main() {
    // Only re-run build script when these specific files change
    // Without this, Cargo re-runs on ANY file change → full rebuild
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/echo.ico");

    let manifest_dir = std::path::PathBuf::from(
        std::env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR is set by Cargo"),
    );
    let manifest = manifest_dir.join("Cargo.toml");
    let gpui_manifest = manifest_dir.join("../zed-gpui-fork/crates/gpui/Cargo.toml");
    println!("cargo:rerun-if-changed={}", manifest.display());
    println!("cargo:rerun-if-changed={}", gpui_manifest.display());
    println!(
        "cargo:rustc-env=ECHO_GPUI_ZED_REV={}",
        gpui_revision(&manifest)
    );
    println!(
        "cargo:rustc-env=ECHO_GPUI_VERSION={}",
        package_version(&gpui_manifest)
    );
    println!(
        "cargo:rustc-env=ECHO_BUILD_TARGET={}",
        std::env::var("TARGET").expect("TARGET is set by Cargo")
    );
    emit_api_proxy_env(&manifest_dir);

    let target_is_windows = std::env::var_os("CARGO_CFG_WINDOWS").is_some();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    if target_is_windows && target_env == "msvc" {
        // Match Zed's Windows GPUI executable stack reserve. The default 1 MB
        // reserve can overflow in deep main-thread render/layout call paths.
        println!("cargo:rustc-link-arg-bin=echo=/stack:{}", 8 * 1024 * 1024);
        // LOAD_LIBRARY_SEARCH_SYSTEM32 for the loader's resolution of this
        // executable's static imports (Windows 10 1607+). It does not cover
        // runtime LoadLibrary calls; main() restricts those separately with
        // SetDefaultDllDirectories.
        println!("cargo:rustc-link-arg-bin=echo=/DEPENDENTLOADFLAG:0x800");
    }

    // `winres` is a host-only build dependency; the target check keeps a
    // Windows host cross-checking Linux from compiling a PE resource.
    #[cfg(windows)]
    if target_is_windows {
        let package_version =
            std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION is set by Cargo");
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/echo.ico");
        res.set("ProductName", "Echo");
        res.set("FileDescription", "Echo - Twitch Chat Client");
        res.set("FileVersion", &package_version);
        res.set("ProductVersion", &package_version);
        res.set("LegalCopyright", "MIT License");
        let numeric_version = windows_version(&package_version);
        res.set_version_info(winres::VersionInfo::FILEVERSION, numeric_version);
        res.set_version_info(winres::VersionInfo::PRODUCTVERSION, numeric_version);
        res.compile().expect("Failed to compile Windows resources");
    }
}
