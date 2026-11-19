//! The Admin Console over sample data, for working on its pages without a
//! server:
//!
//! ```sh
//! OM_ADMIN_PASSWORD=<password> cargo run -p om-admin --example mock_console [-- <addr:port>]
//! ```
//!
//! It listens on `127.0.0.1:16000` unless given an address, signs in the
//! user `admin` with the password in `OM_ADMIN_PASSWORD`, and presents a new
//! self-signed certificate for `localhost`, which the browser warns about.

use std::sync::Arc;

use om_admin::{Console, ConsoleConfig, MockBackend};
use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    let bind = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "127.0.0.1:16000".into())
        .parse()?;
    let password = std::env::var("OM_ADMIN_PASSWORD")
        .map_err(|_| "set OM_ADMIN_PASSWORD to the password to sign in with")?;
    let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()])?;
    // The sample server presents the certificate the console does.
    let backend = Arc::new(MockBackend::sample());
    backend.state().certificate = Some(vec![generated.cert.der().to_vec()]);
    // Volume Status shows the file system of the directory it runs in.
    backend.state().data_folder = std::env::current_dir().ok();
    let console = Console::start(
        ConsoleConfig {
            bind,
            certificates: vec![generated.cert.der().clone()],
            key: PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
                generated.signing_key.serialize_der(),
            )),
            user: "admin".into(),
            password: password.into(),
        },
        backend,
    )?;
    println!(
        "https://localhost:{}/admin-console (user admin)",
        console.local_addr().port()
    );
    loop {
        std::thread::park();
    }
}
