//! The RCFMP12 `Connect` request and reply.
//!
//! Byte layout per `docs/re/network-protocol.md` section 5.2. Both OctetSeq
//! payloads share one shape: 2 bytes, 4 strings, 2 bytes, 3 strings (the reply
//! additionally prefixes a 16-byte connection key). Strings use the
//! `PopUnistring` framing: a plain 1-byte length then SCSU + XOR 0x5A bytes
//! (`om_scsu::Framing::LeadingU8`).
//!
//! Both directions are here: the client builds the request and parses the
//! reply; a server parses the request and builds the reply.

use om_giop::{
    build_reply, build_request, Reader as CdrReader, Reply, ReplyStatus, Request,
    Writer as CdrWriter,
};
use om_scsu::{decode_field, encode_field, Framing};

use crate::{op, OBJECT_KEY, REPO_ID};

/// Client machine information sent in the `Connect` request OctetSeq.
///
/// For a clean client most fields can be synthetic; the one the server gates on
/// is `protocol_version`, which must be `> 0x1a`. `install_code` is the FileMaker
/// license key and may be empty (whether the server then accepts the connection
/// is determined live).
#[derive(Clone)]
pub struct MachineInfo {
    pub exe_type: u8,
    pub app_type: u8,
    pub machine_name: String,
    pub install_code: String,
    pub version_string: String,
    pub mac_addresses: String,
    pub system_language: u8,
    pub protocol_version: u8,
    pub persistent_id: String,
    pub web_service_settings: String,
    pub standby_address: String,
}

impl std::fmt::Debug for MachineInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MachineInfo")
            .field("exe_type", &self.exe_type)
            .field("app_type", &self.app_type)
            .field("machine_name", &self.machine_name)
            .field("install_code", &"[redacted]")
            .field("version_string", &self.version_string)
            .field("mac_addresses", &self.mac_addresses)
            .field("system_language", &self.system_language)
            .field("protocol_version", &self.protocol_version)
            .field("persistent_id", &self.persistent_id)
            .field("web_service_settings", &self.web_service_settings)
            .field("standby_address", &self.standby_address)
            .finish()
    }
}

impl Default for MachineInfo {
    fn default() -> Self {
        // Minimal, recognizable defaults for OpenMaker. protocol_version 0x20
        // matches the 26.0.3 client and clears the server's `> 0x1a` gate.
        Self {
            exe_type: 0x10,
            app_type: 0x01,
            machine_name: "openmaker".into(),
            install_code: String::new(),
            version_string: "OpenMaker 0.0".into(),
            mac_addresses: String::new(),
            system_language: 0x15,
            protocol_version: 0x20,
            persistent_id: String::new(),
            web_service_settings: String::new(),
            standby_address: String::new(),
        }
    }
}

impl MachineInfo {
    /// Marshal the machine-info OctetSeq (2 bytes, 4 strings, 2 bytes, 3 strings).
    pub fn to_octets(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.push(self.exe_type);
        out.push(self.app_type);
        push_string(&mut out, &self.machine_name);
        push_string(&mut out, &self.install_code);
        push_string(&mut out, &self.version_string);
        push_string(&mut out, &self.mac_addresses);
        out.push(self.system_language);
        out.push(self.protocol_version);
        push_string(&mut out, &self.persistent_id);
        push_string(&mut out, &self.web_service_settings);
        push_string(&mut out, &self.standby_address);
        out
    }

    /// Parse a machine-info OctetSeq (the inverse of [`MachineInfo::to_octets`]).
    pub fn from_octets(octets: &[u8]) -> Result<Self, ConnectError> {
        let mut pos = 0;
        Ok(Self {
            exe_type: take_u8(octets, &mut pos)?,
            app_type: take_u8(octets, &mut pos)?,
            machine_name: take_string(octets, &mut pos)?,
            install_code: take_string(octets, &mut pos)?,
            version_string: take_string(octets, &mut pos)?,
            mac_addresses: take_string(octets, &mut pos)?,
            system_language: take_u8(octets, &mut pos)?,
            protocol_version: take_u8(octets, &mut pos)?,
            persistent_id: take_string(octets, &mut pos)?,
            web_service_settings: take_string(octets, &mut pos)?,
            standby_address: take_string(octets, &mut pos)?,
        })
    }
}

fn push_string(out: &mut Vec<u8>, s: &str) {
    // LeadingU8 cannot represent >255 SCSU bytes; Connect fields are short.
    let field = encode_field(s, Framing::LeadingU8).expect("connect string fits in u8 length");
    out.extend_from_slice(&field);
}

/// The server identity carried by a `Connect` reply.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConnectReply {
    /// CORBA return value: the connection id echoed by later Perform/Tickle/Disconnect.
    pub connection_id: u32,
    /// The 16-byte connection key (the PBKDF2 obscuring salt for login).
    pub connection_key: [u8; 16],
    /// Server role bytes in the request's `exeType`/`appType` slots
    /// (`0x22`, `0x03` from FileMaker Server 26).
    pub exe_type: u8,
    pub app_type: u8,
    pub remote_name: String,
    pub version_string: String,
    /// The byte in the request's `systemLanguage` slot (`0x15` observed).
    pub system_language: u8,
    pub protocol_version: u8,
    /// The server's 32-hex id.
    pub install_code: String,
    pub web_service_settings: String,
    pub standby_address: String,
}

impl ConnectReply {
    /// Marshal the server-identity OctetSeq: the 16-byte key, then the shared
    /// 2 bytes, 4 strings, 2 bytes, 3 strings shape with the installCode and
    /// macAddr slots of the first group left empty (spec 5.2).
    ///
    /// # Panics
    /// If a string encodes to more than 255 SCSU bytes.
    pub fn to_octets(&self) -> Vec<u8> {
        let mut out = self.connection_key.to_vec();
        out.push(self.exe_type);
        out.push(self.app_type);
        push_string(&mut out, &self.remote_name);
        push_string(&mut out, "");
        push_string(&mut out, &self.version_string);
        push_string(&mut out, "");
        out.push(self.system_language);
        out.push(self.protocol_version);
        push_string(&mut out, &self.install_code);
        push_string(&mut out, &self.web_service_settings);
        push_string(&mut out, &self.standby_address);
        out
    }
}

/// A parsed `Connect` request (the server side).
#[derive(Clone, Debug)]
pub struct ConnectRequest {
    /// The repository id of the client's callback object reference.
    pub callback_type_id: String,
    /// The machine-info OctetSeq.
    pub machine: MachineInfo,
}

/// Error parsing a `Connect` message.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConnectError {
    Giop(String),
    Status(String),
    Truncated,
    BadString,
}

impl core::fmt::Display for ConnectError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            ConnectError::Giop(e) => write!(f, "connect message not valid GIOP: {e}"),
            ConnectError::Status(s) => write!(f, "connect reply status {s}"),
            ConnectError::Truncated => f.write_str("connect message truncated"),
            ConnectError::BadString => f.write_str("connect message has a malformed string field"),
        }
    }
}

impl std::error::Error for ConnectError {}

/// Build a `Connect` request message.
///
/// The callback IOR advertises `callback_host`:`callback_port` with the RCFMP12
/// object key and no extra components; the server stores but does not validate
/// it at Connect time (spec 5.2), so this suffices to obtain a connection id and
/// key. The two CORBA string args are empty, as in observed traffic.
pub fn build_connect(
    request_id: u32,
    callback_host: &str,
    callback_port: u16,
    info: &MachineInfo,
) -> Vec<u8> {
    let mut body = CdrWriter::with_origin(0);
    write_callback_ior(&mut body, callback_host, callback_port);
    body.string(b""); // unused1
    body.string(b""); // unused2
    body.octet_sequence(&info.to_octets());
    build_request(
        request_id,
        true,
        OBJECT_KEY,
        op::CONNECT,
        &[],
        &body.into_bytes(),
    )
}

/// Write the client callback object reference: type id plus one IIOP 1.2 profile.
fn write_callback_ior(w: &mut CdrWriter, host: &str, port: u16) {
    w.string(REPO_ID);
    w.u32(1); // one TaggedProfile
    w.u32(0); // TAG_INTERNET_IOP

    // The profile body is a CDR encapsulation: its own byte-order byte then a
    // CDR stream aligned from the encapsulation start.
    let mut prof = CdrWriter::with_origin(0);
    prof.u8(1); // little-endian
    prof.u8(1); // IIOP version major
    prof.u8(2); // IIOP version minor
                // host string follows; CDR aligns its u32 length to 4 within the encapsulation.
    prof.string(host.as_bytes());
    prof.u16(port);
    prof.octet_sequence(OBJECT_KEY);
    prof.u32(0); // zero TaggedComponents (server does not require them here)

    w.octet_sequence(&prof.into_bytes());
}

/// Parse a `Connect` request message: the callback object reference (kept only
/// as its type id; a server never dials it when the client negotiated BiDir),
/// two unused strings, then the machine-info OctetSeq.
pub fn parse_connect_request(message: &[u8]) -> Result<ConnectRequest, ConnectError> {
    let request = Request::parse(message).map_err(|e| ConnectError::Giop(format!("{e}")))?;
    let body_offset = message.len() - request.body.len();
    let mut r = CdrReader::new(message, body_offset);
    let truncated = |_| ConnectError::Truncated;
    let type_id = r.string().map_err(truncated)?;
    let profiles = r.u32().map_err(truncated)?;
    for _ in 0..profiles {
        r.u32().map_err(truncated)?; // profile tag
        r.skip_octet_sequence().map_err(truncated)?; // profile body
    }
    r.string().map_err(truncated)?; // unused1
    r.string().map_err(truncated)?; // unused2
    let octets = r.octet_sequence().map_err(truncated)?;
    Ok(ConnectRequest {
        callback_type_id: String::from_utf8_lossy(type_id).into_owned(),
        machine: MachineInfo::from_octets(octets)?,
    })
}

/// Build the `Connect` reply: the connection id (CORBA return value), the
/// identity OctetSeq, then the empty out string `sessKey`.
pub fn build_connect_reply(request_id: u32, identity: &ConnectReply) -> Vec<u8> {
    let mut body = CdrWriter::with_origin(0);
    body.u32(identity.connection_id);
    body.octet_sequence(&identity.to_octets());
    body.string(b"");
    build_reply(
        request_id,
        ReplyStatus::NoException,
        &[],
        &body.into_bytes(),
    )
}

/// Parse a `Connect` reply message into the server identity.
pub fn parse_connect_reply(message: &[u8]) -> Result<ConnectReply, ConnectError> {
    let reply = Reply::parse(message).map_err(|e| ConnectError::Giop(format!("{e}")))?;
    if reply.status != ReplyStatus::NoException {
        return Err(ConnectError::Status(format!("{:?}", reply.status)));
    }
    // Reply body offset within the message, so CDR alignment matches the wire.
    let body_offset = message.len() - reply.body.len();
    let mut r = CdrReader::new(message, body_offset);
    let connection_id = r.u32().map_err(|_| ConnectError::Truncated)?;
    let octets = r.octet_sequence().map_err(|_| ConnectError::Truncated)?;
    parse_identity(connection_id, octets)
}

fn parse_identity(connection_id: u32, octets: &[u8]) -> Result<ConnectReply, ConnectError> {
    let connection_key: [u8; 16] = octets
        .get(..16)
        .ok_or(ConnectError::Truncated)?
        .try_into()
        .unwrap();
    let mut pos = 16;
    let exe_type = take_u8(octets, &mut pos)?;
    let app_type = take_u8(octets, &mut pos)?;
    let remote_name = take_string(octets, &mut pos)?;
    let _empty_install_slot = take_string(octets, &mut pos)?;
    let version_string = take_string(octets, &mut pos)?;
    let _empty_mac_slot = take_string(octets, &mut pos)?;
    let system_language = take_u8(octets, &mut pos)?;
    let protocol_version = take_u8(octets, &mut pos)?;
    let install_code = take_string(octets, &mut pos)?;
    let web_service_settings = take_string(octets, &mut pos)?;
    let standby_address = take_string(octets, &mut pos)?;
    Ok(ConnectReply {
        connection_id,
        connection_key,
        exe_type,
        app_type,
        remote_name,
        version_string,
        system_language,
        protocol_version,
        install_code,
        web_service_settings,
        standby_address,
    })
}

fn take_u8(buf: &[u8], pos: &mut usize) -> Result<u8, ConnectError> {
    let v = *buf.get(*pos).ok_or(ConnectError::Truncated)?;
    *pos += 1;
    Ok(v)
}

fn take_string(buf: &[u8], pos: &mut usize) -> Result<String, ConnectError> {
    let rest = buf.get(*pos..).ok_or(ConnectError::Truncated)?;
    let (text, used) =
        decode_field(rest, Framing::LeadingU8).map_err(|_| ConnectError::BadString)?;
    *pos += used;
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn machine_info_debug_hides_the_client_license() {
        let info = MachineInfo {
            install_code: "synthetic-license-for-diagnostics".into(),
            ..MachineInfo::default()
        };
        let debug = format!("{info:?}");
        assert!(debug.contains("[redacted]"));
        assert!(!debug.contains(&info.install_code));
    }
    use om_giop::Request;

    #[test]
    fn connect_request_structure_round_trips() {
        let info = MachineInfo::default();
        let msg = build_connect(4, "10.0.0.5", 0x138b, &info);
        let req = Request::parse(&msg).unwrap();
        assert_eq!(req.operation, op::CONNECT);
        assert_eq!(req.object_key, OBJECT_KEY);
        // Body starts with the callback IOR type id.
        assert!(req.body.windows(REPO_ID.len()).any(|w| w == REPO_ID));
    }

    #[test]
    fn machine_info_octets_reparse() {
        let info = MachineInfo {
            install_code: String::new(),
            ..MachineInfo::default()
        };
        let octets = info.to_octets();
        // Re-read with the reply-shaped parser's string walker to confirm framing:
        // skip the 2 leading bytes, read machineName.
        let (name, _) = decode_field(&octets[2..], Framing::LeadingU8).unwrap();
        assert_eq!(name, "openmaker");
    }

    #[test]
    fn server_parses_client_connect_request() {
        let info = MachineInfo {
            machine_name: "workstation".into(),
            version_string: "Pro 26.0.3".into(),
            ..MachineInfo::default()
        };
        let msg = build_connect(4, "10.0.0.5", 0x138b, &info);
        let req = parse_connect_request(&msg).unwrap();
        assert_eq!(req.callback_type_id, "IDL:RCFMP12:1.0");
        assert_eq!(req.machine.machine_name, "workstation");
        assert_eq!(req.machine.version_string, "Pro 26.0.3");
        assert_eq!(req.machine.protocol_version, 0x20);
        assert_eq!(req.machine.to_octets(), info.to_octets());
    }

    #[test]
    fn connect_reply_round_trips() {
        let identity = ConnectReply {
            connection_id: 0x463b_59ad,
            connection_key: *b"0123456789abcdef",
            exe_type: 0x22,
            app_type: 0x03,
            remote_name: "server".into(),
            version_string: "Server 26.0.3".into(),
            system_language: 0x15,
            protocol_version: 0x20,
            install_code: "00112233445566778899aabbccddeeff".into(),
            web_service_settings: "port=443;ssl=true".into(),
            standby_address: String::new(),
        };
        let msg = build_connect_reply(4, &identity);
        assert_eq!(parse_connect_reply(&msg).unwrap(), identity);
    }
}
