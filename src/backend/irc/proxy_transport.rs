//! Proxy-aware TLS transport for Twitch IRC.

use async_trait::async_trait;
use std::io;
use tokio_rustls::TlsConnector;
use tokio_rustls::rustls::pki_types::ServerName;
use twitch_irc::transport::tcp::{MakeConnection, TCPTransport, TCPTransportConnectError};

const TWITCH_IRC_HOST: &str = "irc.chat.twitch.tv";
const TWITCH_IRC_TLS_PORT: u16 = 6697;

pub type ProxySecureTCPTransport = TCPTransport<ProxyTlsConnection>;

pub struct ProxyTlsConnection;

#[async_trait]
impl MakeConnection for ProxyTlsConnection {
    type Socket = tokio_rustls::client::TlsStream<crate::backend::ws::ProxySocket>;

    async fn new_socket() -> Result<Self::Socket, TCPTransportConnectError> {
        let socket =
            crate::backend::ws::connect_tcp_endpoint(TWITCH_IRC_HOST, TWITCH_IRC_TLS_PORT, "IRC")
                .await
                .map_err(io::Error::other)?;

        let connector = TlsConnector::from(crate::backend::ws::native_roots_tls_config());
        let domain = ServerName::try_from(TWITCH_IRC_HOST.to_string())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;

        connector
            .connect(domain, socket)
            .await
            .map_err(TCPTransportConnectError::from)
    }
}
