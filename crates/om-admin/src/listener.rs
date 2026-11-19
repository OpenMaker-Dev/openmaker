//! HTTPS: accepting connections, the TLS handshake and HTTP/1.1 on each
//! connection, with limits so that slow or numerous clients cannot hold the
//! console.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use axum::body::Body;
use axum::extract::ConnectInfo;
use axum::http::header::CONNECTION;
use axum::http::{HeaderValue, Response, StatusCode};
use axum::Router;
use hyper::body::Incoming;
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::{TokioIo, TokioTimer};
use log::{debug, warn};
use rustls::crypto::ring;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::ServerConfig;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinSet;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;

use crate::auth::client_key;

/// Connections served at once; more are closed as they arrive.
const MAX_CONNECTIONS: usize = 64;
/// Connections served at once from one client address (an IPv6 /64), so
/// that one client cannot take them all. A browser opens up to six.
const MAX_PER_ADDRESS: usize = 8;
/// How long a client may take over its TLS handshake.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long a client may take to send a request's headers, and how long an
/// idle kept-alive connection stays open.
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
/// How long a request may take from its headers to its response, its body
/// included; past it the client gets 408 and the connection closes.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// How long requests being answered may run on at shutdown.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// The TLS configuration: the server's certificate, no client certificates,
/// HTTP/1.1.
pub(crate) fn tls_config(
    certificates: Vec<CertificateDer<'static>>,
    key: PrivateKeyDer<'static>,
) -> Result<Arc<ServerConfig>, rustls::Error> {
    let mut config = ServerConfig::builder_with_provider(Arc::new(ring::default_provider()))
        .with_safe_default_protocol_versions()?
        .with_no_client_auth()
        .with_single_cert(certificates, key)?;
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

/// Accept and serve connections until `stop` turns true. Then idle
/// connections close at once, requests being answered get
/// [`SHUTDOWN_GRACE`] to finish, and the rest are dropped.
pub(crate) async fn serve(
    listener: TcpListener,
    tls: Arc<ServerConfig>,
    router: Router,
    mut stop: watch::Receiver<bool>,
) {
    let acceptor = TlsAcceptor::from(tls);
    let permits = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let addresses = Arc::new(Addresses::default());
    let mut connections = JoinSet::new();
    loop {
        let (tcp, peer) = tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok(accepted) => accepted,
                Err(e) => {
                    // Out of file descriptors, for example: wait rather than
                    // spin on the error.
                    warn!("Admin Console: accepting a connection: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
            Some(_) = connections.join_next(), if !connections.is_empty() => continue,
            _ = stop.changed() => break,
        };
        let Some(slot) = addresses.take(peer.ip()) else {
            debug!("{peer}: closed: {MAX_PER_ADDRESS} console connections from this address");
            continue;
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            debug!("{peer}: closed: {MAX_CONNECTIONS} console connections are open");
            continue;
        };
        let connection = Connection {
            tcp,
            peer,
            acceptor: acceptor.clone(),
            router: router.clone(),
            stop: stop.clone(),
            _held: (permit, slot),
        };
        connections.spawn(connection.serve());
    }
    drop(listener);
    let finished = tokio::time::timeout(SHUTDOWN_GRACE, async {
        while connections.join_next().await.is_some() {}
    })
    .await;
    if finished.is_err() {
        debug!(
            "Admin Console: closing {} connections still answering",
            connections.len()
        );
    }
}

/// One client connection and what it holds while it is open.
struct Connection {
    tcp: TcpStream,
    peer: std::net::SocketAddr,
    acceptor: TlsAcceptor,
    router: Router,
    stop: watch::Receiver<bool>,
    _held: (OwnedSemaphorePermit, Slot),
}

impl Connection {
    async fn serve(mut self) {
        let peer = self.peer;
        let _ = self.tcp.set_nodelay(true);
        let handshake = tokio::time::timeout(HANDSHAKE_TIMEOUT, self.acceptor.accept(self.tcp));
        let tls = tokio::select! {
            tls = handshake => match tls {
                Ok(Ok(tls)) => tls,
                Ok(Err(e)) => {
                    debug!("{peer}: TLS handshake: {e}");
                    return;
                }
                Err(_) => {
                    debug!("{peer}: TLS handshake timed out");
                    return;
                }
            },
            _ = self.stop.changed() => return,
        };
        let router = self.router;
        let service = service_fn(move |mut request: hyper::Request<Incoming>| {
            request.extensions_mut().insert(ConnectInfo(peer));
            let response = router.clone().oneshot(request);
            async move {
                match tokio::time::timeout(REQUEST_TIMEOUT, response).await {
                    Ok(response) => response,
                    Err(_) => {
                        debug!("{peer}: request timed out");
                        Ok(timed_out())
                    }
                }
            }
        });
        let connection = http1::Builder::new()
            .timer(TokioTimer::new())
            .header_read_timeout(HEADER_TIMEOUT)
            .serve_connection(TokioIo::new(tls), service);
        let mut connection = std::pin::pin!(connection);
        let served = tokio::select! {
            served = connection.as_mut() => served,
            _ = self.stop.changed() => {
                // Close once the request being answered, if any, is.
                connection.as_mut().graceful_shutdown();
                connection.as_mut().await
            }
        };
        if let Err(e) = served {
            debug!("{peer}: {e}");
        }
    }
}

/// 408, closing the connection: the request's body was not read to its end.
fn timed_out() -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = StatusCode::REQUEST_TIMEOUT;
    response
        .headers_mut()
        .insert(CONNECTION, HeaderValue::from_static("close"));
    response
}

/// Open connections by client address.
#[derive(Default)]
struct Addresses {
    open: Mutex<HashMap<IpAddr, usize>>,
}

/// A connection counted against its address until dropped.
struct Slot {
    addresses: Arc<Addresses>,
    key: IpAddr,
}

impl Addresses {
    fn take(self: &Arc<Self>, address: IpAddr) -> Option<Slot> {
        let key = client_key(address);
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let count = open.entry(key).or_default();
        if *count >= MAX_PER_ADDRESS {
            return None;
        }
        *count += 1;
        Some(Slot {
            addresses: self.clone(),
            key,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self
            .addresses
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if let Some(count) = open.get_mut(&self.key) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.key);
            }
        }
    }
}
