//! Contract: custom-command expansion in `src/backend/commands/custom.rs`: argument
//! and channel/stream placeholders, fallbacks, disabled commands, built-in command
//! protection and word-boundary matching.

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

use check_support::{FIXTURE_HEADER, assert_fixture, bool_text};
use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

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

    println!("{FIXTURE_HEADER}");
    print_smoke_rows(
        hello.as_deref(),
        disabled.as_deref(),
        builtin.as_deref(),
        word_boundary.as_deref(),
        dynamic.as_deref(),
        fallback.as_deref(),
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
    assert_fixture(
        "custom_command_arg_tail",
        hello.unwrap_or("none"),
        "Hello one two from SomeChannel",
    );
    assert_fixture(
        "custom_command_disabled_skipped",
        bool_text(disabled.is_none()),
        "true",
    );
    assert_fixture(
        "custom_command_builtin_protected",
        bool_text(builtin.is_none()),
        "true",
    );
    assert_fixture(
        "custom_command_word_boundary",
        bool_text(word_boundary.is_none()),
        "true",
    );
    assert_fixture(
        "custom_command_dynamic_placeholders",
        dynamic.unwrap_or("none"),
        "echo_user:SomeChannel-cached-id:Fixture Stream:Fixture Game:{:",
    );
    assert_fixture(
        "custom_command_fallback_placeholders",
        fallback.unwrap_or("none"),
        "missing:no-id:offline:no-game:{unknown;literal}",
    );
}

fn block_on<F: Future>(future: F) -> F::Output {
    let mut cx = Context::from_waker(Waker::noop());
    let mut future = Box::pin(future);
    loop {
        match Pin::new(&mut future).poll(&mut cx) {
            Poll::Ready(value) => return value,
            Poll::Pending => std::thread::yield_now(),
        }
    }
}
