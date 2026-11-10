mod backend {
    #[derive(Clone)]
    pub struct StreamInfo {
        pub title: String,
        pub game_name: Option<String>,
    }

    pub fn get_stream_info(channel: &str) -> Option<StreamInfo> {
        if channel
            .trim_start_matches('#')
            .eq_ignore_ascii_case("missing")
        {
            return None;
        }

        Some(StreamInfo {
            title: "Fixture Stream".to_string(),
            game_name: Some("Fixture Game".to_string()),
        })
    }

    pub async fn get_stream_info_async(channel: &str) -> Option<StreamInfo> {
        get_stream_info(channel)
    }

    pub mod settings {
        use std::sync::{LazyLock, Mutex};

        pub use echo_core::commands::CustomCommand;

        static COMMANDS: LazyLock<Mutex<Vec<CustomCommand>>> =
            LazyLock::new(|| Mutex::new(Vec::new()));

        pub fn get_custom_commands() -> Vec<CustomCommand> {
            COMMANDS
                .lock()
                .map(|commands| commands.clone())
                .unwrap_or_default()
        }

        pub fn set_custom_commands(commands: Vec<CustomCommand>) {
            if let Ok(mut stored) = COMMANDS.lock() {
                *stored = commands;
            }
        }

        pub fn get_username() -> Option<String> {
            Some("echo_user".to_string())
        }
    }

    pub mod twitch_api {
        pub fn get_cached_channel_id(channel: &str) -> Option<String> {
            if channel.eq_ignore_ascii_case("missing") {
                return None;
            }

            Some(format!("{channel}-cached-id"))
        }

        pub async fn get_user_id_cached(user: &str) -> Result<String, String> {
            if user.eq_ignore_ascii_case("missing") {
                return Err("missing user".to_string());
            }

            Ok(format!("{user}-id"))
        }
    }

    pub mod commands {
        pub fn is_builtin_command(command: &str) -> bool {
            matches!(
                command.to_ascii_lowercase().as_str(),
                "ban" | "timeout" | "me"
            )
        }

        pub mod custom {
            include!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../src/backend/commands/custom.rs"
            ));
        }
    }
}

use std::future::Future;
use std::hint::black_box;
use std::pin::Pin;
use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};
use std::time::Instant;

fn main() {
    backend::settings::set_custom_commands(vec![
        backend::settings::CustomCommand {
            id: "hello".to_string(),
            name: "/hello".to_string(),
            response: "Hello {1+} from {channel.name}".to_string(),
            enabled: true,
        },
        backend::settings::CustomCommand {
            id: "disabled".to_string(),
            name: "/disabled".to_string(),
            response: "hidden".to_string(),
            enabled: false,
        },
        backend::settings::CustomCommand {
            id: "builtin".to_string(),
            name: "/ban".to_string(),
            response: "must not expand".to_string(),
            enabled: true,
        },
        backend::settings::CustomCommand {
            id: "dynamic".to_string(),
            name: "!info".to_string(),
            response: "{my.name}:{channel.id}:{stream.title}:{stream.game}:{{:{9}".to_string(),
            enabled: true,
        },
        backend::settings::CustomCommand {
            id: "fallback".to_string(),
            name: "!fallback".to_string(),
            response: "{channel}:{channel.id;no-id}:{stream.title;offline}:{stream.game;no-game}:{unknown;literal}".to_string(),
            enabled: true,
        },
    ]);

    let hello = block_on(backend::commands::custom::expand_custom_command(
        "#SomeChannel",
        "/hello one two",
    ));
    assert_eq!(hello.as_deref(), Some("Hello one two from SomeChannel"));
    let disabled = block_on(backend::commands::custom::expand_custom_command(
        "#SomeChannel",
        "/disabled",
    ));
    assert_eq!(disabled, None);
    let builtin = block_on(backend::commands::custom::expand_custom_command(
        "#SomeChannel",
        "/ban someone",
    ));
    assert_eq!(builtin, None);
    let word_boundary = block_on(backend::commands::custom::expand_custom_command(
        "#SomeChannel",
        "/hellothere",
    ));
    assert_eq!(word_boundary, None);
    let dynamic = block_on(backend::commands::custom::expand_custom_command(
        "#SomeChannel",
        "!info",
    ));
    assert_eq!(
        dynamic.as_deref(),
        Some("echo_user:SomeChannel-cached-id:Fixture Stream:Fixture Game:{:")
    );
    let fallback = block_on(backend::commands::custom::expand_custom_command(
        "#missing",
        "!fallback",
    ));
    assert_eq!(
        fallback.as_deref(),
        Some("missing:no-id:offline:no-game:{unknown;literal}")
    );

    let iterations = 200_000usize;
    let start = Instant::now();
    let mut checksum = 0usize;
    for _ in 0..iterations {
        let expanded = block_on(backend::commands::custom::expand_custom_command(
            black_box("#SomeChannel"),
            black_box("/hello one two"),
        ))
        .unwrap();
        checksum = checksum.wrapping_add(expanded.len());
    }
    let elapsed = start.elapsed();
    let ns_per_unit = elapsed.as_nanos() as f64 / iterations as f64;
    let units_per_second = iterations as f64 / elapsed.as_secs_f64();

    println!("kind,name,units,ns_per_unit,units_per_second,checksum");
    print_smoke_rows(
        hello.as_deref(),
        disabled.as_deref(),
        builtin.as_deref(),
        word_boundary.as_deref(),
        dynamic.as_deref(),
        fallback.as_deref(),
    );
    println!(
        "expand,custom_command_placeholders,{},{:.2},{:.0},{}",
        iterations, ns_per_unit, units_per_second, checksum
    );
}

fn print_smoke_rows(
    hello: Option<&str>,
    disabled: Option<&str>,
    builtin: Option<&str>,
    word_boundary: Option<&str>,
    dynamic: Option<&str>,
    fallback: Option<&str>,
) {
    print_fixture(
        "custom_command_arg_tail",
        hello.unwrap_or("none"),
        "Hello one two from SomeChannel",
    );
    print_fixture(
        "custom_command_disabled_skipped",
        bool_text(disabled.is_none()),
        "true",
    );
    print_fixture(
        "custom_command_builtin_protected",
        bool_text(builtin.is_none()),
        "true",
    );
    print_fixture(
        "custom_command_word_boundary",
        bool_text(word_boundary.is_none()),
        "true",
    );
    print_fixture(
        "custom_command_dynamic_placeholders",
        dynamic.unwrap_or("none"),
        "echo_user:SomeChannel-cached-id:Fixture Stream:Fixture Game:{:",
    );
    print_fixture(
        "custom_command_fallback_placeholders",
        fallback.unwrap_or("none"),
        "missing:no-id:offline:no-game:{unknown;literal}",
    );
}

fn print_fixture(name: &str, actual: &str, expected: &str) {
    assert_eq!(actual, expected, "fixture mismatch for {name}");
    println!("fixture,{name},1,0.00,0,{}", actual.len());
}

fn bool_text(value: bool) -> &'static str {
    if value { "true" } else { "false" }
}

fn block_on<F: Future>(future: F) -> F::Output {
    let waker = noop_waker();
    let mut cx = Context::from_waker(&waker);
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}

fn noop_waker() -> Waker {
    unsafe fn clone(_: *const ()) -> RawWaker {
        RawWaker::new(std::ptr::null(), &VTABLE)
    }
    unsafe fn wake(_: *const ()) {}
    unsafe fn wake_by_ref(_: *const ()) {}
    unsafe fn drop(_: *const ()) {}
    static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, wake, wake_by_ref, drop);
    // SAFETY: every vtable function ignores its data pointer, so a null pointer satisfies the RawWaker contract.
    unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
}
