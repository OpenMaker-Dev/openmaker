//! TLS + GIOP transport to a FileMaker Server over port 5003.
//!
//! The server requires a client certificate (mutual TLS); a throwaway
//! self-signed one passes the gate (spec F10/F11). Application callers use
//! [`Transport::connect_verified`] for certificate-chain and hostname checks,
//! or [`Transport::connect_trusting`], which also accepts a certificate the
//! user permitted by its fingerprint. A certificate that cannot be verified
//! comes back as [`TransportError::Unverified`], with what it says, so the
//! user can be asked. [`Transport::connect`] preserves the explicitly
//! permissive RE CLI transport.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::client::WebPkiServerVerifier;
use rustls::crypto::{
    ring, verify_tls12_signature, verify_tls13_signature, CryptoProvider, WebPkiSupportedAlgorithms,
};
use rustls::pki_types::{
    CertificateDer, PrivateKeyDer, ServerName, SubjectPublicKeyInfoDer, UnixTime,
};
use rustls::sign::{CertifiedKey, SingleCertAndKey};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore,
    SignatureScheme, StreamOwned,
};

use om_giop::{MessageReader, ReadError};

use crate::cert;

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

/// Verifies a server's certificate against the trusted roots, and else
/// accepts one whose SHA-256 fingerprint the user permitted for this host:
/// its handshake signature is then checked as rustls checks one for the
/// TLS version, with the public key read from the certificate itself
/// (`cert`, which also reads X.509 version 1). It keeps what it decided for
/// the transport. One per connection.
#[derive(Debug)]
struct HostVerifier {
    roots: Arc<WebPkiServerVerifier>,
    algorithms: WebPkiSupportedAlgorithms,
    permitted: Vec<[u8; 32]>,
    decision: Mutex<Option<Decision>>,
}

#[derive(Debug)]
enum Decision {
    Verified,
    Permitted,
    Refused(Box<UnverifiedCertificate>),
}

impl HostVerifier {
    fn decide(&self, decision: Decision) {
        *self.decision.lock().unwrap_or_else(PoisonError::into_inner) = Some(decision);
    }

    fn take(&self) -> Option<Decision> {
        self.decision
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
    }

    /// Whether `cert` is the permitted certificate this connection accepted.
    fn accepted_permitted(&self, cert: &CertificateDer<'_>) -> bool {
        matches!(
            *self.decision.lock().unwrap_or_else(PoisonError::into_inner),
            Some(Decision::Permitted)
        ) && self.permitted.contains(&cert::sha256(cert))
    }

    /// A permitted certificate's handshake signature, checked with its own
    /// public key under rustls's rules for the version: in TLS 1.3 the one
    /// algorithm the scheme names, in TLS 1.2 each it may stand for.
    fn permitted_signature(
        &self,
        tls13: bool,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        let bad = rustls::Error::InvalidCertificate;
        let spki = SubjectPublicKeyInfoDer::from(
            cert::parse(cert)
                .ok_or(bad(CertificateError::BadEncoding))?
                .spki,
        );
        if tls13 {
            return rustls::crypto::verify_tls13_signature_with_raw_key(
                message,
                &spki,
                dss,
                &self.algorithms,
            );
        }
        let key = webpki::RawPublicKeyEntity::try_from(&spki)
            .map_err(|_| bad(CertificateError::BadEncoding))?;
        let candidates = self
            .algorithms
            .mapping
            .iter()
            .find(|(scheme, _)| *scheme == dss.scheme)
            .map(|(_, algorithms)| *algorithms)
            .ok_or(rustls::Error::PeerMisbehaved(
                rustls::PeerMisbehaved::SignedHandshakeWithUnadvertisedSigScheme,
            ))?;
        if candidates.iter().any(|algorithm| {
            key.verify_signature(*algorithm, message, dss.signature())
                .is_ok()
        }) {
            Ok(HandshakeSignatureValid::assertion())
        } else {
            Err(bad(CertificateError::BadSignature))
        }
    }
}

/// Whether a certificate's public key reads as one whose signatures can be
/// checked: only such a certificate can be permitted.
fn has_usable_key(parsed: Option<&cert::Certificate<'_>>) -> bool {
    parsed.is_some_and(|c| {
        webpki::RawPublicKeyEntity::try_from(&SubjectPublicKeyInfoDer::from(c.spki)).is_ok()
    })
}

impl ServerCertVerifier for HostVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        server_name: &ServerName<'_>,
        ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        let error = match self.roots.verify_server_cert(
            end_entity,
            intermediates,
            server_name,
            ocsp_response,
            now,
        ) {
            Ok(verified) => {
                self.decide(Decision::Verified);
                return Ok(verified);
            }
            Err(error) => error,
        };
        let fingerprint = cert::sha256(end_entity);
        let parsed = cert::parse(end_entity);
        // A certificate whose key cannot be read cannot be checked once
        // permitted either: it fails as it is, and nothing asks about it.
        if !has_usable_key(parsed.as_ref()) {
            return Err(error);
        }
        if self.permitted.contains(&fingerprint) {
            self.decide(Decision::Permitted);
            return Ok(ServerCertVerified::assertion());
        }
        // Not one permitted: changed if another was.
        self.decide(Decision::Refused(Box::new(unverified(
            &error,
            fingerprint,
            parsed,
            !self.permitted.is_empty(),
        ))));
        Err(error)
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        if self.accepted_permitted(cert) {
            return self.permitted_signature(false, message, cert, dss);
        }
        self.roots.verify_tls12_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        if self.accepted_permitted(cert) {
            return self.permitted_signature(true, message, cert, dss);
        }
        self.roots.verify_tls13_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.roots.supported_verify_schemes()
    }
}

/// What a user is told of a certificate the roots refused with `error`.
fn unverified(
    error: &rustls::Error,
    fingerprint: [u8; 32],
    parsed: Option<cert::Certificate<'_>>,
    changed: bool,
) -> UnverifiedCertificate {
    let problem = match error {
        // webpki reads only version 3; it refuses the others whatever else
        // is wrong with them.
        _ if parsed.is_some_and(|c| c.version < 3) => CertificateProblem::OldFormat,
        rustls::Error::InvalidCertificate(error) => match error {
            CertificateError::UnknownIssuer => CertificateProblem::UnknownIssuer {
                self_signed: parsed.is_some_and(|c| c.issuer == c.subject),
            },
            CertificateError::Expired | CertificateError::ExpiredContext { .. } => {
                CertificateProblem::Expired
            }
            CertificateError::NotValidYet | CertificateError::NotValidYetContext { .. } => {
                CertificateProblem::NotYetValid
            }
            CertificateError::NotValidForName | CertificateError::NotValidForNameContext { .. } => {
                CertificateProblem::WrongName
            }
            _ => CertificateProblem::Other,
        },
        _ => CertificateProblem::Other,
    };
    let (subject, issuer) = parsed.map_or_else(Default::default, |c| {
        (
            cert::describe_name(c.subject),
            cert::describe_name(c.issuer),
        )
    });
    let (valid_from, valid_until) = parsed
        .and_then(|c| c.validity)
        .map_or_else(Default::default, |(from, until)| {
            (from.describe(), until.describe())
        });
    UnverifiedCertificate {
        problem,
        detail: error.to_string(),
        fingerprint,
        subject,
        issuer,
        valid_from,
        valid_until,
        changed,
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
    /// The server's certificate could not be verified, and was not one
    /// permitted for this host: why, and what it says.
    Unverified(Box<UnverifiedCertificate>),
}

/// How a connection's server certificate was accepted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ServerTrust {
    /// It chained to a trusted root and named the host.
    Verified,
    /// It could not be verified, and the user permitted it for this host by
    /// its fingerprint ([`Transport::connect_trusting`]).
    Permitted,
}

/// Why a server certificate could not be verified.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CertificateProblem {
    /// Issued by an authority this client does not trust; `self_signed`
    /// when it is its own issuer.
    UnknownIssuer {
        self_signed: bool,
    },
    Expired,
    NotYetValid,
    /// It does not name the host connected to.
    WrongName,
    /// In a form the verifier does not read: X.509 version 1, as FileMaker
    /// Server's default certificate is.
    OldFormat,
    /// Another reason, which [`UnverifiedCertificate::detail`] gives.
    Other,
}

/// A server certificate that could not be verified, as a user is asked
/// about it: why, what it says, and the fingerprint a permit keeps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct UnverifiedCertificate {
    pub problem: CertificateProblem,
    /// The TLS library's own words for it.
    pub detail: String,
    /// The SHA-256 fingerprint of the certificate's DER encoding.
    pub fingerprint: [u8; 32],
    /// Its subject's and issuer's names ([`cert::describe_name`]), empty
    /// when they do not read.
    pub subject: String,
    pub issuer: String,
    /// When it becomes valid and when it expires ([`cert::Time::describe`]),
    /// empty when they do not read.
    pub valid_from: String,
    pub valid_until: String,
    /// Another certificate was permitted for this host: this one replaced
    /// it.
    pub changed: bool,
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
            TransportError::Unverified(e) => f.write_str(&e.detail),
        }
    }
}
impl std::error::Error for TransportError {}

/// A synchronous TLS + GIOP connection.
pub struct Transport {
    stream: StreamOwned<ClientConnection, Socket>,
    reader: MessageReader,
    /// How the server's certificate was accepted; `None` for the lab
    /// transport, which checks nothing ([`Transport::connect`]).
    trust: Option<ServerTrust>,
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
        Self::connect_config(host, port, config, None)
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
        Self::connect_trusting(
            host,
            port,
            client_cert_pem,
            client_key_pem,
            additional_ca_pem,
            &[],
        )
    }

    /// [`Self::connect_verified`], and when the certificate cannot be
    /// verified, accept it if its SHA-256 fingerprint is one of `permitted`,
    /// the certificates the user permitted for this host and port: FileMaker
    /// Server's default certificate, X.509 version 1 and issued by Claris's
    /// own authority, connects no other way. Its handshake signature is
    /// still checked, with its own key; its name and dates are not.
    /// [`Self::trust`] says which way it was accepted. A certificate
    /// refused is [`TransportError::Unverified`].
    pub fn connect_trusting(
        host: &str,
        port: u16,
        client_cert_pem: &[u8],
        client_key_pem: &[u8],
        additional_ca_pem: &[u8],
        permitted: &[[u8; 32]],
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
        let roots = WebPkiServerVerifier::builder_with_provider(Arc::new(roots), provider.clone())
            .build()
            .map_err(|e| TransportError::Pem(e.to_string()))?;
        let verifier = Arc::new(HostVerifier {
            roots,
            algorithms: provider.signature_verification_algorithms,
            permitted: permitted.to_vec(),
            decision: Mutex::new(None),
        });
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(TransportError::Tls)?
            .dangerous()
            .with_custom_certificate_verifier(verifier.clone())
            .with_client_auth_cert(certs, key)
            .map_err(TransportError::Tls)?;
        Self::connect_config(host, port, config, Some(verifier))
    }

    /// How the server's certificate was accepted: `None` for the lab
    /// transport ([`Self::connect`]), which checks nothing.
    pub fn trust(&self) -> Option<ServerTrust> {
        self.trust
    }

    fn connect_config(
        host: &str,
        port: u16,
        config: ClientConfig,
        verifier: Option<Arc<HostVerifier>>,
    ) -> Result<Self, TransportError> {
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
        // Force the handshake now so cert problems surface here, with the
        // certificate that caused them.
        let handshake = stream.flush();
        let decision = verifier.as_ref().and_then(|v| v.take());
        if let Some(Decision::Refused(refused)) = decision {
            return Err(TransportError::Unverified(refused));
        }
        handshake?;
        let trust = match decision {
            Some(Decision::Permitted) => Some(ServerTrust::Permitted),
            Some(Decision::Verified) => Some(ServerTrust::Verified),
            Some(Decision::Refused(_)) | None => None,
        };
        Ok(Self {
            stream,
            reader: MessageReader::new(MAX_MESSAGE_LEN),
            trust,
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
