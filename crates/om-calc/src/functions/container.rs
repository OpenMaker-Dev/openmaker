//! Container encoding functions that work on values alone: `Base64Decode`,
//! `Base64Encode`, `Base64EncodeRFC`, `HexDecode`, `HexEncode`, `TextDecode`,
//! `TextEncode`, `CryptAuthCode`, `CryptDecrypt`, `CryptDecryptBase64`,
//! `CryptDigest`, `CryptEncrypt`, `CryptEncryptBase64`,
//! `CryptGenerateSignature`, `CryptVerifySignature` (spec
//! `docs/re/calc-functions.md` 5.4 and the container oracle answers in 9).
//! Container analysis (`GetContainerAttribute`, `GetHeight`, `GetWidth`,
//! `GetThumbnail`, `ReadQRCode`, `GetTextFromPDF`, `VerifyContainer`) reads
//! the data's streams in [`super::container_analysis`]; only `GetLiveText`
//! and `GetLiveTextAsJSON` are left to the host.
//!
//! Data arguments are bytes: container data is its main stream
//! ([`Container::data`]), any other value its text as UTF-8 (the Claris
//! pages). `Base64Encode` alone writes text line breaks as CRLF. Functions
//! that make data return a [`Container`] holding one `FILE` stream: the
//! decoders when given a file name, the crypt functions under a fixed name
//! (`digest.data`, `hmac.data`, `encrypted.data`, `signature.data`) and
//! `TextEncode` as `<encoding>.txt`. Decoded bytes returned as text are read
//! as UTF-8, invalid sequences shown as U+FFFD, CRLF and LF folded to the
//! return character.
//!
//! Errors (oracle): 17 for Base64 or hex that does not decode and for
//! bytes `TextDecode` cannot read, 4 for an unknown algorithm or encoding
//! name (checked first), 5 for an unusable RSA key, 2 for `TextDecode` of
//! empty container data. A wrong decryption key, an empty key or an empty
//! signature gives `?` with no error.

use super::{arg, Function, Keyword};
use crate::error::CalcError;
use crate::value::{Container, Ctx, Value};

mod crypt;
mod encodings;
mod sign;

pub(super) const FUNCTIONS: &[Function] = &[
    Function::pure("Base64Decode", base64_decode),
    Function::pure("Base64Encode", base64_encode),
    Function::pure("Base64EncodeRFC", base64_encode_rfc),
    Function::pure("CryptAuthCode", crypt_auth_code),
    Function::pure("CryptDecrypt", crypt_decrypt),
    Function::pure("CryptDecryptBase64", crypt_decrypt_base64),
    Function::pure("CryptDigest", crypt_digest),
    Function::pure("CryptEncrypt", crypt_encrypt),
    Function::pure("CryptEncryptBase64", crypt_encrypt_base64),
    Function::pure("CryptGenerateSignature", crypt_generate_signature),
    Function::pure("CryptVerifySignature", crypt_verify_signature),
    Function::pure("HexDecode", hex_decode),
    Function::pure("HexEncode", hex_encode),
    Function::pure("TextDecode", text_decode),
    Function::pure("TextEncode", text_encode),
];

pub(super) const KEYWORDS: &[Keyword] = &[];

type Result<T> = std::result::Result<T, CalcError>;

/// 17: Base64 or hex that does not decode, bytes `TextDecode` cannot read
/// ("Attempt to convert foreign character set to UTF-16 failed").
const FOREIGN_CHARSET: CalcError = err(17);
/// 4: an algorithm or encoding name the function does not know.
const UNKNOWN_NAME: CalcError = err(4);
/// 2: `TextDecode` of empty container data.
const NO_DATA: CalcError = err(2);

const fn err(code: u16) -> CalcError {
    match CalcError::from_code(code) {
        Some(e) => e,
        None => unreachable!(),
    }
}

/// The bytes of a data argument: container data as it is, anything else
/// (text a container field holds included, script oracle s34) its text as
/// UTF-8.
fn bytes(v: &Value, cx: &Ctx) -> Vec<u8> {
    match v {
        Value::Container(c) if !c.streams.is_empty() => c.data().to_vec(),
        other => other.to_text(cx).into_string().into_bytes(),
    }
}

fn text(v: &Value, cx: &Ctx) -> String {
    v.to_text(cx).into_string()
}

/// Text that is empty and not container data: the Base64 encoders and the
/// crypt functions give empty text for it.
fn empty_text(v: &Value) -> bool {
    !matches!(v, Value::Container(_)) && v.is_empty()
}

fn file(name: impl Into<String>, data: Vec<u8>) -> Value {
    Value::Container(Container::file(name, data))
}

/// Decoded bytes: a file named by `fileNameWithExtension` when one is
/// given and not empty, else text.
fn decoded(data: Vec<u8>, name: Option<&Value>, cx: &Ctx) -> Value {
    match name.map(|n| text(n, cx)).filter(|n| !n.is_empty()) {
        Some(name) => file(file_name(&name), data),
        None => Value::text(encodings::internal_returns(&String::from_utf8_lossy(&data))),
    }
}

/// The file name a decoder gives its result (oracle): [`base_name`], or
/// `Untitled.dat` when nothing is left.
fn file_name(raw: &str) -> String {
    let name = base_name(raw);
    if name.is_empty() {
        "Untitled.dat".to_string()
    } else {
        name
    }
}

/// A file name as FileMaker cleans it up for container data: the first
/// non-empty line (lines end at CR), without a `type:` prefix (a `:` before
/// the first `/`, as in `image:a.png`, `filewin:/C:/a.txt`) or directories,
/// remaining `:` as `.` and `\ * ? < > |` as `_`. `GetEmbeddingAsFile`
/// keeps an empty result.
pub(super) fn base_name(raw: &str) -> String {
    let line = raw.split('\r').find(|l| !l.is_empty()).unwrap_or("");
    let line = match (line.find(':'), line.find('/')) {
        (Some(colon), slash) if slash.map_or(true, |s| colon < s) => &line[colon + 1..],
        _ => line,
    };
    let base = line.rsplit('/').next().unwrap_or("");
    base.chars()
        .map(|c| match c {
            ':' => '.',
            '\\' | '*' | '?' | '<' | '>' | '|' => '_',
            other => other,
        })
        .collect()
}

/// `Base64Encode`: RFC 2045 layout, text line breaks as CRLF.
fn base64_encode(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    let data = match &args[0] {
        Value::Container(c) if !c.streams.is_empty() => c.data().to_vec(),
        other => encodings::crlf_lines(&text(other, cx)).into_bytes(),
    };
    cx.check_size(encodings::base64_len(2045, data.len()))?;
    Ok(Value::text(encodings::base64_encode_rfc(2045, &data)))
}

/// `Base64EncodeRFC`: the RFC number is read as a count (truncated, sign
/// dropped, modulo 2^32: `-1421` and `1421 + 2^32` are 1421); unknown
/// numbers are 4648.
fn base64_encode_rfc(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[1]) {
        return Ok(Value::empty_text());
    }
    let rfc = args[0].to_number(cx).as_long_long().unsigned_abs() as u32;
    let data = bytes(&args[1], cx);
    cx.check_size(encodings::base64_len(rfc, data.len()))?;
    Ok(Value::text(encodings::base64_encode_rfc(rfc, &data)))
}

fn base64_decode(args: &[Value], cx: &Ctx) -> Result<Value> {
    let data = encodings::base64_decode(&text(&args[0], cx)).ok_or(FOREIGN_CHARSET)?;
    Ok(decoded(data, arg(args, 1), cx))
}

fn hex_encode(args: &[Value], cx: &Ctx) -> Result<Value> {
    let data = bytes(&args[0], cx);
    cx.check_size(data.len().saturating_mul(2))?;
    Ok(Value::text(encodings::hex_encode(&data)))
}

fn hex_decode(args: &[Value], cx: &Ctx) -> Result<Value> {
    let data = encodings::hex_decode(&text(&args[0], cx)).ok_or(FOREIGN_CHARSET)?;
    Ok(decoded(data, arg(args, 1), cx))
}

/// `TextEncode`: a file `<encoding>.txt` (the name as typed, lower case).
/// `lineEndings` is truncated (`3.7` is LF); 1 and unknown values keep the
/// line endings.
fn text_encode(args: &[Value], cx: &Ctx) -> Result<Value> {
    let encoding = text(&args[1], cx);
    let mode = args[2].to_number(cx).as_long_long();
    let data = encodings::text_encode(&text(&args[0], cx), &encoding, mode)?;
    Ok(file(format!("{}.txt", encoding.to_ascii_lowercase()), data))
}

/// `TextDecode`: empty text stays empty; empty container data is error 2;
/// text that is not container data is decoded from its UTF-8 bytes
/// (`TextDecode ( "é" ; "iso-8859-1" )` is `Ã©`).
fn text_decode(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    let data = bytes(&args[0], cx);
    if data.is_empty() {
        return Err(NO_DATA);
    }
    let decoded = encodings::text_decode(&data, &text(&args[1], cx))?;
    Ok(Value::text(decoded))
}

fn crypt_digest(args: &[Value], cx: &Ctx) -> Result<Value> {
    let digest = sign::digest(&text(&args[1], cx), &bytes(&args[0], cx))?;
    Ok(file("digest.data", digest))
}

fn crypt_auth_code(args: &[Value], cx: &Ctx) -> Result<Value> {
    let mac = sign::hmac(
        &text(&args[1], cx),
        &bytes(&args[2], cx),
        &bytes(&args[0], cx),
    )?;
    Ok(file("hmac.data", mac))
}

/// The encrypted bytes of `data` under `key`, `None` for an empty key.
fn encrypted(data: &Value, key: &Value, cx: &Ctx) -> Option<Vec<u8>> {
    let key = bytes(key, cx);
    if key.is_empty() {
        return None;
    }
    match data {
        Value::Container(c) if !c.streams.is_empty() => crypt::encrypt(
            crypt::Plain::File {
                name: c.text.as_str(),
                data: c.data(),
            },
            &key,
        ),
        other => crypt::encrypt(crypt::Plain::Text(&text(other, cx)), &key),
    }
}

/// The value encrypted in `blob`, `?` when `key` does not open it.
fn decrypted(blob: &[u8], key: &Value, cx: &Ctx) -> Value {
    match crypt::decrypt(blob, &bytes(key, cx)) {
        Some(crypt::Decrypted::Text(text)) => Value::text(text),
        Some(crypt::Decrypted::File { name, data }) => file(name, data),
        None => Value::invalid(),
    }
}

/// `CryptEncrypt`: `encrypted.data`; empty text gives empty text and an
/// empty key `?`.
fn crypt_encrypt(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    Ok(encrypted(&args[0], &args[1], cx)
        .map_or_else(Value::invalid, |blob| file("encrypted.data", blob)))
}

/// `CryptEncryptBase64`: `CryptEncrypt` as RFC 4648 Base64.
fn crypt_encrypt_base64(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    Ok(
        encrypted(&args[0], &args[1], cx).map_or_else(Value::invalid, |blob| {
            Value::text(encodings::base64_encode_rfc(4648, &blob))
        }),
    )
}

fn crypt_decrypt(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    Ok(decrypted(&bytes(&args[0], cx), &args[1], cx))
}

fn crypt_decrypt_base64(args: &[Value], cx: &Ctx) -> Result<Value> {
    if empty_text(&args[0]) {
        return Ok(Value::empty_text());
    }
    let blob = encodings::base64_decode(&text(&args[0], cx)).ok_or(FOREIGN_CHARSET)?;
    Ok(decrypted(&blob, &args[1], cx))
}

fn crypt_generate_signature(args: &[Value], cx: &Ctx) -> Result<Value> {
    let signature = sign::generate_signature(
        &bytes(&args[0], cx),
        &text(&args[1], cx),
        &text(&args[2], cx),
        &text(&args[3], cx),
    )?;
    Ok(signature.map_or_else(Value::invalid, |s| file("signature.data", s)))
}

fn crypt_verify_signature(args: &[Value], cx: &Ctx) -> Result<Value> {
    let valid = sign::verify_signature(
        &bytes(&args[0], cx),
        &text(&args[1], cx),
        &text(&args[2], cx),
        &bytes(&args[3], cx),
    )?;
    Ok(valid.map_or_else(Value::invalid, Value::boolean))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::datetime::Date;
    use crate::locale::Locale;

    #[test]
    fn file_names() {
        for (raw, name) in [
            ("a.txt", "a.txt"),
            ("dir/a.txt", "a.txt"),
            ("a:b.txt", "b.txt"),
            ("a:b:c.txt", "b.c.txt"),
            ("a:b/c.txt", "c.txt"),
            ("a/b:c.txt", "b.c.txt"),
            ("filewin:/C:/a.txt", "a.txt"),
            ("a:", "Untitled.dat"),
            ("dir/", "Untitled.dat"),
            ("\r\rb", "b"),
            ("x.txt\ry.txt", "x.txt"),
            ("a\nb", "a\nb"),
            ("a*b?.txt", "a_b_.txt"),
            ("a\\b.txt", "a_b.txt"),
            (" a.txt ", " a.txt "),
        ] {
            assert_eq!(file_name(raw), name, "{raw:?}");
        }
    }

    #[test]
    fn encrypt_round_trip() {
        let en = Locale::en_us();
        let cx = Ctx::new(&en, Date::from_ymd(2026, 10, 28).unwrap());
        let key = Value::text("key");
        let sealed = crypt_encrypt(&[Value::text("a\nb"), key.clone()], &cx).unwrap();
        assert_eq!(sealed.to_text(&cx).as_str(), "encrypted.data");
        let opened = crypt_decrypt(&[sealed.clone(), key], &cx).unwrap();
        assert_eq!(opened.to_text(&cx).as_str(), "a\nb");
        assert!(crypt_decrypt(&[sealed, Value::text("other")], &cx)
            .unwrap()
            .is_invalid());
    }
}
