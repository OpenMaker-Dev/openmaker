//! The certificate authorities OpenMaker Go keeps for its saved hosts, one
//! set of rules for both apps (docs/mobile-client.md, "Hosts").
//!
//! The apps store them as a map from `address:port`, the address as typed,
//! to PEM text. The Hosts tab of earlier versions could save one under each
//! spelling of an address; a host's identity ([`host_key`]) is its address
//! as DNS compares it, so all of them belong to the host.

use std::collections::{BTreeMap, HashMap};

use rustls::pki_types::CertificateDer;

use crate::GoError;

/// A host's address and port.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct Endpoint {
    pub address: String,
    pub port: u16,
}

/// A host's identity: its address as DNS compares it, an international
/// name in its ASCII (punycode) form and letters in ASCII lower case, never
/// a locale's or Unicode's case folding, with its port. Two saved hosts with
/// the same key are the same host.
#[uniffi::export]
pub fn host_key(address: String, port: u16) -> String {
    key(&address, port)
}

fn key(address: &str, port: u16) -> String {
    let address = address.trim();
    // IP addresses and names that are not valid domain names keep their
    // text, in ASCII lower case.
    let ascii = idna::domain_to_ascii(address)
        .ok()
        .filter(|ascii| !ascii.is_empty())
        .unwrap_or_else(|| address.to_owned());
    format!("{}:{port}", ascii.to_ascii_lowercase())
}

fn endpoint_key(endpoint: &Endpoint) -> String {
    key(&endpoint.address, endpoint.port)
}

/// The host and port a stored key (`address:port`) names, by [`key`].
fn stored_key(stored: &str) -> Option<String> {
    let (address, port) = stored.rsplit_once(':')?;
    Some(key(address, port.parse().ok()?))
}

/// The certificate authorities trusted for `address` and `port`: those
/// saved under any spelling of the address, the one under this spelling
/// first and the rest in order, each certificate once, as PEM. A block that
/// is not a certificate a trust store takes is left out: one malformed
/// authority saved long ago under another spelling must not stop the
/// others working.
#[uniffi::export]
pub fn trusted_certificate(saved: HashMap<String, String>, address: String, port: u16) -> String {
    pem_of(&trusted(&saved, &Endpoint { address, port }))
}

fn trusted(saved: &HashMap<String, String>, endpoint: &Endpoint) -> Vec<CertificateDer<'static>> {
    let target = endpoint_key(endpoint);
    let exact = format!("{}:{}", endpoint.address, endpoint.port);
    let spellings: BTreeMap<(bool, &String), &String> = saved
        .iter()
        .filter(|(stored, _)| stored_key(stored).as_deref() == Some(target.as_str()))
        .map(|(stored, pem)| ((*stored != exact, stored), pem))
        .collect();
    let mut certificates = Vec::new();
    for pem in spellings.values() {
        add_unique(&mut certificates, usable(pem));
    }
    certificates
}

/// What a save leaves trusted, for Add Host (`edited` is `None`) or Edit
/// Host of `edited`. `loaded` is the text Edit Host showed when it opened:
/// if the authorities trusted for `edited` changed since (another window
/// saved), the save is refused rather than undo that change. The host is
/// saved at `host`, trusting `certificate` (what the editor holds now) and
/// the authorities of the entries it replaces that the editor did not
/// show; `replaced` are the other entries the save replaces (the entry in
/// its place, and other entries for the same host). The authorities of
/// every replaced entry, `edited` and `host`, under any spelling, are
/// replaced by the result. When the edit moves the host to another address
/// or port, an authority it showed that was saved only under another
/// spelling of the old address stays behind: the user could not tell it
/// came from there.
#[uniffi::export]
pub fn save_trusted_certificates(
    saved: HashMap<String, String>,
    edited: Option<Endpoint>,
    loaded: String,
    host: Endpoint,
    certificate: String,
    replaced: Vec<Endpoint>,
) -> Result<HashMap<String, String>, GoError> {
    let typed = readable(&certificate)?;
    let host_key = endpoint_key(&host);
    let mut kept = Vec::new();
    match &edited {
        Some(edited) => {
            if trusted(&saved, edited) != usable(&loaded) {
                return Err(GoError::failed(
                    "This host's certificate authority was changed while it was being edited. \
                     Cancel, and edit the host again.",
                ));
            }
            if endpoint_key(edited) == host_key {
                add_unique(&mut kept, typed);
            } else {
                // Moved: what came only from another spelling stays.
                let exact = format!("{}:{}", edited.address, edited.port);
                let own = saved.get(&exact).map(|pem| usable(pem)).unwrap_or_default();
                let elsewhere = trusted(&saved, edited);
                add_unique(
                    &mut kept,
                    typed
                        .into_iter()
                        .filter(|der| own.contains(der) || !elsewhere.contains(der))
                        .collect(),
                );
            }
        }
        None => add_unique(&mut kept, typed),
    }
    // What the editor did not show: the host's own place and the entries
    // the save merges into it.
    let seen = edited.as_ref().map(endpoint_key);
    for unseen in std::iter::once(&host).chain(&replaced) {
        if seen.as_deref() != Some(endpoint_key(unseen).as_str()) {
            add_unique(&mut kept, trusted(&saved, unseen));
        }
    }
    let gone: Vec<String> = std::iter::once(&host)
        .chain(&replaced)
        .chain(&edited)
        .map(endpoint_key)
        .collect();
    let mut result: HashMap<String, String> = saved
        .into_iter()
        .filter(|(stored, _)| !stored_key(stored).is_some_and(|key| gone.contains(&key)))
        .collect();
    if !kept.is_empty() {
        result.insert(format!("{}:{}", host.address, host.port), pem_of(&kept));
    }
    Ok(result)
}

/// `saved` without the authorities of `endpoints`, under any spelling.
#[uniffi::export]
pub fn forget_trusted_certificates(
    saved: HashMap<String, String>,
    endpoints: Vec<Endpoint>,
) -> HashMap<String, String> {
    let gone: Vec<String> = endpoints.iter().map(endpoint_key).collect();
    saved
        .into_iter()
        .filter(|(stored, _)| !stored_key(stored).is_some_and(|key| gone.contains(&key)))
        .collect()
}

/// The certificates in `text` a trust store takes, each once; the rest are
/// left out.
fn usable(text: &str) -> Vec<CertificateDer<'static>> {
    let mut out = Vec::new();
    for item in rustls_pemfile::certs(&mut text.as_bytes()) {
        let Ok(der) = item else { continue };
        if takes(&der) && !out.contains(&der) {
            out.push(der);
        }
    }
    out
}

/// Text typed as a certificate authority: empty, or certificates a trust
/// store takes and nothing else.
pub(crate) fn readable(text: &str) -> Result<Vec<CertificateDer<'static>>, GoError> {
    let refused = || GoError::failed("The certificate authority is not a PEM certificate.");
    let mut out = Vec::new();
    for item in rustls_pemfile::read_all(&mut text.as_bytes()) {
        match item.map_err(|_| refused())? {
            rustls_pemfile::Item::X509Certificate(der) if takes(&der) => {
                if !out.contains(&der) {
                    out.push(der);
                }
            }
            _ => return Err(refused()),
        }
    }
    if out.is_empty() && !text.trim().is_empty() {
        return Err(refused());
    }
    Ok(out)
}

/// Whether a trust store takes `der` as an authority, as the engine adds
/// a host's authorities (`om_session::Transport::connect_verified`).
fn takes(der: &CertificateDer<'_>) -> bool {
    rustls::RootCertStore::empty().add(der.clone()).is_ok()
}

fn add_unique(into: &mut Vec<CertificateDer<'static>>, from: Vec<CertificateDer<'static>>) {
    for der in from {
        if !into.contains(&der) {
            into.push(der);
        }
    }
}

/// Certificates as PEM, the base64 in lines of 64.
fn pem_of(certificates: &[CertificateDer<'_>]) -> String {
    use base64::Engine;
    certificates
        .iter()
        .map(|der| {
            let body = base64::engine::general_purpose::STANDARD.encode(der.as_ref());
            let lines: Vec<&str> = body
                .as_bytes()
                .chunks(64)
                .map(|line| std::str::from_utf8(line).unwrap_or_default())
                .collect();
            format!(
                "-----BEGIN CERTIFICATE-----\n{}\n-----END CERTIFICATE-----",
                lines.join("\n")
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn certificate(name: &str) -> String {
        rcgen::generate_simple_self_signed(vec![name.into()])
            .unwrap()
            .cert
            .pem()
    }

    fn endpoint(address: &str) -> Endpoint {
        Endpoint {
            address: address.into(),
            port: 5003,
        }
    }

    fn saved(entries: &[(&str, &str)]) -> HashMap<String, String> {
        entries
            .iter()
            .map(|(key, pem)| ((*key).to_owned(), (*pem).to_owned()))
            .collect()
    }

    /// Names compare as DNS compares them: ASCII case and an international
    /// name's punycode form, never Unicode case folding, under which the
    /// dotless ı would match i.
    #[test]
    fn hosts_are_the_same_only_as_dns_names_are() {
        let key = |address: &str| host_key(address.into(), 5003);
        assert_eq!(key("Server.Example"), key("server.example"));
        assert_eq!(key("BÜCHER.example"), key("xn--bcher-kva.example"));
        assert_ne!(key("ı.example"), key("i.example"));
        assert_ne!(
            key("server.example"),
            host_key("server.example".into(), 5004)
        );
        assert_eq!(key("10.250.0.1"), "10.250.0.1:5003");
        assert_eq!(key("FE80::1"), "fe80::1:5003");
    }

    /// An authority saved under another spelling that does not read is
    /// left out; the one that reads still works. The same certificate with
    /// other line endings is one.
    #[test]
    fn unreadable_or_repeated_authorities_do_not_count() {
        let good = certificate("server.example");
        let crlf = good.replace('\n', "\r\n");
        let store = saved(&[
            ("server.example:5003", &good),
            (
                "SERVER.example:5003",
                "-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----",
            ),
            ("Server.Example:5003", &crlf),
        ]);
        let trusted = trusted_certificate(store, "server.example".into(), 5003);
        assert_eq!(usable(&trusted), usable(&good));
        assert_eq!(trusted.matches("BEGIN CERTIFICATE").count(), 1);
    }

    /// Add Host with an empty field keeps every authority saved under any
    /// spelling of the host, and an Edit Host that cleared what it showed
    /// removes them all.
    #[test]
    fn a_save_keeps_what_the_editor_did_not_show() {
        let (a, b) = (certificate("a"), certificate("b"));
        let store = saved(&[("SERVER.example:5003", &a), ("server.example:5003", &b)]);
        let added = save_trusted_certificates(
            store,
            None,
            String::new(),
            endpoint("Server.Example"),
            String::new(),
            vec![endpoint("server.example")],
        )
        .unwrap();
        assert_eq!(added.len(), 1);
        let shown = trusted_certificate(added.clone(), "Server.Example".into(), 5003);
        assert_eq!(usable(&shown).len(), 2);
        let cleared = save_trusted_certificates(
            added,
            Some(endpoint("Server.Example")),
            shown,
            endpoint("Server.Example"),
            String::new(),
            vec![],
        )
        .unwrap();
        assert!(cleared.is_empty());
    }

    /// An editor whose host's authorities changed since it opened (saved
    /// from another window) is refused: it would erase one it never saw, or
    /// bring back one that was cleared.
    #[test]
    fn a_stale_editor_is_refused() {
        let (a, b) = (certificate("a"), certificate("b"));
        let store = saved(&[("server.example:5003", &b)]);
        let save = |store, loaded: &str| {
            save_trusted_certificates(
                store,
                Some(endpoint("server.example")),
                loaded.into(),
                endpoint("server.example"),
                loaded.into(),
                vec![],
            )
        };
        assert!(save(store.clone(), &a).is_err());
        assert!(save(HashMap::new(), &a).is_err());
        assert!(save(store, &b).is_ok());
    }

    /// Moving a host to another address takes what was saved under its own
    /// spelling and what was typed, but not what was saved only under
    /// another spelling of the old address.
    #[test]
    fn a_moved_host_leaves_other_spellings_behind() {
        let (own, other, typed) = (certificate("own"), certificate("other"), certificate("new"));
        let store = saved(&[
            ("server.example:5003", &own),
            ("SERVER.EXAMPLE:5003", &other),
        ]);
        let shown = trusted_certificate(store.clone(), "server.example".into(), 5003);
        let moved = save_trusted_certificates(
            store,
            Some(endpoint("server.example")),
            shown.clone(),
            endpoint("files.example"),
            format!("{shown}\n{typed}"),
            vec![],
        )
        .unwrap();
        assert_eq!(moved.len(), 1);
        let kept = usable(&moved["files.example:5003"]);
        assert_eq!(kept, [usable(&own), usable(&typed)].concat());
    }

    #[test]
    fn text_that_is_not_a_certificate_is_refused() {
        let refused = |text: &str| {
            save_trusted_certificates(
                HashMap::new(),
                None,
                String::new(),
                endpoint("server.example"),
                text.into(),
                vec![],
            )
            .is_err()
        };
        assert!(refused("not a certificate"));
        assert!(refused(
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----"
        ));
        assert!(!refused(""));
        assert!(!refused(&certificate("server.example")));
    }
}
