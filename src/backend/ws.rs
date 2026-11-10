//! Proxy-aware WebSocket connection helpers.

use base64::{Engine as _, engine::general_purpose};
use std::io;
use std::pin::Pin;
use std::sync::{Arc, OnceLock};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpStream, lookup_host};
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use tokio_rustls::rustls::{ClientConfig, RootCertStore};
use tokio_socks::tcp::Socks5Stream;
use tokio_tungstenite::tungstenite::handshake::client::Response;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream, client_async_tls_with_config};
use url::Url;

const MAX_PROXY_CONNECT_RESPONSE_BYTES: usize = 16 * 1024;

pub type ProxyAwareWebSocket = WebSocketStream<MaybeTlsStream<ProxySocket>>;

pub enum ProxySocket {
    Tcp(TcpStream),
    Tls(Box<tokio_rustls::client::TlsStream<TcpStream>>),
    Socks5(Socks5Stream<TcpStream>),
}

impl AsyncRead for ProxySocket {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Tcp(stream) => Pin::new(stream).poll_read(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_read(cx, buf),
            Self::Socks5(stream) => Pin::new(stream).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for ProxySocket {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match &mut *self {
            Self::Tcp(stream) => Pin::new(stream).poll_write(cx, buf),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_write(cx, buf),
            Self::Socks5(stream) => Pin::new(stream).poll_write(cx, buf),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Tcp(stream) => Pin::new(stream).poll_flush(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_flush(cx),
            Self::Socks5(stream) => Pin::new(stream).poll_flush(cx),
        }
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match &mut *self {
            Self::Tcp(stream) => Pin::new(stream).poll_shutdown(cx),
            Self::Tls(stream) => Pin::new(stream.as_mut()).poll_shutdown(cx),
            Self::Socks5(stream) => Pin::new(stream).poll_shutdown(cx),
        }
    }
}

/// Upper bound for TCP/proxy connect, TLS and the WebSocket upgrade together, so
/// a stalled handshake returns an error and the caller's reconnect loop runs.
const WEBSOCKET_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub async fn connect_websocket(url: &str) -> Result<(ProxyAwareWebSocket, Response), String> {
    let parsed = Url::parse(url).map_err(|error| format!("Invalid WebSocket URL: {error}"))?;
    let connect = async {
        let socket = connect_socket(&parsed).await?;
        client_async_tls_with_config(url, socket, None, None)
            .await
            .map_err(|error| format!("WebSocket connect: {error}"))
    };
    tokio::time::timeout(WEBSOCKET_CONNECT_TIMEOUT, connect)
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "WebSocket connect timed out after {}s",
                WEBSOCKET_CONNECT_TIMEOUT.as_secs()
            ))
        })
}

async fn connect_socket(target: &Url) -> Result<ProxySocket, String> {
    let (target_host, target_port) = target_host_port(target)?;
    connect_tcp_endpoint(&target_host, target_port, "WebSocket").await
}

pub async fn connect_tcp_endpoint(
    target_host: &str,
    target_port: u16,
    service_label: &str,
) -> Result<ProxySocket, String> {
    let Some(proxy) = configured_proxy_url()? else {
        return connect_direct_endpoint(target_host, target_port, service_label).await;
    };

    match proxy.scheme() {
        "http" | "https" => connect_http_proxy_endpoint(&proxy, target_host, target_port).await,
        "socks5" => {
            connect_socks5_endpoint(&proxy, target_host, target_port, false, service_label).await
        }
        "socks5h" => {
            connect_socks5_endpoint(&proxy, target_host, target_port, true, service_label).await
        }
        scheme => Err(format!(
            "{service_label} proxy scheme '{scheme}' is not supported; use http://, https://, socks5://, or socks5h://"
        )),
    }
}

fn configured_proxy_url() -> Result<Option<Url>, String> {
    if !crate::backend::settings::get_proxy_enabled() {
        return Ok(None);
    }

    let proxy_url = crate::backend::settings::get_proxy_url();
    let trimmed = proxy_url.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }

    Url::parse(trimmed)
        .map(Some)
        .map_err(|error| format!("Invalid proxy URL: {error}"))
}

async fn connect_direct_endpoint(
    target_host: &str,
    target_port: u16,
    service_label: &str,
) -> Result<ProxySocket, String> {
    let stream = TcpStream::connect((target_host, target_port))
        .await
        .map_err(|error| {
            format!("{service_label} TCP connect to {target_host}:{target_port} failed: {error}")
        })?;
    let _ = stream.set_nodelay(true);
    Ok(ProxySocket::Tcp(stream))
}

async fn connect_http_proxy_endpoint(
    proxy: &Url,
    target_host: &str,
    target_port: u16,
) -> Result<ProxySocket, String> {
    let (proxy_host, proxy_port) = proxy_host_port(proxy)?;
    let target_authority = authority(target_host, target_port);
    let proxy_authority = authority(&proxy_host, proxy_port);

    let stream = TcpStream::connect((proxy_host.as_str(), proxy_port))
        .await
        .map_err(|error| {
            format!(
                "{} proxy TCP connect to {} failed: {}",
                proxy.scheme().to_ascii_uppercase(),
                proxy_authority,
                error
            )
        })?;
    let _ = stream.set_nodelay(true);

    let mut stream = match proxy.scheme() {
        "http" => ProxySocket::Tcp(stream),
        "https" => {
            let connector = TlsConnector::from(native_roots_tls_config());
            let domain = ServerName::try_from(proxy_host.clone()).map_err(|error| {
                format!(
                    "HTTPS proxy server name '{}' is invalid for TLS: {}",
                    proxy_host, error
                )
            })?;
            let tls_stream = connector.connect(domain, stream).await.map_err(|error| {
                format!(
                    "HTTPS proxy TLS handshake with {} failed: {}",
                    proxy_authority, error
                )
            })?;
            ProxySocket::Tls(Box::new(tls_stream))
        }
        scheme => {
            return Err(format!(
                "Unsupported CONNECT proxy scheme '{}' for {}",
                scheme, proxy_authority
            ));
        }
    };

    let mut request = format!(
        "CONNECT {target_authority} HTTP/1.1\r\nHost: {target_authority}\r\nProxy-Connection: Keep-Alive\r\n"
    );
    if !proxy.username().is_empty() {
        let password = proxy.password().unwrap_or_default();
        let token = general_purpose::STANDARD.encode(format!("{}:{password}", proxy.username()));
        request.push_str(&format!("Proxy-Authorization: Basic {token}\r\n"));
    }
    request.push_str("\r\n");

    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| {
            format!(
                "{} proxy CONNECT write to {} failed: {}",
                proxy.scheme().to_ascii_uppercase(),
                proxy_authority,
                error
            )
        })?;

    read_http_connect_response(
        &mut stream,
        &target_authority,
        &proxy_authority,
        proxy.scheme(),
    )
    .await?;
    Ok(stream)
}

async fn connect_socks5_endpoint(
    proxy: &Url,
    target_host: &str,
    target_port: u16,
    remote_dns: bool,
    service_label: &str,
) -> Result<ProxySocket, String> {
    let (proxy_host, proxy_port) = proxy_host_port(proxy)?;
    let proxy_addr = (proxy_host.as_str(), proxy_port);

    let stream = if !proxy.username().is_empty() {
        let password = proxy.password().unwrap_or_default();
        if remote_dns {
            Socks5Stream::connect_with_password(
                proxy_addr,
                (target_host.to_string(), target_port),
                proxy.username(),
                password,
            )
            .await
        } else {
            let target_addr = resolve_first_socket_addr(target_host, target_port).await?;
            Socks5Stream::connect_with_password(proxy_addr, target_addr, proxy.username(), password)
                .await
        }
    } else if remote_dns {
        Socks5Stream::connect(proxy_addr, (target_host.to_string(), target_port)).await
    } else {
        let target_addr = resolve_first_socket_addr(target_host, target_port).await?;
        Socks5Stream::connect(proxy_addr, target_addr).await
    }
    .map_err(|error| {
        format!(
            "{service_label} SOCKS5 proxy connect via {} to {} failed: {error}",
            authority(&proxy_host, proxy_port),
            authority(target_host, target_port)
        )
    })?;

    Ok(ProxySocket::Socks5(stream))
}

async fn read_http_connect_response<S>(
    stream: &mut S,
    target_authority: &str,
    proxy_authority: &str,
    proxy_scheme: &str,
) -> Result<(), String>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut response = Vec::with_capacity(1024);
    let mut chunk = [0_u8; 1024];

    loop {
        let read = stream.read(&mut chunk).await.map_err(|error| {
            format!(
                "{} proxy CONNECT response read from {} failed: {}",
                proxy_scheme.to_ascii_uppercase(),
                proxy_authority,
                error
            )
        })?;
        if read == 0 {
            return Err(format!(
                "{} proxy {} closed before CONNECT response for {} completed",
                proxy_scheme.to_ascii_uppercase(),
                proxy_authority,
                target_authority
            ));
        }

        response.extend_from_slice(&chunk[..read]);
        if response.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if response.len() > MAX_PROXY_CONNECT_RESPONSE_BYTES {
            return Err(format!(
                "{} proxy CONNECT response from {} exceeded size limit",
                proxy_scheme.to_ascii_uppercase(),
                proxy_authority
            ));
        }
    }

    let response_text = String::from_utf8_lossy(&response);
    let status_line = response_text.lines().next().unwrap_or_default();
    let status_code = status_line.split_whitespace().nth(1).unwrap_or_default();
    if status_code == "200" {
        Ok(())
    } else if status_code == "407" {
        Err(format!(
            "{} proxy authentication failed while opening tunnel to {}: {}",
            proxy_scheme.to_ascii_uppercase(),
            target_authority,
            status_line
        ))
    } else {
        Err(format!(
            "{} proxy CONNECT via {} to {} failed: {}",
            proxy_scheme.to_ascii_uppercase(),
            proxy_authority,
            target_authority,
            status_line
        ))
    }
}

async fn resolve_first_socket_addr(host: &str, port: u16) -> Result<std::net::SocketAddr, String> {
    let mut addrs = lookup_host((host, port))
        .await
        .map_err(|error| format!("Failed to resolve {host}:{port}: {error}"))?;
    addrs
        .next()
        .ok_or_else(|| format!("No resolved addresses for {host}:{port}"))
}

/// Native-roots TLS config shared by the HTTPS proxy and IRC proxy transports.
/// The root store is loaded once per process; a load that found no usable
/// roots is not cached, so the next connect retries instead of failing forever.
pub(crate) fn native_roots_tls_config() -> Arc<ClientConfig> {
    static CONFIG: OnceLock<Arc<ClientConfig>> = OnceLock::new();
    if let Some(config) = CONFIG.get() {
        return Arc::clone(config);
    }

    let mut root_store = RootCertStore::empty();
    let cert_result = rustls_native_certs::load_native_certs();
    let error_count = cert_result.errors.len();
    let (valid_count, invalid_count) = root_store.add_parsable_certificates(cert_result.certs);

    if error_count > 0 || invalid_count > 0 || valid_count == 0 {
        crate::log_net!(
            warn,
            "Loaded {} native TLS roots for proxy transport ({} load errors, {} invalid certs)",
            valid_count,
            error_count,
            invalid_count
        );
    }

    let config = Arc::new(
        ClientConfig::builder()
            .with_root_certificates(root_store)
            .with_no_client_auth(),
    );
    if valid_count == 0 {
        return config;
    }
    Arc::clone(CONFIG.get_or_init(|| config))
}

fn target_host_port(url: &Url) -> Result<(String, u16), String> {
    let host = url
        .host_str()
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| "WebSocket URL has no host".to_string())?
        .to_string();
    let port = url.port_or_known_default().ok_or_else(|| {
        format!(
            "WebSocket URL has no port default for scheme '{}'",
            url.scheme()
        )
    })?;
    Ok((host, port))
}

fn proxy_host_port(url: &Url) -> Result<(String, u16), String> {
    let host = url
        .host_str()
        .filter(|host| !host.trim().is_empty())
        .ok_or_else(|| "Proxy URL has no host".to_string())?
        .to_string();
    let port = url
        .port()
        .or_else(|| proxy_default_port(url.scheme()))
        .ok_or_else(|| {
            format!(
                "Proxy URL has no explicit port and no default for scheme '{}'",
                url.scheme()
            )
        })?;
    Ok((host, port))
}

fn proxy_default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" => Some(80),
        "https" => Some(443),
        "socks5" | "socks5h" => Some(1080),
        _ => None,
    }
}

fn authority(host: &str, port: u16) -> String {
    if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}
