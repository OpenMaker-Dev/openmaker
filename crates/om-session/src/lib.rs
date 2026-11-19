//! OpenMaker's TLS + GIOP transport and the RCFMP12 session, as a library so
//! both the CLI and the GUI's data provider can drive a live FileMaker Server.

pub mod session;
pub mod tls;

pub use session::{
    add_record_requests_sent, record_requests_sent, Admission, Claim, ConnectionRef, OpenedFile,
    Session, SessionError, KEEPALIVE_INTERVAL, TICKLE_TIMEOUT,
};
pub use tls::{Transport, TransportError, MAX_MESSAGE_LEN, READ_TIMEOUT};
