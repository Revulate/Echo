use ring::digest::{Context, SHA256};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::fs::{self, File};
use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tokio::sync::Notify;

pub const SERVICE: &str = "org.echo.Benchmark";
pub const OBJECT_PATH: &str = "/org/echo/Benchmark";
pub const INTERFACE: &str = "org.echo.Benchmark";

#[derive(Clone, Debug)]
pub struct Expectation {
    pub artifact_root: PathBuf,
    pub nonce: String,
    pub campaign_id: String,
    pub expected_pid: u32,
    pub expected_start_ticks: u64,
    pub expected_executable: PathBuf,
    pub expected_executable_sha256: String,
    pub expected_profile_root: PathBuf,
    pub expected_script_sha256: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Geometry {
    pub x: f64,
    pub y: f64,
    pub width: f64,
    pub height: f64,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WindowReceipt {
    pub pid: u32,
    pub internal_id: String,
    pub geometry: Geometry,
    pub output: String,
    pub focused: bool,
    pub active: bool,
    pub app_id: String,
    pub resource_class: String,
    pub title: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DisplayReceipt {
    pub output: String,
    pub geometry: Geometry,
    pub scale: f64,
}

#[derive(Clone)]
pub struct Provider {
    expected: Expectation,
    started: Instant,
    complete: std::sync::Arc<Notify>,
}

impl Provider {
    pub fn new(expected: Expectation) -> Self {
        Self {
            expected,
            started: Instant::now(),
            complete: std::sync::Arc::new(Notify::new()),
        }
    }

    pub async fn completed(&self) {
        self.complete.notified().await;
    }

    fn publish(&self, nonce: &str, window_json: &str, display_json: &str) -> Result<(), String> {
        if nonce != self.expected.nonce {
            return Err("campaign nonce mismatch".into());
        }
        if window_json.len() > 65_536 || display_json.len() > 16_384 {
            return Err("receipt payload exceeds bound".into());
        }
        let window: WindowReceipt = serde_json::from_str(window_json)
            .map_err(|error| format!("invalid window receipt: {error}"))?;
        let display: DisplayReceipt = serde_json::from_str(display_json)
            .map_err(|error| format!("invalid display receipt: {error}"))?;
        validate_window(&self.expected, &window)?;
        validate_display(&window, &display)?;
        let monotonic_ns = u64::try_from(self.started.elapsed().as_nanos()).unwrap_or(u64::MAX);
        let provider_hash =
            sha256_file(Path::new("/proc/self/exe")).map_err(|error| error.to_string())?;
        let common = json!({
            "schema_version": 1,
            "campaign_id": self.expected.campaign_id,
            "campaign_nonce_sha256": sha256_bytes(nonce.as_bytes()),
            "provider_executable_sha256": provider_hash,
            "script_sha256": self.expected.expected_script_sha256,
            "captured_provider_monotonic_ns": monotonic_ns,
            "process_start_ticks": self.expected.expected_start_ticks,
            "executable_path": self.expected.expected_executable.canonicalize().map_err(|error| error.to_string())?,
            "executable_sha256": self.expected.expected_executable_sha256,
            "profile_root": self.expected.expected_profile_root.canonicalize().map_err(|error| error.to_string())?,
        });
        let focus = merge(
            common.clone(),
            serde_json::to_value(&window).map_err(|error| error.to_string())?,
        );
        let display = merge(
            common,
            serde_json::to_value(&display).map_err(|error| error.to_string())?,
        );
        write_atomic_json(&self.expected.artifact_root, "focus.json", &focus)
            .map_err(|error| error.to_string())?;
        write_atomic_json(&self.expected.artifact_root, "display.json", &display)
            .map_err(|error| error.to_string())?;
        self.complete.notify_one();
        Ok(())
    }
}

#[zbus::interface(name = "org.echo.Benchmark")]
impl Provider {
    async fn publish_window(
        &self,
        nonce: &str,
        window_json: &str,
        display_json: &str,
    ) -> zbus::fdo::Result<bool> {
        self.publish(nonce, window_json, display_json)
            .map(|()| true)
            .map_err(zbus::fdo::Error::Failed)
    }
}

pub fn validate_window(expected: &Expectation, receipt: &WindowReceipt) -> Result<(), String> {
    if receipt.pid != expected.expected_pid {
        return Err("active window PID mismatch".into());
    }
    if !receipt.active || !receipt.focused {
        return Err("active window is not focused".into());
    }
    if receipt.internal_id.is_empty()
        || receipt.output.is_empty()
        || (receipt.app_id.is_empty() && receipt.resource_class.is_empty())
    {
        return Err("active window identity is incomplete".into());
    }
    validate_geometry(&receipt.geometry)?;
    let pid = expected.expected_pid;
    let start_ticks = proc_start_ticks(pid)?;
    if start_ticks != expected.expected_start_ticks {
        return Err("process start ticks mismatch".into());
    }
    let executable =
        fs::canonicalize(format!("/proc/{pid}/exe")).map_err(|error| error.to_string())?;
    if executable
        != fs::canonicalize(&expected.expected_executable).map_err(|error| error.to_string())?
    {
        return Err("process executable path mismatch".into());
    }
    if sha256_file(&executable).map_err(|error| error.to_string())?
        != expected.expected_executable_sha256
    {
        return Err("process executable hash mismatch".into());
    }
    let environ = fs::read(format!("/proc/{pid}/environ")).map_err(|error| error.to_string())?;
    let profile = environ
        .split(|byte| *byte == 0)
        .find_map(|entry| entry.strip_prefix(b"ECHO_BENCH_PROFILE_ROOT="))
        .ok_or_else(|| "process profile root is missing".to_owned())?;
    if Path::new(std::str::from_utf8(profile).map_err(|error| error.to_string())?)
        .canonicalize()
        .map_err(|error| error.to_string())?
        != expected
            .expected_profile_root
            .canonicalize()
            .map_err(|error| error.to_string())?
    {
        return Err("process profile root mismatch".into());
    }
    Ok(())
}

pub fn validate_display(window: &WindowReceipt, display: &DisplayReceipt) -> Result<(), String> {
    if display.output != window.output || display.output.is_empty() {
        return Err("display output mismatch".into());
    }
    validate_geometry(&display.geometry)?;
    if !display.scale.is_finite() || display.scale <= 0.0 || display.scale > 8.0 {
        return Err("display scale is invalid".into());
    }
    Ok(())
}

fn validate_geometry(geometry: &Geometry) -> Result<(), String> {
    if !geometry.x.is_finite()
        || !geometry.y.is_finite()
        || !geometry.width.is_finite()
        || !geometry.height.is_finite()
        || geometry.width <= 0.0
        || geometry.height <= 0.0
    {
        return Err("receipt geometry is invalid".into());
    }
    Ok(())
}

pub fn write_ready(expected: &Expectation) -> io::Result<()> {
    let value = json!({
        "schema_version": 1,
        "service": SERVICE,
        "object_path": OBJECT_PATH,
        "interface": INTERFACE,
        "campaign_id": expected.campaign_id,
        "campaign_nonce_sha256": sha256_bytes(expected.nonce.as_bytes()),
        "provider_executable_sha256": sha256_file(Path::new("/proc/self/exe"))?,
        "script_sha256": expected.expected_script_sha256,
    });
    write_atomic_json(&expected.artifact_root, "provider-ready.json", &value)
}

fn write_atomic_json(root: &Path, name: &str, value: &Value) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let destination = root.join(name);
    let temporary = root.join(format!(".{name}.tmp-{}", std::process::id()));
    let bytes = serde_json::to_vec_pretty(value).map_err(io::Error::other)?;
    let mut file = File::create(&temporary)?;
    // The provider only runs under KWin; the gate keeps the workspace building
    // on Windows.
    #[cfg(unix)]
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    fs::rename(temporary, destination)
}

fn merge(mut left: Value, right: Value) -> Value {
    if let (Some(left), Some(right)) = (left.as_object_mut(), right.as_object()) {
        left.extend(right.clone());
    }
    left
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut context = Context::new(&SHA256);
    let mut buffer = [0_u8; 65_536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        context.update(&buffer[..count]);
    }
    Ok(hex(context.finish().as_ref()))
}

pub fn sha256_bytes(bytes: &[u8]) -> String {
    hex(ring::digest::digest(&SHA256, bytes).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn proc_start_ticks(pid: u32) -> Result<u64, String> {
    let stat =
        fs::read_to_string(format!("/proc/{pid}/stat")).map_err(|error| error.to_string())?;
    stat.split_whitespace()
        .nth(21)
        .ok_or_else(|| "process stat lacks start ticks".to_owned())?
        .parse()
        .map_err(|error| format!("invalid process start ticks: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_requires_matching_output_and_bounded_scale() {
        let window = sample_window();
        let mut display = sample_display();
        validate_display(&window, &display).unwrap();
        display.output = "other".into();
        assert!(validate_display(&window, &display).is_err());
    }

    #[test]
    fn geometry_rejects_nonfinite_and_empty_dimensions() {
        assert!(
            validate_geometry(&Geometry {
                x: 0.0,
                y: 0.0,
                width: 0.0,
                height: 1.0
            })
            .is_err()
        );
        assert!(
            validate_geometry(&Geometry {
                x: f64::NAN,
                y: 0.0,
                width: 1.0,
                height: 1.0
            })
            .is_err()
        );
    }

    #[test]
    fn atomic_receipt_is_private_and_replaced() {
        let temp = tempfile::tempdir().unwrap();
        write_atomic_json(temp.path(), "receipt.json", &json!({"value": 1})).unwrap();
        write_atomic_json(temp.path(), "receipt.json", &json!({"value": 2})).unwrap();
        let path = temp.path().join("receipt.json");
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap()["value"],
            2
        );
    }

    fn sample_window() -> WindowReceipt {
        WindowReceipt {
            pid: 1,
            internal_id: "id".into(),
            geometry: Geometry {
                x: 0.0,
                y: 0.0,
                width: 10.0,
                height: 10.0,
            },
            output: "DP-1".into(),
            focused: true,
            active: true,
            app_id: "echo".into(),
            resource_class: "echo".into(),
            title: "Echo".into(),
        }
    }

    fn sample_display() -> DisplayReceipt {
        DisplayReceipt {
            output: "DP-1".into(),
            geometry: Geometry {
                x: 0.0,
                y: 0.0,
                width: 1920.0,
                height: 1080.0,
            },
            scale: 1.0,
        }
    }
}
