//! TLS + GIOP transport to a FileMaker Server over port 5003.
//!
//! The server requires a client certificate (mutual TLS); a throwaway
//! self-signed one passes the gate (spec F10/F11). Application callers use
//! [`Transport::connect_verified`] for certificate-chain and hostname checks.
//! [`Transport::connect`] preserves the explicitly permissive RE CLI transport.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{
    ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore, SignatureScheme,
    StreamOwned,
};

use om_giop::{MessageReader, ReadError};

/// Largest GIOP message this transport will buffer. A single FileMaker reply
/// fragment is well under this; larger logical replies arrive as fragments.
/// Caps memory against a peer advertising a huge body size (64 MiB).
pub const MAX_MESSAGE_LEN: usize = 64 * 1024 * 1024;

/// How long a read waits for the server by default.
pub const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// How long writing a message may wait for the host to take it.
pub const WRITE_TIMEOUT: Duration = Duration::from_secs(30);

/// A server-certificate verifier that accepts any chain (lab use) while still
/// delegating signature checks to the crypto provider.
#[derive(Debug)]
struct AcceptServerCert {
    provider: Arc<CryptoProvider>,
}

impl ServerCertVerifier for AcceptServerCert {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        eprintln!(
            "[tls] accepting server cert for {:?} ({} bytes), lab client",
            server_name,
            end_entity.len()
        );
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// Errors establishing or using the transport.
#[derive(Debug)]
pub enum TransportError {
    Io(std::io::Error),
    Tls(rustls::Error),
    Pem(String),
    /// Reading a GIOP message failed: closed, oversized (over
    /// [`MAX_MESSAGE_LEN`]), or malformed.
    Read(ReadError),
}

impl TransportError {
    /// Whether a read gave up waiting: the socket's read timeout passed with
    /// nothing to read. Nothing is lost; reading again goes on.
    pub fn is_timeout(&self) -> bool {
        match self {
            TransportError::Io(e) | TransportError::Read(ReadError::Io(e)) => matches!(
                e.kind(),
                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
            ),
            _ => false,
        }
    }
}

impl From<std::io::Error> for TransportError {
    fn from(e: std::io::Error) -> Self {
        TransportError::Io(e)
    }
}
impl From<rustls::Error> for TransportError {
    fn from(e: rustls::Error) -> Self {
        TransportError::Tls(e)
    }
}
impl From<ReadError> for TransportError {
    fn from(e: ReadError) -> Self {
        TransportError::Read(e)
    }
}

impl std::fmt::Display for TransportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            TransportError::Io(e) => write!(f, "io: {e}"),
            TransportError::Tls(e) => write!(f, "tls: {e}"),
            TransportError::Pem(e) => write!(f, "pem: {e}"),
            TransportError::Read(e) => write!(f, "{e}"),
        }
    }
}
impl std::error::Error for TransportError {}

/// A synchronous TLS + GIOP connection.
pub struct Transport {
    stream: StreamOwned<ClientConnection, Socket>,
    reader: MessageReader,
}

/// The TCP socket under the TLS stream. While a deadline is set, each read
/// of the socket waits only for the time left: TLS may read the socket
/// several times for one record, and none of those reads goes past it.
struct Socket {
    tcp: TcpStream,
    deadline: Option<Instant>,
    /// While set, each write of the socket waits only for the time left
    /// ([`Transport::set_write_deadline`]).
    write_deadline: Option<Instant>,
}

impl Socket {
    /// The time left before the write deadline, as the socket's write
    /// timeout; none left is a timeout.
    fn bound_write(&mut self) -> std::io::Result<()> {
        if let Some(deadline) = self.write_deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            self.tcp.set_write_timeout(Some(left))?;
        }
        Ok(())
    }
}

impl Read for Socket {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if let Some(deadline) = self.deadline {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            self.tcp.set_read_timeout(Some(left))?;
        }
        self.tcp.read(buf)
    }
}

impl Write for Socket {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bound_write()?;
        self.tcp.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.bound_write()?;
        self.tcp.flush()
    }
}

impl Transport {
    /// Lab-only connection that accepts untrusted server certificates.
    /// Application callers should use [`Self::connect_verified`].
    pub fn connect(
        host: &str,
        port: u16,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
    ) -> Result<Self, TransportError> {
        let provider = Arc::new(ring::default_provider());

        let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut &client_cert_pem[..])
            .collect::<Result<_, _>>()
            .map_err(|e| TransportError::Pem(e.to_string()))?;
        let key: PrivateKeyDer<'static> = rustls_pemfile::private_key(&mut &client_key_pem[..])
            .map_err(|e| TransportError::Pem(e.to_string()))?
            .ok_or_else(|| TransportError::Pem("no private key in PEM".into()))?;

        let verifier = Arc::new(AcceptServerCert {
            provider: provider.clone(),
        });
        // Load the key directly rather than through `with_client_auth_cert`,
        // whose key-matches-certificate check parses the certificate with
        // webpki and so refuses X.509 version 1 certificates, the kind
        // FileMaker Pro itself presents.
        let signing_key = provider
            .key_provider
            .load_private_key(key)
            .map_err(TransportError::Tls)?;
        let client_cert = SingleCertAndKey::from(CertifiedKey::new(certs, signing_key));
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TransportError::Tls)?
            .dangerous()
            .with_custom_certificate_verifier(verifier)
            .with_client_cert_resolver(Arc::new(client_cert));
        Self::connect_config(host, port, config)
    }

    /// Connect with normal chain and hostname validation. The public Web PKI
    /// root set is extended only by explicitly supplied CA certificates.
    pub fn connect_verified(
        host: &str,
        port: u16,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        additional_ca_pem: &[u8],
    ) -> Result<Self, TransportError> {
        let provider = Arc::new(ring::default_provider());
        let certs = rustls_pemfile::certs(&mut &client_cert_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| TransportError::Pem(e.to_string()))?;
        let key = rustls_pemfile::private_key(&mut &client_key_pem[..])
            .map_err(|e| TransportError::Pem(e.to_string()))?
            .ok_or_else(|| TransportError::Pem("no private key in PEM".into()))?;
        let mut roots = RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        for cert in rustls_pemfile::certs(&mut &additional_ca_pem[..]) {
            roots
                .add(cert.map_err(|e| TransportError::Pem(e.to_string()))?)
                .map_err(TransportError::Tls)?;
        }
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TransportError::Tls)?
            .with_root_certificates(roots)
            .with_client_auth_cert(certs, key)
            .map_err(TransportError::Tls)?;
        Self::connect_config(host, port, config)
    }

    fn connect_config(host: &str, port: u16, config: ClientConfig) -> Result<Self, TransportError> {
        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|_| TransportError::Pem(format!("invalid server name {host}")))?;
        let conn = ClientConnection::new(Arc::new(config), server_name)?;

        let tcp = TcpStream::connect((host, port))?;
        tcp.set_read_timeout(Some(READ_TIMEOUT))?;
        tcp.set_write_timeout(Some(WRITE_TIMEOUT))?;
        let socket = Socket {
            tcp,
            deadline: None,
            write_deadline: None,
        };
        let mut stream = StreamOwned::new(conn, socket);
        // Force the handshake now so cert problems surface here.
        stream.flush()?;

        Ok(Self {
            stream,
            reader: MessageReader::new(MAX_MESSAGE_LEN),
        })
    }

    /// Negotiated TLS protocol version as a string, once handshaked.
    pub fn tls_version(&self) -> Option<String> {
        self.stream
            .conn
            .protocol_version()
            .map(|v| format!("{v:?}"))
    }

    /// The connection's local address: the client's address as the server
    /// sees it.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        Ok(self.stream.sock.tcp.local_addr()?)
    }

    /// How long a read waits for the server; `None` waits indefinitely (30
    /// seconds are set when connecting).
    pub fn set_read_timeout(&mut self, timeout: Option<Duration>) -> Result<(), TransportError> {
        self.stream.sock.tcp.set_read_timeout(timeout)?;
        Ok(())
    }

    /// Until it is cleared, every write of the socket, however TLS splits
    /// a message, ends by `deadline`; cleared, writes wait
    /// [`WRITE_TIMEOUT`] again. A write that times out leaves the stream
    /// unusable: its caller drops the connection.
    pub fn set_write_deadline(&mut self, deadline: Option<Instant>) -> Result<(), TransportError> {
        self.stream.sock.write_deadline = deadline;
        if deadline.is_none() {
            self.stream
                .sock
                .tcp
                .set_write_timeout(Some(WRITE_TIMEOUT))?;
        }
        Ok(())
    }

    /// Send one already-framed GIOP message.
    pub fn send(&mut self, message: &[u8]) -> Result<(), TransportError> {
        self.stream.write_all(message)?;
        self.stream.flush()?;
        Ok(())
    }

    /// End the TLS session cleanly (close_notify), then shut the socket.
    pub fn close(mut self) -> Result<(), TransportError> {
        self.stream.conn.send_close_notify();
        self.stream.flush()?;
        let _ = self.stream.sock.tcp.shutdown(std::net::Shutdown::Both);
        Ok(())
    }

    /// Read exactly one GIOP message (one fragment, if fragmented).
    pub fn recv(&mut self) -> Result<Vec<u8>, TransportError> {
        Ok(self.reader.read_message(&mut self.stream)?)
    }

    /// Read one logical GIOP message, joining 1.2 fragments.
    pub fn recv_logical(&mut self) -> Result<Vec<u8>, TransportError> {
        Ok(self.reader.read_logical(&mut self.stream)?)
    }

    /// [`Transport::recv_logical`] that gives up at `deadline`, however the
    /// message arrives: every read of the socket waits only for what is
    /// left, so a peer sending fragments, or the bytes of one TLS record,
    /// slowly cannot stretch it. A timeout loses nothing (see
    /// [`TransportError::is_timeout`]). The read timeout is `timeout` again
    /// afterwards.
    pub fn recv_logical_by(
        &mut self,
        deadline: Instant,
        timeout: Option<Duration>,
    ) -> Result<Vec<u8>, TransportError> {
        self.stream.sock.deadline = Some(deadline);
        let read = self.reader.read_logical(&mut self.stream);
        self.stream.sock.deadline = None;
        self.set_read_timeout(timeout)?;
        Ok(read?)
    }
}
