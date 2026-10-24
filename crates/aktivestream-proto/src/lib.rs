//! Phase 2 native transport for the AktiveStream client: a QUIC/TLS connection to the Swift host
//! agent, with trust-on-first-use certificate pinning. See `docs/phase2-protocol.md`.
//!
//! Checkpoint 1 scope: connect, negotiate `astream/1`, learn and pin the host's self-signed
//! certificate fingerprint. Message framing and the streams build on top of [`Connection`].

pub mod wire;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use wire::{Reader, Writer, kind};

use quinn::crypto::rustls::QuicClientConfig;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{CryptoProvider, verify_tls12_signature, verify_tls13_signature};
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use sha2::{Digest, Sha256};

pub const ALPN: &[u8] = b"astream/1";
pub const DEFAULT_PORT: u16 = 35390;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("i/o: {0}")]
    Io(#[from] std::io::Error),
    #[error("connect: {0}")]
    Connect(#[from] quinn::ConnectError),
    #[error("connection: {0}")]
    Connection(#[from] quinn::ConnectionError),
    #[error("tls: {0}")]
    Tls(#[from] rustls::Error),
    #[error("quic tls config: {0}")]
    QuicConfig(String),
    #[error("certificate fingerprint mismatch: expected {expected}, got {got}")]
    FingerprintMismatch { expected: String, got: String },
    #[error("the host presented no certificate")]
    NoCertificate,
    #[error("protocol: {0}")]
    Protocol(String),
    #[error("host refused: {0}")]
    HostError(String),
}

pub type Result<T> = std::result::Result<T, Error>;

/// Lowercase hex SHA-256 of a DER certificate (the pinned fingerprint).
pub fn fingerprint(cert: &CertificateDer<'_>) -> String {
    let digest = Sha256::digest(cert.as_ref());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// A rustls verifier that trusts whatever certificate the host presents, records its
/// fingerprint, and (when a pin is set) requires the fingerprint to match. TLS 1.3 still
/// authenticates that the host holds the matching private key; TOFU replaces CA validation,
/// exactly like SSH known_hosts.
#[derive(Debug)]
struct PinVerifier {
    provider: Arc<CryptoProvider>,
    expected: Option<String>,
    seen: Mutex<Option<String>>,
}

impl ServerCertVerifier for PinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        let got = fingerprint(end_entity);
        if let Some(expected) = &self.expected
            && !expected.eq_ignore_ascii_case(&got)
        {
            return Err(rustls::Error::General(format!("certificate fingerprint mismatch: {got}")));
        }
        *self.seen.lock().unwrap() = Some(got);
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.provider.signature_verification_algorithms)
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider.signature_verification_algorithms.supported_schemes()
    }
}

/// What the host reports about itself after the handshake.
#[derive(Debug, Clone)]
pub struct HostInfo {
    pub proto_version: u32,
    pub name: String,
    pub display_count: u8,
}

/// A raw test frame (checkpoint 1b), RGBA8 top-to-bottom.
#[derive(Debug, Clone)]
pub struct TestFrame {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// A live connection to the host, plus the fingerprint it presented.
pub struct Connection {
    pub conn: quinn::Connection,
    pub fingerprint: String,
    endpoint: quinn::Endpoint,
}

impl Connection {
    /// Opens the control stream and runs the handshake: Hello -> HelloOk, Auth -> AuthOk.
    /// `password` is sent as the auth token (checkpoint 1b: the host accepts any; real
    /// validation against the macOS account with TLS channel binding is a later step).
    pub async fn control(&self, client_name: &str, password: &str) -> Result<Control> {
        let (mut send, mut recv) = self.conn.open_bi().await?;

        let mut hello = Writer::new();
        hello.u32(wire::PROTO_VERSION).short_string(client_name);
        wire::write_msg(&mut send, kind::HELLO, hello.finish()).await?;

        let (k, body) = wire::read_msg(&mut recv).await?;
        let info = match k {
            kind::HELLO_OK => {
                let mut r = Reader::new(&body);
                HostInfo { proto_version: r.u32()?, name: r.short_string()?, display_count: r.u8()? }
            }
            kind::ERROR => return Err(Error::HostError(error_text(&body))),
            other => return Err(Error::Protocol(format!("expected HelloOk, got kind {other}"))),
        };

        let mut auth = Writer::new();
        auth.u16(password.len().min(u16::MAX as usize) as u16).bytes(password.as_bytes());
        wire::write_msg(&mut send, kind::AUTH, auth.finish()).await?;

        let (k, body) = wire::read_msg(&mut recv).await?;
        match k {
            kind::AUTH_OK => {}
            kind::ERROR => return Err(Error::HostError(error_text(&body))),
            other => return Err(Error::Protocol(format!("expected AuthOk, got kind {other}"))),
        }
        Ok(Control { send, recv, info })
    }

    /// Gracefully closes the connection and waits for the endpoint to drain.
    pub async fn close(self) {
        self.conn.close(0u32.into(), b"bye");
        self.endpoint.wait_idle().await;
    }
}

fn error_text(body: &[u8]) -> String {
    let mut r = Reader::new(body);
    r.u16().map(|_| String::from_utf8_lossy(r.rest()).into_owned()).unwrap_or_default()
}

/// The control stream after a successful handshake.
/// Start of a video stream: the codec and dimensions the client needs for its decoder.
#[derive(Debug, Clone, Copy)]
pub struct VideoStart {
    pub width: u32,
    pub height: u32,
    /// 0 = HEVC, 1 = H.264.
    pub codec: u8,
}

/// One encoded video frame (Annex B).
#[derive(Debug, Clone)]
pub struct VideoFrame {
    pub keyframe: bool,
    pub data: Vec<u8>,
}

/// Client -> host input (native path). Pointer coordinates are 0..1 of the streamed display.
#[derive(Debug, Clone, Copy)]
pub enum Input {
    Key { keycode: u16, down: bool, flags: u32 },
    Pointer { buttons: u8, x: f32, y: f32 },
    Scroll { dx: f32, dy: f32 },
}

impl Input {
    fn encode(&self) -> (u8, Vec<u8>) {
        let mut w = Writer::new();
        match *self {
            Input::Key { keycode, down, flags } => {
                w.u16(keycode).u8(down as u8).u32(flags);
                (kind::INPUT_KEY, w.finish().to_vec())
            }
            Input::Pointer { buttons, x, y } => {
                w.u8(buttons).f32(x).f32(y);
                (kind::INPUT_POINTER, w.finish().to_vec())
            }
            Input::Scroll { dx, dy } => {
                w.f32(dx).f32(dy);
                (kind::INPUT_SCROLL, w.finish().to_vec())
            }
        }
    }
}

/// Writes one input message to the control stream's send half.
pub async fn write_input(send: &mut quinn::SendStream, input: &Input) -> Result<()> {
    let (kind, body) = input.encode();
    wire::write_msg(send, kind, &body).await
}

/// A message read from the control stream.
#[derive(Debug)]
pub enum Message {
    VideoStart(VideoStart),
    VideoFrame(VideoFrame),
    TestFrame(TestFrame),
    Error(String),
    /// Any other kind, passed through as (kind, body).
    Other(u8, Vec<u8>),
}

/// The control stream after a successful handshake.
pub struct Control {
    send: quinn::SendStream,
    recv: quinn::RecvStream,
    pub info: HostInfo,
}

/// Parses one framed control message from its kind and body.
fn decode_message(k: u8, body: Vec<u8>) -> Result<Message> {
    let mut r = Reader::new(&body);
    Ok(match k {
        kind::VIDEO_START => Message::VideoStart(VideoStart { width: r.u32()?, height: r.u32()?, codec: r.u8()? }),
        kind::VIDEO_FRAME => {
            let keyframe = r.u8()? != 0;
            Message::VideoFrame(VideoFrame { keyframe, data: r.rest().to_vec() })
        }
        kind::TEST_FRAME => {
            let width = r.u32()?;
            let height = r.u32()?;
            Message::TestFrame(TestFrame { width, height, rgba: r.rest().to_vec() })
        }
        kind::ERROR => Message::Error(error_text(&body)),
        other => Message::Other(other, body),
    })
}

/// Reads and parses the next control message from a receive stream, or `None` on clean EOF.
pub async fn read_message(recv: &mut quinn::RecvStream) -> Result<Option<Message>> {
    match wire::read_msg_opt(recv).await? {
        Some((k, body)) => Ok(Some(decode_message(k, body)?)),
        None => Ok(None),
    }
}

impl Control {
    /// Reads the next control message, or `None` when the host closed the stream.
    pub async fn recv(&mut self) -> Result<Option<Message>> {
        read_message(&mut self.recv).await
    }

    /// Reads the next control message as a test frame (checkpoint 1b).
    pub async fn recv_test_frame(&mut self) -> Result<TestFrame> {
        match self.recv().await? {
            Some(Message::TestFrame(frame)) => Ok(frame),
            Some(other) => Err(Error::Protocol(format!("expected TestFrame, got {other:?}"))),
            None => Err(Error::Protocol("stream ended before a test frame".into())),
        }
    }

    /// Keep the writer reachable for later input/clipboard messages.
    pub fn sender(&mut self) -> &mut quinn::SendStream {
        &mut self.send
    }

    /// Splits into the raw halves so a client can read video and send input concurrently.
    pub fn into_parts(self) -> (HostInfo, quinn::SendStream, quinn::RecvStream) {
        (self.info, self.send, self.recv)
    }
}

/// Connects to the host over QUIC. `expected_fingerprint` is the pin from a previous connect
/// (first-ever connect passes `None` and reads [`Connection::fingerprint`] to store).
pub async fn connect(addr: SocketAddr, expected_fingerprint: Option<String>) -> Result<Connection> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(PinVerifier {
        provider: provider.clone(),
        expected: expected_fingerprint.clone(),
        seen: Mutex::new(None),
    });
    let mut crypto = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("TLS 1.3 is supported by the ring provider")
        .dangerous()
        .with_custom_certificate_verifier(verifier.clone())
        .with_no_client_auth();
    crypto.alpn_protocols = vec![ALPN.to_vec()];

    let quic_crypto = QuicClientConfig::try_from(crypto).map_err(|e| Error::QuicConfig(e.to_string()))?;
    let mut client_config = quinn::ClientConfig::new(Arc::new(quic_crypto));
    // Keep the connection alive while the remote screen is static: ScreenCaptureKit emits no frames
    // when nothing changes, so without a periodic keep-alive the QUIC idle timeout fires and
    // abruptly drops the session. Ping every 5s, well under the 30s idle timeout.
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(std::time::Duration::from_secs(5)));
    if let Ok(idle) = quinn::IdleTimeout::try_from(std::time::Duration::from_secs(30)) {
        transport.max_idle_timeout(Some(idle));
    }
    client_config.transport_config(Arc::new(transport));
    let mut endpoint = quinn::Endpoint::client((std::net::Ipv4Addr::UNSPECIFIED, 0).into())?;
    endpoint.set_default_client_config(client_config);

    // SNI/server name is cosmetic under TOFU; the cert CN is "AktiveStream Host".
    let conn = endpoint.connect(addr, "aktivestream-host")?.await?;

    let got = verifier.seen.lock().unwrap().clone().ok_or(Error::NoCertificate)?;
    if let Some(expected) = expected_fingerprint
        && !expected.eq_ignore_ascii_case(&got)
    {
        return Err(Error::FingerprintMismatch { expected, got });
    }
    Ok(Connection { conn, fingerprint: got, endpoint })
}
