//! An OpenMaker Server run as a process of its own on loopback, for the
//! scorecard's hosted measures: the `om-server` binary (`OM_SERVER_BIN`,
//! else this workspace's `target/debug/om-server`; build it with
//! `cargo build -p om-server`) hosting copies of the files given, signed
//! in to with each file's own account: the starter solutions' Admin, in
//! [Full Access] with an empty password. FileMaker Server does not host
//! such files by default, so the server is told to
//! (`--allow-insecure-files`, FileMaker Server's `securefilesonly=false`).
//! Stopped when dropped. Only `std`: the integration tests include this
//! file too.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

/// The starter solutions' account, and its password.
pub const ACCOUNT: &str = "Admin";
pub const PASSWORD: &str = "";

pub struct HostedServer {
    child: Child,
    pub port: u16,
    /// The certificate the server made, which clients trust.
    pub ca_pem: Vec<u8>,
}

impl HostedServer {
    /// Host `files` (each by its file name) from `dir`, where the server
    /// keeps its state and log; ready once its port answers.
    pub fn start(files: &[PathBuf], dir: &Path) -> Result<HostedServer, String> {
        let binary = std::env::var_os("OM_SERVER_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/debug/om-server")
            });
        if !binary.is_file() {
            return Err(format!(
                "no om-server at {} (cargo build -p om-server, or set OM_SERVER_BIN)",
                binary.display()
            ));
        }
        let state = dir.join("server-state");
        std::fs::create_dir_all(&state).map_err(|e| e.to_string())?;
        // A free port: bound and let go of just before the server takes it.
        let port = TcpListener::bind("127.0.0.1:0")
            .and_then(|l| l.local_addr())
            .map_err(|e| e.to_string())?
            .port();
        let log = std::fs::File::create(dir.join("server.log")).map_err(|e| e.to_string())?;
        let mut command = Command::new(&binary);
        command
            .arg("--bind")
            .arg(format!("127.0.0.1:{port}"))
            .arg("--state-dir")
            .arg(&state)
            .arg("--allow-insecure-files")
            .arg("--list-all")
            .args(files)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(log);
        let child = command
            .spawn()
            .map_err(|e| format!("starting om-server: {e}"))?;
        let mut server = HostedServer {
            child,
            port,
            ca_pem: Vec::new(),
        };
        let cert = state.join("tls").join("cert.pem");
        let address: SocketAddr = format!("127.0.0.1:{port}").parse().expect("an address");
        let started = Instant::now();
        loop {
            if let Ok(Some(status)) = server.child.try_wait() {
                return Err(format!(
                    "om-server exited ({status}); its log: {}",
                    std::fs::read_to_string(dir.join("server.log")).unwrap_or_default()
                ));
            }
            if cert.is_file()
                && TcpStream::connect_timeout(&address, Duration::from_millis(200)).is_ok()
            {
                break;
            }
            if started.elapsed() > Duration::from_secs(60) {
                return Err("om-server did not answer within 60 s".into());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        server.ca_pem = std::fs::read(&cert).map_err(|e| e.to_string())?;
        Ok(server)
    }
}

impl Drop for HostedServer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
