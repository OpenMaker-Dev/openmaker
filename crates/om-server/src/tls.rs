//! The server side of mutual TLS on port 5003.
//!
//! FileMaker clients must present a client certificate, but FileMaker Server
//! accepts a throwaway self-signed one (spec F10/F11): the certificate is a
//! transport gate, and authentication happens at Login. OpenMaker Server
//! applies the same policy: a client certificate is required and its handshake
//! signature is verified, but its chain is not. FileMaker Pro's certificate is
//! X.509 version 1, which webpki refuses, so the signature is checked against
//! the key read by `cert` instead.

use std::sync::Arc;

use log::info;
use rustls::client::danger::HandshakeSignatureValid;
use rustls::client::{verify_server_cert_signed_by_trust_anchor, verify_server_name};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::server::ParsedCertificate;
use rustls::sign::CertifiedKey;
use rustls::{
    CertificateError, DigitallySignedStruct, DistinguishedName, Error, PeerIncompatible,
    RootCertStore, ServerConfig, SignatureScheme,
};

use om_session::cert;

/// A server certificate chain and its private key.
#[derive(Debug)]
pub struct Identity {
    pub certs: Vec<CertificateDer<'static>>,
    pub key: PrivateKeyDer<'static>,
}

impl Clone for Identity {
    fn clone(&self) -> Self {
        Self {
            certs: self.certs.clone(),
            key: self.key.clone_key(),
        }
    }
}

/// Errors loading or generating a server identity.
#[derive(Debug)]
pub enum IdentityError {
    Pem(String),
    Generate(rcgen::Error),
    Tls(rustls::Error),
}

impl std::fmt::Display for IdentityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            IdentityError::Pem(e) => write!(f, "pem: {e}"),
            IdentityError::Generate(e) => write!(f, "generating a certificate: {e}"),
            IdentityError::Tls(e) => write!(f, "tls: {e}"),
        }
    }
}

impl std::error::Error for IdentityError {}

impl Identity {
    /// Load a PEM certificate chain and private key.
    pub fn from_pem(cert_pem: &[u8], key_pem: &[u8]) -> Result<Self, IdentityError> {
        let certs = rustls_pemfile::certs(&mut &cert_pem[..])
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| IdentityError::Pem(e.to_string()))?;
        if certs.is_empty() {
            return Err(IdentityError::Pem("no certificate in PEM".into()));
        }
        let key = rustls_pemfile::private_key(&mut &key_pem[..])
            .map_err(|e| IdentityError::Pem(e.to_string()))?
            .ok_or_else(|| IdentityError::Pem("no private key in PEM".into()))?;
        Ok(Self { certs, key })
    }

    /// A fresh self-signed certificate for `names` (DNS names or IPs), as
    /// FileMaker Server ships with until an administrator installs a real one.
    pub fn self_signed(names: Vec<String>) -> Result<Self, IdentityError> {
        let generated =
            rcgen::generate_simple_self_signed(names).map_err(IdentityError::Generate)?;
        Ok(Self {
            certs: vec![generated.cert.der().clone()],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                generated.signing_key.serialize_der(),
            )),
        })
    }

    /// The SHA-256 fingerprint of the server's own certificate (the first),
    /// as `openssl x509 -fingerprint -sha256` and browsers show it:
    /// upper-case hex pairs joined by colons. Empty without a certificate.
    pub fn fingerprint(&self) -> String {
        let Some(cert) = self.certs.first() else {
            return String::new();
        };
        let digest = ::ring::digest::digest(&::ring::digest::SHA256, cert.as_ref());
        let pairs: Vec<String> = digest.as_ref().iter().map(|b| format!("{b:02X}")).collect();
        pairs.join(":")
    }

    /// Check that a client told to trust this certificate itself (OpenMaker's
    /// `OPENMAKER_CA_CERT`) accepts it now: the key is the certificate's, and
    /// the certificate is self-signed with an algorithm clients verify, valid
    /// today and not a CA (rustls refuses a CA as a server's own). Whether it
    /// names the host a client dials is [`Self::names`].
    pub fn check_self_trusted(&self) -> Result<(), String> {
        let provider = ring::default_provider();
        let end_entity = self.certs.first().ok_or("no certificate")?;
        let key = provider
            .key_provider
            .load_private_key(self.key.clone_key())
            .map_err(|e| e.to_string())?;
        // ring tells for the RSA, ECDSA and Ed25519 keys it loads.
        CertifiedKey::new(self.certs.clone(), key)
            .keys_match()
            .map_err(|_| "the key is not the certificate's")?;
        let parsed = ParsedCertificate::try_from(end_entity).map_err(|e| e.to_string())?;
        let mut roots = RootCertStore::empty();
        roots.add(end_entity.clone()).map_err(|e| e.to_string())?;
        verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &roots,
            &self.certs[1..],
            UnixTime::now(),
            provider.signature_verification_algorithms.all,
        )
        .map_err(|e| format!("clients would refuse it: {e}"))
    }

    /// Whether the certificate names `host` (a DNS name or an IP address), as
    /// a client that connects by that name checks.
    pub fn names(&self, host: &str) -> bool {
        let (Some(cert), Ok(host)) = (self.certs.first(), ServerName::try_from(host)) else {
            return false;
        };
        ParsedCertificate::try_from(cert)
            .is_ok_and(|parsed| verify_server_name(&parsed, &host).is_ok())
    }
}

/// Requires a client certificate and checks its handshake signature, but not
/// its chain (see the module docs).
#[derive(Debug)]
struct AnyClientCert;

impl AnyClientCert {
    fn verify_signature(
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        let parsed =
            cert::parse(cert).ok_or(Error::InvalidCertificate(CertificateError::BadEncoding))?;
        if parsed.key.verify(dss.scheme, message, dss.signature()) {
            Ok(HandshakeSignatureValid::assertion())
        } else {
            info!(
                "client certificate ({}): bad {:?} handshake signature",
                parsed.key.describe(),
                dss.scheme
            );
            Err(Error::InvalidCertificate(CertificateError::BadSignature))
        }
    }
}

impl ClientCertVerifier for AnyClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        let Some(parsed) = cert::parse(end_entity) else {
            info!("client certificate is not parseable X.509");
            return Err(Error::InvalidCertificate(CertificateError::BadEncoding));
        };
        info!(
            "client certificate: X.509 v{}, {}",
            parsed.version,
            parsed.key.describe()
        );
        if matches!(parsed.key, cert::PublicKey::Other(_)) {
            return Err(Error::PeerIncompatible(
                PeerIncompatible::NoSignatureSchemesInCommon,
            ));
        }
        Ok(ClientCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Self::verify_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        Self::verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        cert::SCHEMES.to_vec()
    }
}

/// Accepts only the one client certificate it was given, with a valid
/// handshake signature: the private host of a local client
/// ([`crate::LocalHost`]), which nothing but that client may use.
#[derive(Debug)]
struct PinnedClientCert(CertificateDer<'static>);

impl ClientCertVerifier for PinnedClientCert {
    fn root_hint_subjects(&self) -> &[DistinguishedName] {
        &[]
    }

    fn verify_client_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _now: UnixTime,
    ) -> Result<ClientCertVerified, Error> {
        if end_entity.as_ref() == self.0.as_ref() {
            Ok(ClientCertVerified::assertion())
        } else {
            info!("refusing a client certificate other than the local client's");
            Err(Error::InvalidCertificate(
                CertificateError::ApplicationVerificationFailure,
            ))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        AnyClientCert::verify_signature(message, cert, dss)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, Error> {
        AnyClientCert::verify_signature(message, cert, dss)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        cert::SCHEMES.to_vec()
    }
}

/// The rustls server configuration for port 5003.
pub fn server_config(identity: Identity) -> Result<Arc<ServerConfig>, IdentityError> {
    config_with(identity, Arc::new(AnyClientCert))
}

/// A server configuration that accepts only `client`, the certificate of
/// the one client a private host serves.
pub fn private_server_config(
    identity: Identity,
    client: CertificateDer<'static>,
) -> Result<Arc<ServerConfig>, IdentityError> {
    config_with(identity, Arc::new(PinnedClientCert(client)))
}

fn config_with(
    identity: Identity,
    clients: Arc<dyn ClientCertVerifier>,
) -> Result<Arc<ServerConfig>, IdentityError> {
    let provider = Arc::new(ring::default_provider());
    let config = ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(IdentityError::Tls)?
        .with_client_cert_verifier(clients)
        .with_single_cert(identity.certs, identity.key)
        .map_err(IdentityError::Tls)?;
    Ok(Arc::new(config))
}
