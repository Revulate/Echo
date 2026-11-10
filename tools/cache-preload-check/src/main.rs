//! Contract: channel emote preload planning in `crates/echo-cache-policy`
//! (eager limit, URL cap, plan summaries); the report is built in `lib.rs`.

use std::io;

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Err(error) = cache_preload_check::run(&args, io::stdout().lock()) {
        eprintln!("cache-preload-check failed: {error}");
        std::process::exit(1);
    }
}
