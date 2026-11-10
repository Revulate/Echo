//! Echo GPUI - A native Twitch chat client built with GPUI
//!
//! This is the main entry point for the application.

#![cfg_attr(
    all(not(debug_assertions), not(feature = "dhat-heap")),
    windows_subsystem = "windows"
)]
#![recursion_limit = "4096"]

#[cfg(target_os = "linux")]
mod linux_activation;
#[cfg(windows)]
mod windows_activation;
#[cfg(windows)]
mod windows_tray;

#[cfg(feature = "dhat-heap")]
#[global_allocator]
static GLOBAL: dhat::Alloc = dhat::Alloc;

#[cfg(all(feature = "alloc-profile", not(feature = "dhat-heap")))]
#[global_allocator]
static GLOBAL: echo_gpui::diagnostics::alloc_profile::CountingAllocator =
    echo_gpui::diagnostics::alloc_profile::CountingAllocator::new();

use echo_gpui::assets::EchoAssets;
use echo_gpui::components::ui::{Root, open_native_window};
use echo_gpui::state::AppState;
use echo_gpui::utils::main_window_restore_bounds;
use echo_gpui::{EchoApp, identity};
use echo_gpui::{
    backend, cache, components, diagnostics::demo, diagnostics::fps_profiler,
    diagnostics::profiling, log, managers, state, theme,
};
use gpui::*;
use reqwest_client::ReqwestClient;
use std::{sync::Arc, time::Duration};

/// GPUI only uses this client for finite media assets. Match Echo's own
/// four-second media request budget so a URI body that stops producing bytes
/// cannot pin GPUI's fallback loader indefinitely; this is an idle-read timeout,
/// not a total download deadline.
const GPUI_MEDIA_BODY_READ_TIMEOUT: Duration = Duration::from_secs(4);

/// Unfocused-window frame interval forced by `ECHO_INACTIVE_THROTTLE=1`: 30 fps.
const FORCED_INACTIVE_FRAME_INTERVAL: Duration = Duration::from_micros(33_333);

/// GPUI's frame gate for an unfocused main window. `ECHO_INACTIVE_THROTTLE`
/// forces 30 fps when on, disables the gate when off, and otherwise leaves the
/// scheduler's default.
fn inactive_frame_interval() -> Option<Duration> {
    match echo_core::env::tristate("ECHO_INACTIVE_THROTTLE") {
        echo_core::env::Tristate::On => Some(FORCED_INACTIVE_FRAME_INTERVAL),
        echo_core::env::Tristate::Off => None,
        echo_core::env::Tristate::Default => Some(Duration::from_millis(
            managers::dynamic_tick_scheduler::inactive_frame_gate_interval_ms(),
        )),
    }
}

/// Record the main window's GPU adapter under the session header, once.
fn log_gpu_adapter(window: &Window) {
    let Some(specs) = window.gpu_specs() else {
        profiling::write_session_detail("==== gpu adapter unavailable ====");
        return;
    };
    profiling::write_session_detail(&format!(
        "==== gpu {} | vendor {} | driver {} | software emulated {} ====",
        specs.device_name, specs.driver_name, specs.driver_info, specs.is_software_emulated,
    ));
    if specs.is_software_emulated {
        echo_gpui::log_app!(
            warn,
            device = %specs.device_name,
            "Rendering on a software GPU adapter; expect high CPU use"
        );
    }
}

/// GPUI's image/media client, routed through Echo's proxy setting.
///
/// The fork's `reqwest_client` attaches `NoProxy::from_env()` to the proxy, so
/// hosts listed in `NO_PROXY`/`no_proxy` bypass Echo's proxy for GPUI media
/// requests, while Echo's own clients still route them through it.
fn build_gpui_http_client() -> gpui::Result<ReqwestClient> {
    let proxy = backend::http::effective_proxy_url();
    let no_proxy_set = ["NO_PROXY", "no_proxy"]
        .iter()
        .any(|name| std::env::var_os(name).is_some_and(|value| !value.is_empty()));
    if proxy.is_some() && no_proxy_set {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            echo_gpui::log_net!(
                warn,
                "NO_PROXY is set: media requests to the hosts it lists bypass Echo's proxy"
            );
        });
    }
    ReqwestClient::proxy_user_agent_and_read_timeout(
        proxy,
        identity::USER_AGENT,
        Some(GPUI_MEDIA_BODY_READ_TIMEOUT),
    )
}

/// Restrict runtime `LoadLibrary` searches to System32 and Echo's own
/// directory, so a DLL planted in the working directory or on `PATH` is never
/// loaded. GPUI's optional vendor probes (`nvapi64.dll`, `amd_ags_x64.dll`)
/// still find driver DLLs installed in System32.
#[cfg(windows)]
fn restrict_dll_search_path() {
    const LOAD_LIBRARY_SEARCH_APPLICATION_DIR: u32 = 0x0000_0200;
    const LOAD_LIBRARY_SEARCH_SYSTEM32: u32 = 0x0000_0800;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn SetDefaultDllDirectories(directory_flags: u32) -> i32;
    }

    // SAFETY: SetDefaultDllDirectories takes only flag bits and runs before any other thread loads a DLL.
    unsafe {
        SetDefaultDllDirectories(
            LOAD_LIBRARY_SEARCH_APPLICATION_DIR | LOAD_LIBRARY_SEARCH_SYSTEM32,
        );
    }
}

fn main() {
    allocator::configure();
    #[cfg(windows)]
    restrict_dll_search_path();
    let args: Vec<String> = std::env::args().skip(1).collect();

    // The updater can run elevated, so it starts before anything reads
    // user-controlled `ECHO_BENCH_*` environment or profile state.
    if args.first().map(String::as_str) == Some("apply-update") {
        if let Err(error) = echo_update_core::apply_update(&args[1..]) {
            eprintln!("Echo update failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    backend::reliability::install_panic_hook();
    echo_gpui::diagnostics::startup_metrics::initialize_clock();
    components::chat::canvas_renderer::install_render_ports();

    if let Err(error) =
        echo_gpui::storage_roots::initialize().and_then(|()| echo_gpui::bench::initialize())
    {
        eprintln!("Echo benchmark profile initialization failed: {error}");
        std::process::exit(1);
    }
    backend::reliability::note_storage_roots_ready();

    if backend::browser_watching::is_native_messaging_invocation(&args) {
        if let Err(error) = backend::browser_watching::run_native_messaging_host() {
            eprintln!("Echo browser integration failed: {error}");
            std::process::exit(1);
        }
        return;
    }

    if handle_version_command(&args) {
        return;
    }

    #[cfg(windows)]
    if !echo_gpui::bench::ui_fixture::single_instance_bypass_enabled()
        && !windows_activation::acquire_single_instance()
    {
        return;
    }
    #[cfg(target_os = "linux")]
    if !echo_gpui::bench::ui_fixture::single_instance_bypass_enabled()
        && !linux_activation::acquire_single_instance()
    {
        return;
    }

    profiling::init();

    echo_gpui::diagnostics::alloc_profile::init();

    echo_gpui::diagnostics::dhat_profile::start();

    log::init();
    state::install_domain_ports();
    echo_gpui::diagnostics::startup_metrics::record(
        echo_gpui::diagnostics::startup_metrics::StartupMarker::ProcessCreated,
    );

    backend::updates::start_staged_installer_cleanup();

    allocator::start_trim_thread();

    fps_profiler::init();
    backend::metrics::init();

    let anim_cap_hz = managers::dynamic_tick_scheduler::animation_cap_fps_hz();
    let anim_cap_source = managers::dynamic_tick_scheduler::animation_cap_source();
    let anim_min_tick_ms = managers::dynamic_tick_scheduler::animation_min_tick_interval_ms();
    echo_gpui::log_perf!(
        info,
        cap_hz = anim_cap_hz,
        source = anim_cap_source,
        min_tick_ms = anim_min_tick_ms,
        "Animation cap configured: {}Hz (source={}, min_tick={}ms)",
        anim_cap_hz,
        anim_cap_source,
        anim_min_tick_ms
    );

    backend::reliability::initialize_for_current_run(&args);

    demo::init_from_args();

    let prewarm_handle = std::thread::spawn(|| {
        backend::http::ensure_rustls_crypto_provider();
        let _ = echo_gpui::cache::budget::prewarmed_total_memory_bytes();
        echo_gpui::backend::twitch_api::prewarm_http_client();
    });

    gpui_platform::application()
        .with_assets(EchoAssets)
        .run(move |cx: &mut App| {
            // Before any text exists: Linux replaces GPUI's uninstalled defaults.
            theme::fonts::install_platform_font_families(cx);
            echo_gpui::utils::install_platform_clipboard(cx);
            // The prewarm steps are exactly-once lazies, so an unfinished
            // prewarm can keep running detached; only the TLS provider must be
            // installed before the media client below, and that is a `Once`.
            backend::http::ensure_rustls_crypto_provider();
            if prewarm_handle.is_finished() {
                if let Err(e) = prewarm_handle.join() {
                    echo_gpui::log_app!(warn, "Prewarm thread panicked: {:?}", e);
                }
                echo_gpui::diagnostics::startup_metrics::record(
                    echo_gpui::diagnostics::startup_metrics::StartupMarker::PrewarmJoined,
                );
            } else {
                echo_gpui::diagnostics::startup_metrics::record(
                    echo_gpui::diagnostics::startup_metrics::StartupMarker::PrewarmDetached,
                );
            }

            match build_gpui_http_client() {
                Ok(http_client) => cx.set_http_client(Arc::new(http_client)),
                Err(e) => {
                    echo_gpui::log_net!(error, "Failed to create HTTP client: {e}");
                    backend::reliability::note_startup_degraded_feature(
                        "Network media support is degraded",
                        format!(
                            "Echo could not initialize its HTTP client: {e}. Emotes, badges, and link previews may be unavailable until restart."
                        ),
                    );
                }
            }

            let mut proxy_changes = backend::http::subscribe_proxy_changes();
            cx.spawn(async move |cx| {
                while proxy_changes.changed().await.is_ok() {
                    match build_gpui_http_client() {
                        Ok(http_client) => {
                            cx.update(|cx| cx.set_http_client(Arc::new(http_client)));
                            echo_gpui::log_net!(info, "Rebuilt the media HTTP client for new proxy settings");
                        }
                        Err(e) => echo_gpui::log_net!(
                            warn,
                            "Could not rebuild the media HTTP client for new proxy settings: {e}"
                        ),
                    }
                }
            })
            .detach();

            echo_gpui::diagnostics::startup_metrics::record(
                echo_gpui::diagnostics::startup_metrics::StartupMarker::GpuiHttpClientReady,
            );
            #[cfg(unix)]
            quit_on_termination_signals(cx);
            backend::initialize();
            echo_gpui::diagnostics::startup_metrics::record(
                echo_gpui::diagnostics::startup_metrics::StartupMarker::BackendInitialized,
            );

            cx.set_reduce_motion(backend::settings::get_reduce_decorative_motion());

            cache::uri_layer::init(cx);

            state::emote_preferences::init_preferences();
            echo_gpui::diagnostics::startup_metrics::record(
                echo_gpui::diagnostics::startup_metrics::StartupMarker::CachesInitialized,
            );

            components::ui::init(cx);

            cx.set_cursor_hide_mode(CursorHideMode::Never);

            let window_transparent = backend::settings::get_window_transparent();
            let always_on_top = backend::settings::get_always_on_top();

            cx.set_text_rendering_mode(TextRenderingMode::Subpixel);

            echo_gpui::diagnostics::startup_metrics::record(
                echo_gpui::diagnostics::startup_metrics::StartupMarker::ThemeInitialized,
            );

            let (main_window_width, main_window_height) = backend::settings::get_main_window_size();
            let restore_size = size(px(main_window_width), px(main_window_height));
            let restore_bounds = main_window_restore_bounds(
                backend::settings::get_main_window_position(),
                restore_size,
                cx,
            );
            let window_bounds = if backend::settings::get_main_window_maximized() {
                WindowBounds::Maximized(restore_bounds)
            } else {
                WindowBounds::Windowed(restore_bounds)
            };

            let inactive_frame_interval = inactive_frame_interval();

            echo_gpui::diagnostics::startup_metrics::record(
                echo_gpui::diagnostics::startup_metrics::StartupMarker::MainWindowOpening,
            );
            if let Err(e) = open_native_window(
                cx,
                WindowOptions {
                    inactive_frame_interval,
                    titlebar: Some(TitlebarOptions {
                        title: Some("Echo".into()),
                        appears_transparent: true,
                        ..Default::default()
                    }),
                    window_bounds: Some(window_bounds),
                    focus: true,
                    show: true,
                    kind: if always_on_top {
                        WindowKind::Floating
                    } else {
                        WindowKind::Normal
                    },
                    is_movable: true,
                    display_id: None,
                    window_background: if window_transparent {
                        WindowBackgroundAppearance::Transparent
                    } else {
                        WindowBackgroundAppearance::Opaque
                    },
                    app_id: Some(identity::APP_ID.to_string()),
                    window_min_size: Some(size(
                        px(backend::settings::MAIN_WINDOW_MIN_WIDTH_PX as f32),
                        px(backend::settings::MAIN_WINDOW_MIN_HEIGHT_PX as f32),
                    )),
                    ..Default::default()
                },
                |window, cx| {
                    // Closing the main window quits Echo on every platform.
                    // GPUI's default only quits once the last window closes, so
                    // an open settings, channel, or user-card popout would
                    // otherwise keep a process with no main window alive.
                    window.on_window_should_close(cx, |_, cx| {
                        cx.quit();
                        false
                    });
                    #[cfg(windows)]
                    windows_tray::install(window);
                    #[cfg(windows)]
                    if !echo_gpui::bench::ui_fixture::single_instance_bypass_enabled()
                        && !echo_gpui::storage_roots::is_isolated()
                    {
                        windows_activation::install(window);
                    }
                    #[cfg(target_os = "linux")]
                    linux_activation::install(window, cx);
                    echo_gpui::diagnostics::startup_metrics::record(
                        echo_gpui::diagnostics::startup_metrics::StartupMarker::MainWindowCreated,
                    );
                    window.on_next_frame(|window, _| log_gpu_adapter(window));
                    components::chat::canvas_renderer::initialize_font_metrics(window, cx);

                    let app_state = cx.new(AppState::new);
                    let popup_app_state = app_state.clone();
                    let echo_app = cx.new(|cx| EchoApp::new(app_state.clone(), window, cx));
                    let root = cx.new(|cx| Root::new(echo_app, window, cx));
                    echo_gpui::diagnostics::startup_metrics::record(
                        echo_gpui::diagnostics::startup_metrics::StartupMarker::LayoutRestored,
                    );

                    if echo_gpui::bench::live_channel::requested_channel().is_some() {
                        let state = app_state.read(cx);
                        if let Err(error) =
                            echo_gpui::bench::live_channel::start_readiness_probe(
                            state.active_tab().active_channel(),
                            state.active_tab().channels(),
                        ) {
                            echo_gpui::log_app!(error, %error, "Live benchmark readiness probe did not start");
                            std::process::exit(1);
                        }
                    }

                    if !echo_gpui::bench::ui_fixture::disposable_workspace_enabled() {
                        cx.defer(move |cx| {
                            components::chat::restore_open_channel_popups(popup_app_state, cx);
                        });
                    }
                    root
                },
            ) {
                echo_gpui::log_app!(error, "Failed to open main window: {e}");
                // Without its main window Echo is unusable, and a process left
                // running would keep the single-instance lock, so every later
                // launch would only try to activate it.
                cx.quit();
            }
        });

    #[cfg(windows)]
    windows_tray::shutdown();

    if !backend::is_shutdown_started() {
        backend::shutdown_once("gpui_run_returned");
    }
    backend::reliability::mark_clean_shutdown();
}

/// Quit through GPUI on SIGTERM, SIGINT, or SIGHUP, so a session logout, a
/// `kill`, or a closed terminal runs the same settings, layout, and history
/// flush as closing the main window. A second signal during that shutdown
/// exits immediately.
#[cfg(unix)]
fn quit_on_termination_signals(cx: &mut App) {
    use tokio::signal::unix::{SignalKind, signal};

    /// A signal the launcher ignored (`nohup` ignores SIGHUP, a shell without
    /// job control ignores SIGINT for background jobs) stays ignored, because
    /// registering a handler would replace that disposition.
    fn inherited_ignore(signal_number: libc::c_int) -> bool {
        // SAFETY: a null new action only reads the current disposition into `old`.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            libc::sigaction(signal_number, std::ptr::null(), &mut old) == 0
                && old.sa_sigaction == libc::SIG_IGN
        }
    }

    let watched = [
        (SignalKind::terminate(), libc::SIGTERM, "SIGTERM", 143),
        (SignalKind::interrupt(), libc::SIGINT, "SIGINT", 130),
        (SignalKind::hangup(), libc::SIGHUP, "SIGHUP", 129),
    ];
    let (sender, receiver) = tokio::sync::oneshot::channel::<&'static str>();
    backend::bridge::get_runtime().spawn(async move {
        let mut streams = Vec::new();
        for (kind, number, name, exit_code) in watched {
            if inherited_ignore(number) {
                continue;
            }
            match signal(kind) {
                Ok(stream) => streams.push((stream, name, exit_code)),
                Err(error) => echo_gpui::log_app!(warn, "Could not watch {name}: {error}"),
            }
        }
        if streams.is_empty() {
            return;
        }
        let mut sender = Some(sender);
        loop {
            let (name, exit_code) = futures_util::future::select_all(streams.iter_mut().map(
                |(stream, name, exit_code)| {
                    Box::pin(async move {
                        stream.recv().await;
                        (*name, *exit_code)
                    })
                },
            ))
            .await
            .0;
            // The first signal quits through GPUI. A second one, or one that
            // arrives after GPUI stopped listening, ends the process at once.
            if sender
                .take()
                .is_some_and(|sender| sender.send(name).is_ok())
            {
                continue;
            }
            eprintln!("Echo received {name} during shutdown; exiting now");
            // SAFETY: _exit ends the process without atexit handlers or static
            // destructors, which could race the main thread's GPU teardown.
            unsafe { libc::_exit(exit_code) };
        }
    });
    cx.spawn(async move |cx| {
        if let Ok(name) = receiver.await {
            echo_gpui::log_app!(info, signal = name, "Termination signal received; quitting");
            cx.update(|cx| cx.quit());
        }
    })
    .detach();
}

/// mimalloc, the global allocator whenever no profiling allocator replaces it.
///
/// This is the only place that spells the predicate; everything that talks to
/// mimalloc directly lives inside it, so nothing calls into an allocator that
/// is not installed.
#[cfg(all(
    not(feature = "alloc-profile"),
    not(feature = "dhat-heap"),
    any(target_os = "linux", all(windows, feature = "windows-mimalloc"))
))]
mod allocator {
    #[global_allocator]
    static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

    /// mimalloc v3 `mi_option_page_commit_on_demand`; libmimalloc-sys has no
    /// constant for it yet.
    const MI_OPTION_PAGE_COMMIT_ON_DEMAND: libmimalloc_sys::mi_option_t = 40;

    /// Commit mimalloc page memory as it is first used instead of a whole page at
    /// a time.
    ///
    /// Same-binary stress A/Bs (2 splits, 100 msg/s) measured 50-72 MB less private
    /// commit with unchanged working set and CPU. A `MIMALLOC_PAGE_COMMIT_ON_DEMAND`
    /// environment value still wins, so the default stays easy to A/B.
    pub(crate) fn configure() {
        if std::env::var_os("MIMALLOC_PAGE_COMMIT_ON_DEMAND").is_none() {
            // SAFETY: mi_option_set only updates an option slot; it is called
            // before Echo starts any other thread.
            unsafe { libmimalloc_sys::mi_option_set(MI_OPTION_PAGE_COMMIT_ON_DEMAND, 1) };
        }
    }

    /// Start an opt-in forced mimalloc collection loop.
    ///
    /// Mimalloc already applies its own purge policy. A background forced collection
    /// can reduce retained RSS for particular long-running workloads, but it also wakes
    /// a thread and scans allocator state, so distribution builds leave it disabled
    /// unless an operator supplies a positive interval.
    pub(crate) fn start_trim_thread() {
        let Some(raw_interval) = echo_core::env::value("ECHO_MIMALLOC_TRIM_INTERVAL_SECS") else {
            return;
        };
        let Ok(interval_secs) = raw_interval.parse::<u64>() else {
            echo_gpui::log_app!(
                warn,
                "Ignoring invalid ECHO_MIMALLOC_TRIM_INTERVAL_SECS; expected a non-negative integer"
            );
            return;
        };
        if interval_secs == 0 {
            return;
        }

        match std::thread::Builder::new()
            .name("mimalloc-trim".into())
            .spawn(move || {
                loop {
                    std::thread::sleep(std::time::Duration::from_secs(interval_secs));
                    // SAFETY: mi_collect is documented as a thread-safe mimalloc API.
                    unsafe { libmimalloc_sys::mi_collect(true) };
                }
            }) {
            Ok(_) => echo_gpui::log_app!(
                info,
                interval_secs,
                "Periodic forced mimalloc collection enabled"
            ),
            Err(error) => {
                echo_gpui::log_app!(warn, %error, "Failed to start mimalloc collection thread")
            }
        }
    }
}

/// The system or a profiling allocator is installed; there is nothing to tune.
#[cfg(not(all(
    not(feature = "alloc-profile"),
    not(feature = "dhat-heap"),
    any(target_os = "linux", all(windows, feature = "windows-mimalloc"))
)))]
mod allocator {
    pub(crate) fn configure() {}
    pub(crate) fn start_trim_thread() {}
}

fn handle_version_command(args: &[String]) -> bool {
    let is_version_command = matches!(
        args,
        [arg] if matches!(arg.as_str(), "--version" | "-V" | "version" | "gpui-version" | "--gpui-version")
    ) || matches!(
        args,
        [scope, command]
            if scope.eq_ignore_ascii_case("gpui") && command.eq_ignore_ascii_case("version")
    );

    if !is_version_command {
        return false;
    }

    println!("{}", identity::VERSION_REPORT);
    true
}
