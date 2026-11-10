//! Helper: prints Echo's version and target, or with `apply-update <args>` runs
//! the self-update installer from `echo-update-core`.

fn main() {
    let arguments: Vec<String> = std::env::args().collect();
    if arguments.get(1).map(String::as_str) != Some("apply-update") {
        println!("Echo {}", env!("CARGO_PKG_VERSION"));
        println!("Target {}/{}", std::env::consts::OS, std::env::consts::ARCH);
        return;
    }

    if let Err(error) = echo_update_core::apply_update(&arguments[2..]) {
        eprintln!("Echo update failed: {error}");
        std::process::exit(1);
    }
}
