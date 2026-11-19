//! The application transport must authenticate the server, including private
//! CA support and hostname verification. All servers here are local test peers.
use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use om_session::Transport;
use rustls::pki_types::PrivatePkcs8KeyDer;
use rustls::{ServerConfig, ServerConnection};

fn connect_to_local_peer(name: &str, trust: bool) -> Result<Transport, om_session::TransportError> {
    let server_identity = rcgen::generate_simple_self_signed(vec![name.into()]).unwrap();
    let client_identity =
        rcgen::generate_simple_self_signed(vec!["OpenMaker test".into()]).unwrap();
    let ca = if trust {
        server_identity.cert.pem().into_bytes()
    } else {
        vec![]
    };
    let config =
        ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(
                vec![server_identity.cert.der().clone()],
                PrivatePkcs8KeyDer::from(server_identity.key_pair.serialize_der()).into(),
            )
            .unwrap();
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = std::thread::spawn(move || {
        let (mut socket, _) = listener.accept().unwrap();
        socket
            .set_read_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        socket
            .set_write_timeout(Some(Duration::from_secs(3)))
            .unwrap();
        let mut connection = ServerConnection::new(Arc::new(config)).unwrap();
        while connection.is_handshaking() {
            if connection.complete_io(&mut socket).is_err() {
                break;
            }
        }
    });
    let result = Transport::connect_verified(
        "127.0.0.1",
        port,
        client_identity.cert.pem().as_bytes(),
        client_identity.key_pair.serialize_pem().as_bytes(),
        &ca,
    );
    peer.join().unwrap();
    result
}

#[test]
fn explicitly_trusted_server_with_matching_hostname_connects() {
    assert!(connect_to_local_peer("127.0.0.1", true).is_ok());
}

#[test]
fn untrusted_server_certificate_is_rejected() {
    let error = match connect_to_local_peer("127.0.0.1", false) {
        Ok(_) => panic!("untrusted server was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("UnknownIssuer"), "{error}");
}

#[test]
fn trusted_server_with_wrong_hostname_is_rejected() {
    let error = match connect_to_local_peer("wrong.invalid", true) {
        Ok(_) => panic!("wrong server hostname was accepted"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("not valid for name"), "{error}");
}
