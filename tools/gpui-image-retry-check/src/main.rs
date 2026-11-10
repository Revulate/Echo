//! Runtime: GPUI's image asset loader against a loopback HTTP fixture. A cluster of
//! truncated successful bodies is retried until every asset loads, while HTTP 404
//! and corrupt images fail after one request each.

use gpui::{App, Asset, ImageAssetLoader, ImageCacheError, Resource};
use reqwest_client::ReqwestClient;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CLUSTER_ASSET_COUNT: usize = 20;

const ONE_PIXEL_PNG: &[u8] = &[
    0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52,
    0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x04, 0x00, 0x00, 0x00, 0xB5, 0x1C, 0x0C,
    0x02, 0x00, 0x00, 0x00, 0x0B, 0x49, 0x44, 0x41, 0x54, 0x78, 0xDA, 0x63, 0x64, 0xF8, 0x0F, 0x00,
    0x01, 0x05, 0x01, 0x01, 0x27, 0x18, 0xE3, 0x66, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4E, 0x44,
    0xAE, 0x42, 0x60, 0x82,
];

#[derive(Default)]
struct RequestCounts {
    flaky: AtomicUsize,
    flaky_by_path: Mutex<HashMap<String, usize>>,
    missing: AtomicUsize,
    corrupt: AtomicUsize,
}

fn main() {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback fixture");
    listener
        .set_nonblocking(true)
        .expect("set loopback fixture nonblocking");
    let address = listener.local_addr().expect("fixture address");
    let counts = Arc::new(RequestCounts::default());
    let server_stop = Arc::new(AtomicBool::new(false));
    let server = {
        let counts = Arc::clone(&counts);
        let server_stop = Arc::clone(&server_stop);
        std::thread::spawn(move || run_fixture_server(listener, counts, server_stop))
    };

    let completed = Arc::new(AtomicBool::new(false));
    let outcome = Arc::new(AtomicBool::new(false));
    let completed_for_app = Arc::clone(&completed);
    let outcome_for_app = Arc::clone(&outcome);
    let counts_for_app = Arc::clone(&counts);
    let server_stop_for_app = Arc::clone(&server_stop);
    let missing_uri = format!("http://{address}/missing.png");
    let corrupt_uri = format!("http://{address}/corrupt.png");

    gpui_platform::application()
        .with_assets(())
        .run(move |cx: &mut App| {
            // GPUI polls image bodies on its own executor. Keep the idle-read
            // timeout enabled here so this non-test fixture also proves that
            // the reqwest response reader re-enters its owning Tokio runtime
            // instead of panicking when it lazily constructs the timeout.
            let client = ReqwestClient::proxy_user_agent_and_read_timeout(
                None,
                "Echo GPUI image retry checker",
                Some(Duration::from_secs(2)),
            )
            .expect("build fixture HTTP client");
            cx.set_http_client(Arc::new(client));

            let mut flaky = Vec::with_capacity(CLUSTER_ASSET_COUNT);
            for index in 0..CLUSTER_ASSET_COUNT {
                let uri = format!("http://{address}/flaky-{index}.png");
                flaky.push(ImageAssetLoader::load(Resource::Uri(uri.into()), cx));
            }
            let missing = ImageAssetLoader::load(Resource::Uri(missing_uri.into()), cx);
            let corrupt = ImageAssetLoader::load(Resource::Uri(corrupt_uri.into()), cx);
            cx.spawn(async move |cx| {
                let flaky_recovered_count = futures::future::join_all(flaky)
                    .await
                    .into_iter()
                    .filter(|result| result.is_ok())
                    .count();
                let flaky_recovered = flaky_recovered_count == CLUSTER_ASSET_COUNT;
                let missing_stayed_terminal =
                    matches!(missing.await, Err(ImageCacheError::BadStatus { .. }));
                let corrupt_stayed_terminal = corrupt.await.is_err();
                let flaky_requests = counts_for_app.flaky.load(Ordering::Acquire);
                let flaky_paths = counts_for_app
                    .flaky_by_path
                    .lock()
                    .expect("flaky request counts");
                let cluster_retried_once = flaky_paths.len() == CLUSTER_ASSET_COUNT
                    && flaky_paths.values().all(|attempts| *attempts == 2);
                let missing_requests = counts_for_app.missing.load(Ordering::Acquire);
                let corrupt_requests = counts_for_app.corrupt.load(Ordering::Acquire);
                let passed = flaky_recovered
                    && missing_stayed_terminal
                    && corrupt_stayed_terminal
                    && cluster_retried_once
                    && flaky_requests == CLUSTER_ASSET_COUNT * 2
                    && missing_requests == 1
                    && corrupt_requests == 1;
                outcome_for_app.store(passed, Ordering::Release);
                completed_for_app.store(true, Ordering::Release);
                server_stop_for_app.store(true, Ordering::Release);
                cx.update(|cx| cx.quit());
            })
            .detach();
        });

    server_stop.store(true, Ordering::Release);
    server.join().expect("loopback fixture server");

    let flaky_requests = counts.flaky.load(Ordering::Acquire);
    let missing_requests = counts.missing.load(Ordering::Acquire);
    let corrupt_requests = counts.corrupt.load(Ordering::Acquire);
    let passed = completed.load(Ordering::Acquire)
        && outcome.load(Ordering::Acquire)
        && flaky_requests == CLUSTER_ASSET_COUNT * 2
        && missing_requests == 1
        && corrupt_requests == 1;

    println!("scenario,requests,result");
    println!(
        "clustered_truncated_success_bodies,{flaky_requests},{CLUSTER_ASSET_COUNT}/{CLUSTER_ASSET_COUNT}_recovered"
    );
    println!("http_404,{missing_requests},terminal");
    println!("corrupt_image,{corrupt_requests},terminal");
    println!(
        "decision,{},{}",
        u8::from(passed),
        if passed { "pass" } else { "fail" }
    );

    if !passed {
        std::process::exit(1);
    }
}

fn run_fixture_server(listener: TcpListener, counts: Arc<RequestCounts>, stop: Arc<AtomicBool>) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !stop.load(Ordering::Acquire) && Instant::now() < deadline {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_nonblocking(false)
                    .expect("set fixture stream blocking");
                respond(&mut stream, &counts);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => panic!("loopback fixture accept failed: {error}"),
        };
    }
}

fn respond(stream: &mut TcpStream, counts: &RequestCounts) {
    stream
        .set_read_timeout(Some(Duration::from_secs(2)))
        .expect("set fixture read timeout");
    let mut request = Vec::with_capacity(1024);
    let mut chunk = [0u8; 512];
    while !request.windows(4).any(|window| window == b"\r\n\r\n") {
        let read = stream.read(&mut chunk).expect("read fixture request");
        if read == 0 {
            break;
        }
        request.extend_from_slice(&chunk[..read]);
        if request.len() > 8192 {
            panic!("fixture request header exceeded bound");
        }
    }

    let request_line = String::from_utf8_lossy(&request);
    let path = request_line
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("/");
    match path {
        path if path.starts_with("/flaky-") && path.ends_with(".png") => {
            counts.flaky.fetch_add(1, Ordering::AcqRel);
            let attempt = {
                let mut by_path = counts.flaky_by_path.lock().expect("flaky request counts");
                let attempts = by_path.entry(path.to_string()).or_default();
                *attempts += 1;
                *attempts
            };
            if attempt == 1 {
                write_header(stream, "200 OK", "image/png", ONE_PIXEL_PNG.len());
                stream
                    .write_all(&ONE_PIXEL_PNG[..ONE_PIXEL_PNG.len() / 2])
                    .expect("write truncated fixture body");
            } else {
                write_response(stream, "200 OK", "image/png", ONE_PIXEL_PNG);
            }
        }
        "/missing.png" => {
            counts.missing.fetch_add(1, Ordering::AcqRel);
            write_response(stream, "404 Not Found", "text/plain", b"not found\n");
        }
        "/corrupt.png" => {
            counts.corrupt.fetch_add(1, Ordering::AcqRel);
            write_response(stream, "200 OK", "image/png", b"not an image");
        }
        _ => write_response(stream, "400 Bad Request", "text/plain", b"bad request\n"),
    }
}

fn write_header(stream: &mut TcpStream, status: &str, content_type: &str, length: usize) {
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {length}\r\nConnection: close\r\n\r\n"
    )
    .expect("write fixture response header");
}

fn write_response(stream: &mut TcpStream, status: &str, content_type: &str, body: &[u8]) {
    write_header(stream, status, content_type, body.len());
    stream.write_all(body).expect("write fixture response body");
}
