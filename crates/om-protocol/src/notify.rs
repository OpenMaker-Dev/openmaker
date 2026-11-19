//! What a host sends a client on its own: `Notify` calls on the client's
//! callback object, the `DBMesg` records they carry, and the
//! administrator's messages among them (Send Message, the requests to close
//! a file or every file, and the forced disconnect).
//!
//! `Notify(in ulong connectionid, in string sessKey, in sequence<octet>
//! parms, out sequence<octet> backchannel)`: `parms` is the notification
//! back channel, `ff 00` and one `{u32 BE code, u16 BE length, DBMesg}`
//! record per message ([`crate::write::notification_backchannel`]). The
//! client answers NO_EXCEPTION with its own back channel (FileMaker Pro's
//! CPU report, `fe <cpu> <0-3 bytes>`; an empty one is accepted). Before its
//! first call of a session the host sends a GIOP LocateRequest for the
//! callback object ([`CALLBACK_KEY`]), which the client answers OBJECT_HERE.
//!
//! A `DBMesg` is a 16-byte header (send-to session, reply-to session, the
//! attention code, the number of items, the offset of the last item's
//! length word) and its items, each followed by its `u16 BE` length.
//!
//! Spec: `docs/re/network-protocol.md` 6.5, `docs/re/write-path.md` 6.3-6.4.

use om_giop::{build_reply, build_request, Reader as CdrReader, Reply, ReplyStatus};

use crate::write::Notification;
use crate::{op, PerformError, FILE_SESSION_ID, OBJECT_KEY};

/// The object key of a client's callback object, the target of the host's
/// calls: the same bytes as the host's own object key (6.5).
pub const CALLBACK_KEY: &[u8] = OBJECT_KEY;

/// `DBAttnCode` values of the administrator's messages (FileMaker Server's
/// constants, write-path.md 6.2). A message's code is one of these, with
/// [`HOST_SERIAL`] when its item 1 is the host's file id.
pub mod attention {
    /// `kDBAttnMessage`: a message to show.
    pub const MESSAGE: u32 = 0x10;
    /// `kDBAttnMaxIdleTime`.
    pub const MAX_IDLE_TIME: u32 = 0x30;
    /// `kDBAttnCloseFile`: close this file.
    pub const CLOSE_FILE: u32 = 0x40;
    /// `kDBAttnCloseAll`: close every file.
    pub const CLOSE_ALL: u32 = 0x50;
    /// `kDBAttnDisconnect`.
    pub const DISCONNECT: u32 = 0x60;
    /// `kDBAttnDisconnectAll`: the host disconnects the client now.
    pub const DISCONNECT_ALL: u32 = 0x70;
    /// `kDBAttnHostSerial`: item 1 is the host's file id, not the client's.
    pub const HOST_SERIAL: u32 = 0x800;
    /// `kDBAttnLocal`.
    pub const LOCAL: u32 = 0x1000;
}

/// Bytes of a `DBMesg` header.
const HEADER_LEN: usize = 16;

/// The countdown FileMaker Server names in its requests to close: FileMaker
/// Pro closes the files 30 s after the request (6.5).
pub const CLOSE_COUNTDOWN: u32 = 30;

/// A message too long for the `u16` lengths of a `DBMesg` or its record.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MessageTooLong;

impl core::fmt::Display for MessageTooLong {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("the message is too long for a notification")
    }
}

impl std::error::Error for MessageTooLong {}

/// A marshalled `DBMesg` (write-path.md 6.3).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DbMesg<'a> {
    /// The session the message is for: 1, the client's file session.
    pub send_to: u32,
    /// The session to answer: 0.
    pub reply_to: u32,
    /// The attention code.
    pub code: u32,
    /// The items, in order.
    pub items: Vec<&'a [u8]>,
}

impl DbMesg<'_> {
    /// The message's bytes: the header, then each item followed by its
    /// length.
    pub fn marshal(&self) -> Result<Vec<u8>, MessageTooLong> {
        let count = u16::try_from(self.items.len()).map_err(|_| MessageTooLong)?;
        let mut body = Vec::new();
        let mut last_length_word = 0;
        for item in &self.items {
            let len = u16::try_from(item.len()).map_err(|_| MessageTooLong)?;
            body.extend_from_slice(item);
            last_length_word = HEADER_LEN + body.len();
            body.extend(len.to_be_bytes());
        }
        let last_length_word = u16::try_from(last_length_word).map_err(|_| MessageTooLong)?;
        let mut out = Vec::with_capacity(HEADER_LEN + body.len());
        out.extend(self.send_to.to_be_bytes());
        out.extend(self.reply_to.to_be_bytes());
        out.extend(self.code.to_be_bytes());
        out.extend(count.to_be_bytes());
        out.extend(last_length_word.to_be_bytes());
        out.extend(body);
        // A record carries the message with a `u16` length too.
        u16::try_from(out.len()).map_err(|_| MessageTooLong)?;
        Ok(out)
    }
}

/// Read a marshalled `DBMesg`, or `None` when it is not one. Its items are
/// found from the last length word back to the first item, which must
/// start right after the header.
pub fn parse_dbmesg(bytes: &[u8]) -> Option<DbMesg<'_>> {
    let word = |at: usize| -> Option<usize> {
        Some(usize::from(u16::from_be_bytes(
            bytes.get(at..at + 2)?.try_into().ok()?,
        )))
    };
    let u32_at = |at: usize| -> Option<u32> {
        Some(u32::from_be_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
    };
    let header = DbMesg {
        send_to: u32_at(0)?,
        reply_to: u32_at(4)?,
        code: u32_at(8)?,
        items: Vec::new(),
    };
    let count = word(12)?;
    if count == 0 {
        return (bytes.len() == HEADER_LEN).then_some(header);
    }
    let last = word(14)?;
    if last + 2 != bytes.len() {
        return None;
    }
    // Each item takes at least its length word.
    let mut items = Vec::with_capacity(count.min(bytes.len() / 2));
    // Where the item being read ends: its length word.
    let mut end = last;
    for _ in 0..count {
        let start = end.checked_sub(word(end)?)?;
        if start < HEADER_LEN {
            return None;
        }
        items.push(&bytes[start..end]);
        end = start.checked_sub(2)?;
    }
    if end + 2 != HEADER_LEN {
        return None;
    }
    items.reverse();
    Some(DbMesg { items, ..header })
}

/// Whether `code` is one of the administrator's messages rather than a
/// change notification (whose codes are a change kind and a scope,
/// write-path.md 6.2).
pub fn is_admin_code(code: u32) -> bool {
    use attention::*;
    matches!(
        code & !(HOST_SERIAL | LOCAL),
        MESSAGE | MAX_IDLE_TIME | CLOSE_FILE | CLOSE_ALL | DISCONNECT | DISCONNECT_ALL
    )
}

/// One of the administrator's messages to a client (6.5): its code and
/// four items.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdminMessage {
    /// What it asks: [`attention::MESSAGE`], [`attention::CLOSE_FILE`],
    /// [`attention::CLOSE_ALL`] or [`attention::DISCONNECT_ALL`], with
    /// [`attention::HOST_SERIAL`] when [`AdminMessage::file`] is the host's
    /// file id.
    pub code: u32,
    /// Item 1, the file: the host's file id (the Open reply's), or with a
    /// code without [`attention::HOST_SERIAL`] the client's own id for it
    /// (the one it registered with op63, write-path.md 6.1).
    pub file: i32,
    /// Item 2, the administrator's text (UTF-16BE on the wire).
    pub text: String,
    /// Item 3, the host's name (`remoteName`, network-protocol.md 5.2).
    pub server_name: String,
    /// Item 4: the client's countdown in seconds in a request to close
    /// ([`CLOSE_COUNTDOWN`]), 0 otherwise.
    pub countdown: u32,
}

impl AdminMessage {
    /// Send Message: `text` to show, naming the host's file `host_file`
    /// (code `0x0810`).
    pub fn message(host_file: u32, text: &str, server_name: &str) -> Self {
        Self {
            code: attention::HOST_SERIAL | attention::MESSAGE,
            file: host_file as i32,
            text: text.to_owned(),
            server_name: server_name.to_owned(),
            countdown: 0,
        }
    }

    /// Disconnect's request to close every file (code `0x0850`) within the
    /// client's [`CLOSE_COUNTDOWN`], naming the host's file `host_file`.
    pub fn close_all(host_file: u32, text: &str, server_name: &str) -> Self {
        Self {
            code: attention::HOST_SERIAL | attention::CLOSE_ALL,
            countdown: CLOSE_COUNTDOWN,
            ..Self::message(host_file, text, server_name)
        }
    }

    /// Close's request to close one file (code `0x0040`) within the
    /// client's [`CLOSE_COUNTDOWN`], naming it by the client's own id for
    /// it (the one it registered with op63).
    pub fn close_file(client_file: i32, text: &str, server_name: &str) -> Self {
        Self {
            code: attention::CLOSE_FILE,
            file: client_file,
            countdown: CLOSE_COUNTDOWN,
            ..Self::message(0, text, server_name)
        }
    }

    /// The notice that the host disconnects the client now, its grace time
    /// over (code `0x0870`): FileMaker Pro logs out and disconnects at once.
    pub fn disconnect_all(host_file: u32, text: &str, server_name: &str) -> Self {
        Self {
            code: attention::HOST_SERIAL | attention::DISCONNECT_ALL,
            ..Self::message(host_file, text, server_name)
        }
    }

    /// The notification record that carries the message, addressed to the
    /// client's file session.
    pub fn to_notification(&self) -> Result<Notification, MessageTooLong> {
        let file = self.file.to_be_bytes();
        let text = utf16_be(&self.text);
        let server_name = utf16_be(&self.server_name);
        let countdown = self.countdown.to_be_bytes();
        let message = DbMesg {
            send_to: FILE_SESSION_ID,
            reply_to: 0,
            code: self.code,
            items: vec![&file, &text, &server_name, &countdown],
        }
        .marshal()?;
        Ok(Notification {
            code: self.code,
            message,
        })
    }

    /// Read an administrator's message from its record, or `None` when it
    /// is not one (a change notification, or not four items of the right
    /// sizes). Text that is not UTF-16 is read with replacement characters.
    pub fn parse(notification: &Notification) -> Option<Self> {
        if !is_admin_code(notification.code) {
            return None;
        }
        let message = parse_dbmesg(&notification.message)?;
        let [file, text, server_name, countdown] = message.items[..] else {
            return None;
        };
        Some(Self {
            code: message.code,
            file: i32::from_be_bytes(file.try_into().ok()?),
            text: from_utf16_be(text)?,
            server_name: from_utf16_be(server_name)?,
            countdown: u32::from_be_bytes(countdown.try_into().ok()?),
        })
    }
}

fn utf16_be(text: &str) -> Vec<u8> {
    text.encode_utf16().flat_map(u16::to_be_bytes).collect()
}

/// UTF-16BE text, or `None` for an odd number of bytes.
fn from_utf16_be(bytes: &[u8]) -> Option<String> {
    let (units, rest) = bytes.as_chunks::<2>();
    rest.is_empty().then(|| {
        let units: Vec<u16> = units.iter().map(|unit| u16::from_be_bytes(*unit)).collect();
        String::from_utf16_lossy(&units)
    })
}

/// Build a `Notify` call carrying `parms` (the `ff 00` form) to the
/// callback object of the client of connection `connection_id`.
pub fn build_notify(request_id: u32, connection_id: u32, parms: &[u8]) -> Vec<u8> {
    // The body starts 8-aligned and needs at most 4-byte alignment, so
    // authoring it at origin 0 pads as at the real offset.
    let mut body = om_giop::Writer::with_origin(0);
    body.u32(connection_id);
    body.string(b"");
    body.octet_sequence(parms);
    build_request(
        request_id,
        true,
        CALLBACK_KEY,
        op::NOTIFY,
        &[],
        &body.into_bytes(),
    )
}

/// The arguments of a `Notify` call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NotifyRequest<'a> {
    /// The connection id `Connect` handed the client.
    pub connection_id: u32,
    pub session: &'a [u8],
    /// The notification back channel (`ff 00` and records).
    pub parms: &'a [u8],
}

/// Parse a `Notify` call's body ([`om_giop::Request::body`]).
pub fn parse_notify_request(body: &[u8]) -> Option<NotifyRequest<'_>> {
    let mut r = CdrReader::new(body, 0);
    Some(NotifyRequest {
        connection_id: r.u32().ok()?,
        session: r.string().ok()?,
        parms: r.octet_sequence().ok()?,
    })
}

/// Build the client's answer to a `Notify` call: NO_EXCEPTION and its out
/// `backchannel`.
pub fn build_notify_reply(request_id: u32, backchannel: &[u8]) -> Vec<u8> {
    let mut body = om_giop::Writer::with_origin(0);
    body.octet_sequence(backchannel);
    build_reply(
        request_id,
        ReplyStatus::NoException,
        &[],
        &body.into_bytes(),
    )
}

/// The out `backchannel` of a `Notify` reply message.
pub fn parse_notify_reply(message: &[u8]) -> Result<Vec<u8>, PerformError> {
    let reply = Reply::parse(message).map_err(|e| PerformError::Giop(format!("{e}")))?;
    if reply.status != ReplyStatus::NoException {
        return Err(PerformError::Status(format!("{:?}", reply.status)));
    }
    let body_offset = message.len() - reply.body.len();
    CdrReader::new(message, body_offset)
        .octet_sequence()
        .map(<[u8]>::to_vec)
        .map_err(|_| PerformError::Truncated)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::write::{notification_backchannel, parse_notification_backchannel};
    use om_giop::Request;

    fn hex(s: &str) -> Vec<u8> {
        let s: String = s.split_whitespace().collect();
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    }

    /// Send Message's `parms`, s07 conn-000 request 5 (network-protocol.md
    /// 6.5), with the server's name as `server`.
    const SEND_MESSAGE_PARMS: &str = "
        ff000000 08100098 00000001 00000000
        00000810 00040096 00000165 0004004f
        00700065 006e004d 0061006b 00650072
        00200063 00610070 00740075 00720065
        00200031 003a0020 00610020 00740065
        00730074 0020006d 00650073 00730061
        00670065 00200066 0072006f 006d0020
        00740068 00650020 00410064 006d0069
        006e0020 00410050 0049006c 00730065
        00720076 00650072 000c0000 00000004";

    const TEXT: &str = "OpenMaker capture 1: a test message from the Admin API";

    #[test]
    fn send_message_matches_the_capture() {
        let message = AdminMessage::message(0x165, TEXT, "server");
        let parms = notification_backchannel(&[message.to_notification().unwrap()]);
        assert_eq!(parms, hex(SEND_MESSAGE_PARMS));

        let [record] = &parse_notification_backchannel(&parms).unwrap()[..] else {
            panic!("one record");
        };
        assert_eq!(AdminMessage::parse(record), Some(message));
        let dbmesg = parse_dbmesg(&record.message).unwrap();
        assert_eq!((dbmesg.send_to, dbmesg.reply_to), (1, 0));
    }

    #[test]
    fn the_other_actions_differ_in_code_file_and_countdown() {
        // 6.5: Disconnect (0x0850, countdown 30), Close (0x0040 with the
        // session's op63 client file id -9, countdown 30) and the forced
        // notice (0x0870, countdown 0).
        for (code, file, countdown) in [(0x0850, 0x165, 30), (0x0040, -9, 30), (0x0870, 0x168, 0)] {
            let message = AdminMessage {
                code,
                file,
                text: TEXT.into(),
                server_name: "server".into(),
                countdown,
            };
            match code {
                0x0850 => assert_eq!(AdminMessage::close_all(0x165, TEXT, "server"), message),
                0x0040 => assert_eq!(AdminMessage::close_file(-9, TEXT, "server"), message),
                0x0870 => assert_eq!(AdminMessage::disconnect_all(0x168, TEXT, "server"), message),
                _ => {}
            }
            let mut expected = hex(SEND_MESSAGE_PARMS);
            expected[2..6].copy_from_slice(&code.to_be_bytes());
            expected[16..20].copy_from_slice(&code.to_be_bytes());
            expected[24..28].copy_from_slice(&file.to_be_bytes());
            let len = expected.len();
            expected[len - 6..len - 2].copy_from_slice(&countdown.to_be_bytes());
            let notification = message.to_notification().unwrap();
            assert_eq!(
                notification_backchannel(std::slice::from_ref(&notification)),
                expected
            );
            assert_eq!(AdminMessage::parse(&notification), Some(message));
        }
    }

    #[test]
    fn builds_and_reads_the_notify_call() {
        // s07 conn-000 request 5 as FileMaker Server sent it, the server's
        // name as `server` and the four runs of padding FileMaker Server
        // leaves uninitialized zeroed.
        let captured = [
            hex("47494f50 01020100 ec000000 05000000 03000000 00000000 17000000"),
            CALLBACK_KEY.to_vec(),
            hex("00 07000000 4e6f7469 667900 00 00000000 00000000 7e20adb6 01000000 00 000000 a0000000"),
            hex(SEND_MESSAGE_PARMS),
        ]
        .concat();
        let parms = hex(SEND_MESSAGE_PARMS);
        assert_eq!(build_notify(5, 0xb6ad_207e, &parms), captured);

        let request = Request::parse(&captured).unwrap();
        assert_eq!(
            (
                request.operation,
                request.object_key,
                request.response_expected
            ),
            (op::NOTIFY, CALLBACK_KEY, true)
        );
        assert_eq!(
            parse_notify_request(request.body),
            Some(NotifyRequest {
                connection_id: 0xb6ad_207e,
                session: b"",
                parms: &parms,
            })
        );

        // FileMaker Pro's answer: its CPU report in the out sequence.
        let reply = hex("47494f50 01020101 14000000 05000000 00000000 00000000 04000000 fe452532");
        assert_eq!(build_notify_reply(5, &[0xfe, 0x45, 0x25, 0x32]), reply);
        assert_eq!(
            parse_notify_reply(&reply).unwrap(),
            [0xfe, 0x45, 0x25, 0x32]
        );
    }

    #[test]
    fn tells_admin_messages_from_change_notifications() {
        for code in [0x0810, 0x0850, 0x0040, 0x0870] {
            assert!(is_admin_code(code), "{code:#x}");
        }
        for code in [0x0101, 0x0201, 0x0401, 0x0003] {
            assert!(!is_admin_code(code), "{code:#x}");
        }
        // A change notification is not read as a message.
        let record = Notification {
            code: 0x0201,
            message: hex(
                "00000001 00000000 00000201 0002 001e fffffffc 0004 0702800201050104 0008",
            ),
        };
        assert_eq!(AdminMessage::parse(&record), None);
        // But it is a DBMesg (write-path.md 6.3).
        let dbmesg = parse_dbmesg(&record.message).unwrap();
        assert_eq!(
            dbmesg.items,
            [&hex("fffffffc")[..], &hex("0702800201050104")[..]]
        );
    }

    #[test]
    fn rejects_malformed_messages() {
        let good = AdminMessage::message(1, "hi", "server")
            .to_notification()
            .unwrap();
        // Cut anywhere, or with a length word pointing outside: never a
        // panic, never a message.
        for len in 0..good.message.len() {
            assert_eq!(parse_dbmesg(&good.message[..len]), None, "cut at {len}");
        }
        let mut bad = good.clone();
        let last = bad.message.len() - 2;
        bad.message[last..].copy_from_slice(&0xffffu16.to_be_bytes());
        assert_eq!(AdminMessage::parse(&bad), None);
        let mut many = good.clone();
        many.message[12..14].copy_from_slice(&0xffffu16.to_be_bytes());
        assert_eq!(AdminMessage::parse(&many), None);
        // Text too long for its length word cannot be built.
        let huge = AdminMessage::message(1, &"x".repeat(40_000), "server");
        assert_eq!(huge.to_notification(), Err(MessageTooLong));
    }
}
