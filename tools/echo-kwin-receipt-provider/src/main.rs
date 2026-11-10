use echo_kwin_receipt_provider::{Expectation, OBJECT_PATH, Provider, SERVICE, write_ready};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("KWin receipt provider failed: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let args = parse_args()?;
    let expected = Expectation {
        artifact_root: required_path(&args, "artifact-root")?,
        nonce: required(&args, "nonce")?.to_owned(),
        campaign_id: required(&args, "campaign-id")?.to_owned(),
        expected_pid: required(&args, "expected-pid")?.parse()?,
        expected_start_ticks: required(&args, "expected-start-ticks")?.parse()?,
        expected_executable: required_path(&args, "expected-executable")?,
        expected_executable_sha256: required(&args, "expected-executable-sha256")?.to_owned(),
        expected_profile_root: required_path(&args, "expected-profile-root")?,
        expected_script_sha256: required(&args, "script-sha256")?.to_owned(),
    };
    let timeout_seconds: u64 = required(&args, "timeout-seconds")?.parse()?;
    if !(1..=60).contains(&timeout_seconds) {
        return Err("timeout-seconds must be between 1 and 60".into());
    }
    let provider = Provider::new(expected.clone());
    let connection = zbus::connection::Builder::session()?
        .name(SERVICE)?
        .serve_at(OBJECT_PATH, provider.clone())?
        .build()
        .await?;
    write_ready(&expected)?;
    tokio::time::timeout(Duration::from_secs(timeout_seconds), provider.completed()).await?;
    tokio::task::yield_now().await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    drop(connection);
    Ok(())
}

fn parse_args() -> Result<HashMap<String, String>, String> {
    let mut args = std::env::args().skip(1);
    let mut result = HashMap::new();
    while let Some(flag) = args.next() {
        let key = flag
            .strip_prefix("--")
            .ok_or_else(|| format!("invalid argument: {flag}"))?;
        let value = args
            .next()
            .ok_or_else(|| format!("missing value for {flag}"))?;
        if result.insert(key.to_owned(), value).is_some() {
            return Err(format!("duplicate argument: {flag}"));
        }
    }
    Ok(result)
}

fn required<'a>(args: &'a HashMap<String, String>, name: &str) -> Result<&'a str, String> {
    args.get(name)
        .map(String::as_str)
        .ok_or_else(|| format!("missing --{name}"))
}

fn required_path(args: &HashMap<String, String>, name: &str) -> Result<PathBuf, String> {
    Ok(PathBuf::from(required(args, name)?))
}
