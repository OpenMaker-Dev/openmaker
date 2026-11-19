//! OpenMaker command-line client (protocol probe).
//!
//! Subcommands open the TLS + GIOP transport to a FileMaker Server:
//!   isa     - `_is_a` type check (no credentials)
//!   connect - `_is_a` + `Connect`, print the server identity, Disconnect
//!   open    - connect, then Open a hosted file (op78) and print its status/nonce
//!   login   - connect, Open, then Login (op80) once and print the error code
//!   files   - connect, list the hosted files (op77 GetFileList), Disconnect
//!
//! None of these modify anything. `login` sends account credentials read from
//! the environment (`OM_ACCOUNT`, `OM_PASSWORD`); they are never printed. It
//! makes exactly one attempt per run: failed logins count toward the server's
//! account lockout, so it never retries.

use om_session as session;

use std::process::ExitCode;

use om_protocol::{LoginEnvironment, MachineInfo};
use session::Session;

fn arg(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("isa") => run(run_isa(&args)),
        Some("connect") => run(run_connect(&args)),
        Some("open") => run(run_open(&args)),
        Some("login") => run(run_login(&args)),
        Some("files") => run(run_files(&args)),
        _ => {
            eprintln!("usage:");
            eprintln!("  om-client isa     --host <h> [--port 5003] --client-cert <pem> --client-key <pem>");
            eprintln!("  om-client connect --host <h> [--port 5003] --client-cert <pem> --client-key <pem> [--callback-host <ip>]");
            eprintln!("  om-client open    --host <h> [--port 5003] --client-cert <pem> --client-key <pem> --file <name.fmp12> [--callback-host <ip>]");
            eprintln!("  om-client login   --host <h> [--port 5003] --client-cert <pem> --client-key <pem> --file <name.fmp12> [--callback-host <ip>]");
            eprintln!("                    [--internal-creation <hex u32> | --internal-creation-from <local copy.fmp12>]  (debug override; default reads it from the server)");
            eprintln!(
                "                    credentials from env OM_ACCOUNT / OM_PASSWORD (never printed)"
            );
            eprintln!("  om-client files   --host <h> [--port 5003] --client-cert <pem> --client-key <pem> [--callback-host <ip>] [--save <reply.bin>]");
            ExitCode::from(64)
        }
    }
}

fn run(result: Result<(), Box<dyn std::error::Error>>) -> ExitCode {
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(1)
        }
    }
}

struct Conn {
    host: String,
    port: u16,
    cert: Vec<u8>,
    key: Vec<u8>,
    callback_host: String,
}

fn conn_args(args: &[String]) -> Result<Conn, Box<dyn std::error::Error>> {
    Ok(Conn {
        host: arg(args, "--host").ok_or("missing --host")?,
        port: arg(args, "--port").as_deref().unwrap_or("5003").parse()?,
        cert: std::fs::read(arg(args, "--client-cert").ok_or("missing --client-cert")?)?,
        key: std::fs::read(arg(args, "--client-key").ok_or("missing --client-key")?)?,
        callback_host: arg(args, "--callback-host").unwrap_or_else(|| "127.0.0.1".into()),
    })
}

fn run_isa(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    use om_giop::{Reply, ServiceContext};
    use om_protocol::{build_is_a, OBJECT_KEY};
    use om_session::Transport;
    let c = conn_args(args)?;
    let _ = OBJECT_KEY;
    let mut t = Transport::connect(&c.host, c.port, &c.cert, &c.key)?;
    eprintln!(
        "[om-client] TLS up: {}",
        t.tls_version().unwrap_or_default()
    );
    let no_contexts: [ServiceContext; 0] = [];
    t.send(&build_is_a(2, &no_contexts))?;
    let reply = Reply::parse(&t.recv()?)?.body.first().copied().unwrap_or(0) != 0;
    println!(
        "result: server object {} {}",
        if reply { "IS a" } else { "is NOT a" },
        String::from_utf8_lossy(om_protocol::REPO_ID)
    );
    if reply {
        Ok(())
    } else {
        Err("not the expected interface".into())
    }
}

fn machine_info() -> MachineInfo {
    // The server validates the FileMaker license key (install_code) on the first
    // Open (op78), returning 0x507c if it is empty or invalid. Supply a valid key
    // via FM_LICENSE_KEY; it is never logged or committed. Connect still succeeds
    // without one, but Open will fail until a valid key is provided.
    let install_code = std::env::var("FM_LICENSE_KEY").unwrap_or_default();
    MachineInfo {
        install_code,
        ..MachineInfo::default()
    }
}

fn run_connect(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let c = conn_args(args)?;
    let keyed = if std::env::var("FM_LICENSE_KEY").is_ok() {
        "with license key"
    } else {
        "no license key"
    };
    eprintln!(
        "[om-client] connecting to {}:{} ({keyed}, no credentials)",
        c.host, c.port
    );
    let session = Session::connect(
        &c.host,
        c.port,
        &c.cert,
        &c.key,
        &c.callback_host,
        &machine_info(),
    )?;
    let id = session.identity();
    println!("Connected. Server identity:");
    println!("  connection id:   0x{:08x}", id.connection_id);
    println!("  server name:     {}", id.remote_name);
    println!("  version:         {}", id.version_string);
    println!("  protocol:        0x{:02x}", id.protocol_version);
    println!("  server id len:   {}", id.install_code.len());
    session.disconnect()?;
    eprintln!("[om-client] disconnected");
    Ok(())
}

fn run_open(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let c = conn_args(args)?;
    let file = arg(args, "--file").ok_or("missing --file (e.g. \"test project.fmp12\")")?;
    eprintln!("[om-client] connecting to {}:{}", c.host, c.port);
    let mut session = Session::connect(
        &c.host,
        c.port,
        &c.cert,
        &c.key,
        &c.callback_host,
        &machine_info(),
    )?;
    eprintln!(
        "[om-client] connected (0x{:08x}), opening {file:?}",
        session.identity().connection_id
    );
    let open = session.open_file(&c.host, &file).map_err(|e| {
        if e.to_string().contains("0x507c") && std::env::var("FM_LICENSE_KEY").is_err() {
            format!("{e}\nhint: the server rejects Open without a valid license key; set FM_LICENSE_KEY")
        } else {
            e.to_string()
        }
    })?;
    println!("Opened {file:?}:");
    println!("  status:        {}", open.status);
    println!("  server msg:    {}", open.server_message);
    println!("  file nonce:    0x{:08x}", open.file_nonce);
    session.disconnect()?;
    eprintln!("[om-client] disconnected");
    Ok(())
}

/// The file's `mInternalCreation`: given directly, or read from a local copy of
/// the hosted file's HBAM.
/// An explicit `mInternalCreation` override, for debugging. Without one, Login
/// reads it from the server the way the official client does (op90 Download).
fn internal_creation_override(args: &[String]) -> Result<Option<u32>, Box<dyn std::error::Error>> {
    if let Some(hex) = arg(args, "--internal-creation") {
        return Ok(Some(u32::from_str_radix(hex.trim_start_matches("0x"), 16)?));
    }
    let Some(path) = arg(args, "--internal-creation-from") else {
        return Ok(None);
    };
    let file = om_hbam::HbamFile::open(&path)?;
    Ok(Some(file.internal_creation()?.ok_or_else(|| {
        format!("{path}: no mInternalCreation in the file's HBAM")
    })?))
}

fn run_login(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let c = conn_args(args)?;
    let file = arg(args, "--file").ok_or("missing --file (e.g. \"test project.fmp12\")")?;
    let account = std::env::var("OM_ACCOUNT").map_err(|_| "set OM_ACCOUNT")?;
    let password = std::env::var("OM_PASSWORD").map_err(|_| "set OM_PASSWORD")?;
    let override_value = internal_creation_override(args)?;

    eprintln!("[om-client] connecting to {}:{}", c.host, c.port);
    let mut session = Session::connect(
        &c.host,
        c.port,
        &c.cert,
        &c.key,
        &c.callback_host,
        &machine_info(),
    )?;
    let open = session.open_file(&c.host, &file)?;
    eprintln!(
        "[om-client] opened {file:?} (server file id {})",
        open.file_id
    );
    let internal_creation = match override_value {
        Some(value) => value,
        None => session.fetch_internal_creation()?,
    };
    eprintln!("[om-client] mInternalCreation: 0x{internal_creation:08x}; logging in once");
    let environment = LoginEnvironment::for_remote_file(&c.host, &file);
    let reply = session.login(&account, &password, internal_creation, &environment)?;
    println!("Login {file:?}:");
    println!("  error code:    0x{:04x}", reply.error_code);
    if let Some(version) = reply.version {
        println!("  reply version: {version}");
        println!("  result key:    {} bytes", reply.result_key.len());
    }
    println!(
        "  result:        {}",
        match reply.error_code {
            0 => "success",
            0xd0 => "password change required",
            0x20d0 => "external authentication continuation",
            _ => "rejected",
        }
    );
    session.disconnect()?;
    eprintln!("[om-client] disconnected");
    if reply.is_success() {
        Ok(())
    } else {
        Err(format!("login failed with error code 0x{:04x}", reply.error_code).into())
    }
}

fn run_files(args: &[String]) -> Result<(), Box<dyn std::error::Error>> {
    let c = conn_args(args)?;
    eprintln!("[om-client] connecting to {}:{}", c.host, c.port);
    let mut session = Session::connect(
        &c.host,
        c.port,
        &c.cert,
        &c.key,
        &c.callback_host,
        &machine_info(),
    )?;
    let request = om_protocol::FileListRequest::anonymous();
    let reply = session.perform_reply(&om_protocol::build_file_list(1, &request))?;
    session.disconnect()?;
    eprintln!("[om-client] disconnected");
    if let Some(path) = arg(args, "--save") {
        save_private(&path, &reply.result)?;
        eprintln!(
            "[om-client] saved the {}-byte reply to {path}",
            reply.result.len()
        );
    }
    println!("GetFileList reply: {} bytes", reply.result.len());
    Ok(())
}

/// Write `bytes` to a new file only its owner can read.
fn save_private(path: &str, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}
