//! OpenMaker Go's saved hosts and the certificate authorities trusted for
//! them, by one set of rules for both apps (docs/mobile-client.md,
//! "Hosts"). The apps keep the list and the authorities ([`SavedHosts`]);
//! every rule about them is here.
//!
//! **A host's identity** ([`host_key`]) is its address in the form the
//! network stack and TLS use: an IP address as Rust's `IpAddr` writes it,
//! else a domain name as UTS #46 maps it to ASCII (punycode, lower case).
//! Add Host saves that form ([`save_host`]), so what the list shows, what
//! the client connects to and what its authorities are kept by are one
//! string. UTS #46 maps full-width digits to ASCII, as browsers do, so
//! `１２７.０.０.１` is saved, and connected to, as `127.0.0.1`. An
//! address that does not map is refused; one kept by an earlier version
//! is its own host, by its exact text.
//!
//! **Authorities** are stored as a map from `address:port`, the address as
//! saved, to PEM text. Earlier versions could keep one under each spelling
//! of an address; all of them belong to the host. A host trusts at most
//! [`MAX_CERTIFICATES`] certificates in [`MAX_PEM_BYTES`]. A save never
//! drops, truncates or rewrites a stored authority it cannot carry whole:
//! when the result would pass the bounds, or a value it would replace does
//! not read whole, it saves nothing and says why. Lookup never truncates
//! either; connecting refuses a host past the bounds ([`check_bundle`]).

use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;

use rustls::pki_types::CertificateDer;

use crate::GoError;

/// The most text a host's certificate authorities may take, typed or kept.
pub const MAX_PEM_BYTES: usize = 64 * 1024;
/// The most certificates a host may trust.
pub const MAX_CERTIFICATES: usize = 16;
/// The most text a stored value is read from at all: beyond it, a value is
/// read as nothing, and a save that would replace it is refused.
const MAX_STORED_BYTES: usize = 1024 * 1024;

/// A host of the Hosts list: its address and port, and the name it is
/// listed by, empty for its address.
#[derive(Clone, Debug, PartialEq, Eq, uniffi::Record)]
pub struct ListedHost {
    pub address: String,
    pub port: u16,
    pub name: String,
}

/// The Hosts list and the certificate authorities, by `address:port`.
#[derive(Clone, Debug, Default, PartialEq, Eq, uniffi::Record)]
pub struct SavedHosts {
    pub hosts: Vec<ListedHost>,
    pub certificates: HashMap<String, String>,
}

/// Add Host (`replaces` is `None`) or Edit Host of `replaces`, as typed.
#[derive(Clone, Debug, uniffi::Record)]
pub struct HostChange {
    /// The host Edit Host edits, as it was when the editor opened.
    pub replaces: Option<ListedHost>,
    /// The certificate authorities the editor showed when it opened.
    pub loaded: String,
    pub address: String,
    pub port: String,
    pub name: String,
    pub certificate: String,
}

/// A saved host, and the list and authorities with it saved.
#[derive(Clone, Debug, uniffi::Record)]
pub struct HostSaved {
    pub saved: SavedHosts,
    pub host: ListedHost,
}

/// A host's identity: its address as the network stack and TLS take it
/// (the module's notes) with its port. Two hosts with the same key are the
/// same host.
#[uniffi::export]
pub fn host_key(address: String, port: u16) -> String {
    key(&address, port)
}

/// The address `address` is saved and connected as, or `None` when it maps
/// to none.
pub(crate) fn canonical_address(address: &str) -> Option<String> {
    let address = address.trim();
    if address.is_ascii() {
        if let Ok(ip) = address.parse::<IpAddr>() {
            return Some(ip.to_string());
        }
    }
    let ascii = idna::domain_to_ascii(address).ok()?;
    // An empty label is no name; one trailing dot is a full name.
    let labels = ascii.strip_suffix('.').unwrap_or(&ascii);
    if labels.is_empty() || !ascii.is_ascii() || labels.split('.').any(str::is_empty) {
        return None;
    }
    // Full-width digits map to an IP address.
    Some(match ascii.parse::<IpAddr>() {
        Ok(ip) => ip.to_string(),
        Err(_) => ascii,
    })
}

fn key(address: &str, port: u16) -> String {
    match canonical_address(address) {
        Some(address) => format!("{address}:{port}"),
        // Kept by an earlier version and mapping to nothing: its own host.
        None => format!("?{}:{port}", address.trim()),
    }
}

fn host_of(host: &ListedHost) -> String {
    key(&host.address, host.port)
}

/// The host a stored authority's key (`address:port`) names.
fn stored_key(stored: &str) -> Option<String> {
    let (address, port) = stored.rsplit_once(':')?;
    Some(key(address, port.parse().ok()?))
}

/// `hosts` with each host once, its first entry kept: earlier versions
/// could list one host under two spellings.
#[uniffi::export]
pub fn distinct_hosts(hosts: Vec<ListedHost>) -> Vec<ListedHost> {
    let mut seen = Vec::new();
    hosts
        .into_iter()
        .filter(|host| {
            let key = host_of(host);
            let first = !seen.contains(&key);
            seen.push(key);
            first
        })
        .collect()
}

/// The Hosts list of an app that had none: the host the old Hosts tab
/// kept (its last address and port as typed, if they still make a host),
/// then each host it kept a certificate authority for, in address order,
/// each host once and saved in its canonical form.
#[uniffi::export]
pub fn former_hosts(
    last_address: String,
    last_port: String,
    certificates: HashMap<String, String>,
) -> Vec<ListedHost> {
    let mut hosts = Vec::new();
    if let Ok((address, port)) = om_gui::host_address(&last_address, &last_port) {
        if let Some(address) = canonical_address(&address) {
            hosts.push(ListedHost {
                address,
                port,
                name: String::new(),
            });
        }
    }
    let mut trusted: Vec<ListedHost> = certificates
        .keys()
        .filter_map(|stored| {
            let (address, port) = stored.rsplit_once(':')?;
            Some(ListedHost {
                address: canonical_address(address)?,
                port: port.parse().ok().filter(|port| *port > 0)?,
                name: String::new(),
            })
        })
        .collect();
    trusted.sort_by(|a, b| (&a.address, a.port).cmp(&(&b.address, b.port)));
    hosts.extend(trusted);
    distinct_hosts(hosts)
}

/// The certificate authorities trusted for `address` and `port`: those
/// kept under any spelling of the address, the one under this spelling
/// first and the rest in order, each certificate once, as PEM, none left
/// out for the bounds (connecting refuses a host past them,
/// [`check_bundle`]). What does not read as a certificate a trust store
/// takes is left out, so one malformed authority kept long ago cannot stop
/// the others working.
#[uniffi::export]
pub fn trusted_certificate(
    certificates: HashMap<String, String>,
    address: String,
    port: u16,
) -> String {
    pem_of(&trusted(&certificates, &address, port))
}

fn trusted(
    certificates: &HashMap<String, String>,
    address: &str,
    port: u16,
) -> Vec<CertificateDer<'static>> {
    let target = key(address, port);
    let exact = format!("{address}:{port}");
    let spellings: BTreeMap<(bool, &String), &String> = certificates
        .iter()
        .filter(|(stored, _)| stored_key(stored).as_deref() == Some(target.as_str()))
        .map(|(stored, pem)| ((*stored != exact, stored), pem))
        .collect();
    let mut out = Vec::new();
    for pem in spellings.values() {
        add_unique(&mut out, stored(pem).certificates);
    }
    out
}

/// A host's certificate authorities as the engine is given them, within
/// the bounds, or what to do about them.
pub(crate) fn check_bundle(text: &str) -> Result<(), GoError> {
    if text.len() > MAX_PEM_BYTES || stored(text).certificates.len() > MAX_CERTIFICATES {
        return Err(too_many());
    }
    Ok(())
}

fn too_many() -> GoError {
    GoError::failed(format!(
        "A host can trust at most {MAX_CERTIFICATES} certificate authorities in {} KiB. \
         Edit the host and remove some.",
        MAX_PEM_BYTES / 1024
    ))
}

/// Save Add Host or Edit Host's `change` into `saved`.
///
/// - The address and port are checked as the desktop's Add Host checks
///   them (`om_gui::host_address`), and the address is saved in its
///   canonical form (the module's notes). The certificate authority must be
///   empty, or certificates and nothing else, within bounds.
/// - Edit Host is refused when its host was changed, moved or removed, or
///   its authorities changed, since it opened (in another window), rather
///   than undo that change.
/// - The host goes in the place of the one it edits, else of the one with
///   its identity; other entries with its identity go.
/// - It trusts what the editor holds, and what the editor did not show:
///   the authorities of its own identity and of the entries it replaces.
/// - Moved to another address or port, it takes what the editor showed of
///   its own spelling and what was typed; what is kept under other
///   spellings of its old address stays there.
#[uniffi::export]
pub fn save_host(saved: SavedHosts, change: HostChange) -> Result<HostSaved, GoError> {
    let (address, port) =
        om_gui::host_address(&change.address, &change.port).map_err(GoError::failed)?;
    let address = canonical_address(&address)
        .ok_or_else(|| GoError::failed("Enter a valid host name or IP address."))?;
    let typed = readable(&change.certificate)?;
    let host = ListedHost {
        address,
        port,
        name: change.name.trim().to_owned(),
    };
    let SavedHosts {
        hosts,
        certificates,
    } = saved;
    let stale = || {
        GoError::failed(
            "This host was changed in another window while it was being edited. \
             Cancel, and edit the host again.",
        )
    };
    if let Some(old) = &change.replaces {
        let current = hosts.iter().find(|listed| host_of(listed) == host_of(old));
        if current != Some(old)
            || trusted(&certificates, &old.address, old.port) != stored(&change.loaded).certificates
        {
            return Err(stale());
        }
    }
    let host_key = host_of(&host);
    let old_key = change.replaces.as_ref().map(host_of);
    let moved = old_key.as_ref().is_some_and(|old| *old != host_key);
    let at = old_key
        .as_ref()
        .and_then(|old| hosts.iter().position(|listed| host_of(listed) == *old))
        .or_else(|| hosts.iter().position(|listed| host_of(listed) == host_key));
    let replaced: Vec<&ListedHost> = hosts
        .iter()
        .enumerate()
        .filter(|(i, listed)| Some(*i) == at || host_of(listed) == host_key)
        .map(|(_, listed)| listed)
        .collect();

    let mut kept = Vec::new();
    match (&change.replaces, moved) {
        (Some(old), true) => {
            // Moved: what came only from another spelling stays.
            let exact = format!("{}:{}", old.address, old.port);
            let own = certificates
                .get(&exact)
                .map(|pem| stored(pem).certificates)
                .unwrap_or_default();
            let all = trusted(&certificates, &old.address, old.port);
            add_unique(
                &mut kept,
                typed
                    .into_iter()
                    .filter(|der| own.contains(der) || !all.contains(der))
                    .collect(),
            );
        }
        _ => add_unique(&mut kept, typed),
    }
    // What the editor did not show: the host's own identity, and the
    // entries it replaces, but not the one it edits.
    for unseen in std::iter::once(&host).chain(replaced.iter().copied()) {
        if Some(host_of(unseen)) != old_key {
            add_unique(
                &mut kept,
                trusted(&certificates, &unseen.address, unseen.port),
            );
        }
    }
    let bundle = pem_of(&kept);
    if kept.len() > MAX_CERTIFICATES || bundle.len() > MAX_PEM_BYTES {
        return Err(too_many());
    }

    // Replaced under every spelling: the host's identity and the entries it
    // merges. A moved host's old identity loses only its own spelling.
    let mut gone: Vec<String> = std::iter::once(&host)
        .chain(replaced.iter().copied())
        .map(host_of)
        .filter(|key| !moved || Some(key) != old_key.as_ref())
        .collect();
    gone.dedup();
    // Nothing it replaces may be lost: every value it would drop or rewrite
    // must read whole, or nothing is saved.
    let moved_spelling = change
        .replaces
        .as_ref()
        .filter(|_| moved)
        .map(|old| format!("{}:{}", old.address, old.port));
    let replaces_unreadable = certificates.iter().any(|(stored_as, pem)| {
        let replaced = stored_key(stored_as).is_some_and(|key| gone.contains(&key))
            || moved_spelling.as_deref() == Some(stored_as.as_str());
        replaced && !stored(pem).whole
    });
    if replaces_unreadable {
        return Err(GoError::failed(
            "A certificate authority saved for this host cannot be read, so nothing was saved. \
             Remove the host and add it again to drop it.",
        ));
    }
    let mut certificates: HashMap<String, String> = certificates
        .into_iter()
        .filter(|(stored, _)| !stored_key(stored).is_some_and(|key| gone.contains(&key)))
        .collect();
    if let Some(spelling) = &moved_spelling {
        certificates.remove(spelling);
    }
    if !kept.is_empty() {
        certificates.insert(format!("{}:{}", host.address, host.port), bundle);
    }

    let mut list: Vec<ListedHost> = hosts
        .iter()
        .enumerate()
        .filter_map(|(i, listed)| {
            if Some(i) == at {
                Some(host.clone())
            } else if host_of(listed) == host_key {
                None
            } else {
                Some(listed.clone())
            }
        })
        .collect();
    if at.is_none() {
        list.push(host.clone());
    }
    Ok(HostSaved {
        saved: SavedHosts {
            hosts: list,
            certificates,
        },
        host,
    })
}

/// `saved` without `host`, and without the certificate authorities kept
/// for it under any spelling.
#[uniffi::export]
pub fn remove_host(saved: SavedHosts, host: ListedHost) -> SavedHosts {
    let gone = host_of(&host);
    SavedHosts {
        hosts: saved
            .hosts
            .into_iter()
            .filter(|listed| host_of(listed) != gone)
            .collect(),
        certificates: saved
            .certificates
            .into_iter()
            .filter(|(stored, _)| stored_key(stored).as_deref() != Some(gone.as_str()))
            .collect(),
    }
}

/// One `-----BEGIN label-----` ... `-----END label-----` section, and
/// whether it was closed by its own END line.
struct Section {
    label: String,
    body: String,
}

/// The PEM sections of `text`, and whether it has anything else: text
/// outside a section, a section never closed (a BEGIN after it starts a
/// new one, so it hides nothing that follows), or an END for another
/// label.
fn sections(text: &str) -> (Vec<Section>, bool) {
    let mut out = Vec::new();
    let mut other = false;
    let mut open: Option<Section> = None;
    // Lines end in LF, CRLF or a CR alone (classic Mac text).
    for line in text.split(['\n', '\r']) {
        let line = line.trim();
        if let Some(label) = line
            .strip_prefix("-----BEGIN ")
            .and_then(|rest| rest.strip_suffix("-----"))
        {
            other |= open.is_some();
            open = Some(Section {
                label: label.to_owned(),
                body: String::new(),
            });
        } else if let Some(label) = line
            .strip_prefix("-----END ")
            .and_then(|rest| rest.strip_suffix("-----"))
        {
            match open.take() {
                Some(section) if section.label == label => out.push(section),
                _ => other = true,
            }
        } else if let Some(section) = &mut open {
            section.body.push_str(line);
        } else {
            other |= !line.is_empty();
        }
    }
    (out, other || open.is_some())
}

/// A certificate section's DER, if it is a certificate a trust store takes.
fn certificate(section: &Section) -> Option<CertificateDer<'static>> {
    use base64::Engine;
    if section.label != "CERTIFICATE" {
        return None;
    }
    let der = base64::engine::general_purpose::STANDARD
        .decode(section.body.as_bytes())
        .ok()?;
    let der = CertificateDer::from(der);
    takes(&der).then_some(der)
}

/// A value kept for a host: the certificates in it a trust store takes,
/// each once, and whether it reads whole (certificates and nothing else).
/// Text over [`MAX_STORED_BYTES`] is not read at all.
struct Stored {
    certificates: Vec<CertificateDer<'static>>,
    whole: bool,
}

fn stored(text: &str) -> Stored {
    if text.len() > MAX_STORED_BYTES {
        return Stored {
            certificates: Vec::new(),
            whole: false,
        };
    }
    let (found, other) = sections(text);
    let mut whole = !other;
    let mut certificates = Vec::new();
    for section in &found {
        match certificate(section) {
            Some(der) if !certificates.contains(&der) => certificates.push(der),
            Some(_) => {}
            None => whole = false,
        }
    }
    Stored {
        certificates,
        whole,
    }
}

/// Text typed as a certificate authority: empty, or certificates a trust
/// store takes and nothing else, within bounds.
pub(crate) fn readable(text: &str) -> Result<Vec<CertificateDer<'static>>, GoError> {
    if text.len() > MAX_PEM_BYTES {
        return Err(GoError::failed(format!(
            "The certificate authority is longer than {} KiB.",
            MAX_PEM_BYTES / 1024
        )));
    }
    let refused = || GoError::failed("The certificate authority is not PEM certificates alone.");
    let (found, other) = sections(text);
    if other || (found.is_empty() && !text.trim().is_empty()) {
        return Err(refused());
    }
    if found.len() > MAX_CERTIFICATES {
        return Err(too_many());
    }
    let mut out = Vec::new();
    for section in &found {
        let der = certificate(section).ok_or_else(refused)?;
        if !out.contains(&der) {
            out.push(der);
        }
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

    fn usable(text: &str) -> Vec<CertificateDer<'static>> {
        stored(text).certificates
    }

    fn certificate(name: &str) -> String {
        rcgen::generate_simple_self_signed(vec![name.into()])
            .unwrap()
            .cert
            .pem()
    }

    fn listed(address: &str) -> ListedHost {
        ListedHost {
            address: address.into(),
            port: 5003,
            name: String::new(),
        }
    }

    fn saved(hosts: &[&str], certificates: &[(&str, &str)]) -> SavedHosts {
        SavedHosts {
            hosts: hosts.iter().map(|address| listed(address)).collect(),
            certificates: certificates
                .iter()
                .map(|(key, pem)| ((*key).to_owned(), (*pem).to_owned()))
                .collect(),
        }
    }

    fn add(saved: SavedHosts, address: &str, certificate: &str) -> Result<HostSaved, GoError> {
        save_host(
            saved,
            HostChange {
                replaces: None,
                loaded: String::new(),
                address: address.into(),
                port: "5003".into(),
                name: String::new(),
                certificate: certificate.into(),
            },
        )
    }

    fn edit(
        saved: SavedHosts,
        old: &ListedHost,
        loaded: &str,
        address: &str,
        port: &str,
        certificate: &str,
    ) -> Result<HostSaved, GoError> {
        save_host(
            saved,
            HostChange {
                replaces: Some(old.clone()),
                loaded: loaded.into(),
                address: address.into(),
                port: port.into(),
                name: old.name.clone(),
                certificate: certificate.into(),
            },
        )
    }

    fn shown(saved: &SavedHosts, address: &str, port: u16) -> String {
        trusted_certificate(saved.certificates.clone(), address.into(), port)
    }

    /// One identity for trust, DNS and TLS: an IP address as `IpAddr`
    /// writes it (full-width digits mapped by UTS #46, as browsers map
    /// them), a name in its ASCII form; never Unicode case folding, under
    /// which ı would match i; an address that maps to nothing is its own
    /// host by its exact text.
    #[test]
    fn hosts_are_the_same_as_the_network_takes_them() {
        let key = |address: &str| host_key(address.into(), 5003);
        assert_eq!(key("Server.Example"), key("server.example"));
        assert_eq!(key("BÜCHER.example"), key("xn--bcher-kva.example"));
        assert_ne!(key("ı.example"), key("i.example"));
        assert_ne!(
            key("server.example"),
            host_key("server.example".into(), 5004)
        );
        assert_eq!(key("１２７.０.０.１"), key("127.0.0.1"));
        assert_eq!(key("FE80:0::1"), key("fe80::1"));
        assert_ne!(key("Ａ..example"), key("a..example"));
        // Saved in the form it is connected as.
        let saved = add(SavedHosts::default(), "１２７.０.０.１", "").unwrap();
        assert_eq!(saved.host.address, "127.0.0.1");
        assert!(add(SavedHosts::default(), "a..example", "").is_err());
        // An earlier version's two spellings of one host are listed once.
        let hosts = distinct_hosts(vec![
            listed("bücher.example"),
            listed("xn--bcher-kva.example"),
        ]);
        assert_eq!(hosts, [listed("bücher.example")]);
    }

    /// An authority kept under another spelling that does not read, or a
    /// block never closed before a good one, does not stop the good one;
    /// the same certificate with other line endings is one.
    #[test]
    fn unreadable_or_repeated_authorities_do_not_count() {
        let good = certificate("server.example");
        let crlf = good.replace('\n', "\r\n");
        let unclosed = format!("-----BEGIN CERTIFICATE-----\n!\n{good}");
        let store = saved(
            &[],
            &[
                ("server.example:5003", &good),
                (
                    "SERVER.example:5003",
                    "-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----",
                ),
                ("Server.Example:5003", &crlf),
            ],
        );
        let trusted = shown(&store, "server.example", 5003);
        assert_eq!(usable(&trusted), usable(&good));
        assert_eq!(trusted.matches("BEGIN CERTIFICATE").count(), 1);
        assert_eq!(usable(&unclosed), usable(&good));
    }

    /// Add Host with an empty field keeps every authority kept under any
    /// spelling of the host; Edit Host clearing what it showed removes
    /// them all.
    #[test]
    fn a_save_keeps_what_the_editor_did_not_show() {
        let (a, b) = (certificate("a"), certificate("b"));
        let store = saved(
            &["server.example"],
            &[("SERVER.example:5003", &a), ("server.example:5003", &b)],
        );
        let added = add(store, "Server.Example", "").unwrap();
        assert_eq!(added.saved.hosts, [listed("server.example")]);
        assert_eq!(added.saved.certificates.len(), 1);
        let loaded = shown(&added.saved, "server.example", 5003);
        assert_eq!(usable(&loaded).len(), 2);
        let host = added.host.clone();
        let cleared = edit(added.saved, &host, &loaded, "server.example", "5003", "").unwrap();
        assert!(cleared.saved.certificates.is_empty());
    }

    /// An editor whose host was changed, moved or removed in another window
    /// since it opened, or whose authorities were, is refused: it would
    /// recreate a host, erase an authority it never showed, or bring back
    /// one that was cleared.
    #[test]
    fn a_stale_editor_is_refused() {
        let (a, b) = (certificate("a"), certificate("b"));
        let host = listed("server.example");
        let with_b = saved(&["server.example"], &[("server.example:5003", &b)]);
        // Its authorities changed or were cleared.
        assert!(edit(with_b.clone(), &host, &a, "server.example", "5003", &a).is_err());
        assert!(edit(
            saved(&["server.example"], &[]),
            &host,
            &a,
            "server.example",
            "5003",
            &a
        )
        .is_err());
        // It was moved or removed: saving would recreate it.
        let moved = saved(&["other.example"], &[]);
        assert!(edit(moved, &host, "", "server.example", "5003", "").is_err());
        let renamed = SavedHosts {
            hosts: vec![ListedHost {
                name: "Renamed".into(),
                ..host.clone()
            }],
            ..with_b.clone()
        };
        assert!(edit(renamed, &host, &b, "server.example", "5003", &b).is_err());
        assert!(edit(with_b, &host, &b, "server.example", "5003", &b).is_ok());
    }

    /// Moving a host to another address or port takes what was kept under
    /// its own spelling and what was typed; what was kept under another
    /// spelling of its old address stays there, trusted as before.
    #[test]
    fn a_moved_host_leaves_other_spellings_in_place() {
        let (own, other, typed) = (certificate("own"), certificate("other"), certificate("new"));
        let store = saved(
            &["server.example"],
            &[
                ("server.example:5003", &own),
                ("SERVER.EXAMPLE:5003", &other),
            ],
        );
        let loaded = shown(&store, "server.example", 5003);
        let host = listed("server.example");
        let moved = edit(
            store,
            &host,
            &loaded,
            "server.example",
            "5004",
            &format!("{loaded}\n{typed}"),
        )
        .unwrap();
        assert_eq!(moved.saved.hosts.len(), 1);
        assert_eq!(
            usable(&shown(&moved.saved, "server.example", 5004)),
            [usable(&own), usable(&typed)].concat()
        );
        assert_eq!(
            usable(&shown(&moved.saved, "server.example", 5003)),
            usable(&other)
        );
    }

    /// Typed text that is not certificates alone, or past the bounds, is
    /// refused; text kept past the bounds is not read.
    #[test]
    fn certificate_text_is_certificates_alone_and_bounded() {
        let good = certificate("server.example");
        let refused = |text: &str| add(SavedHosts::default(), "server.example", text).is_err();
        assert!(refused("not a certificate"));
        assert!(refused(&format!("junk\n{good}")));
        assert!(refused(&format!(
            "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n{good}"
        )));
        assert!(!refused(""));
        assert!(!refused(&good));
        let many: String = (0..MAX_CERTIFICATES + 1)
            .map(|i| certificate(&format!("ca{i}.example")))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(refused(&many));
        let copies = good.repeat(MAX_PEM_BYTES / good.len() + 1);
        assert!(refused(&copies));
        // Kept, it reads (one certificate, repeated), but not to connect.
        assert_eq!(usable(&copies), usable(&good));
        assert!(check_bundle(&copies).is_err());
    }

    /// A save never drops, truncates or rewrites a stored authority it
    /// cannot carry whole: past the bounds, or with a value under any
    /// spelling of the host that does not read whole, it saves nothing.
    /// Lookup gives them all, and connecting refuses past the bounds.
    #[test]
    fn a_save_never_drops_what_it_cannot_carry() {
        let sixteen: Vec<String> = (0..MAX_CERTIFICATES)
            .map(|i| certificate(&format!("ca{i}.example")))
            .collect();
        let seventeenth = certificate("ca16.example");
        let store = saved(
            &["server.example"],
            &[
                ("server.example:5003", &sixteen.join("\n")),
                ("SERVER.example:5003", &seventeenth),
            ],
        );
        assert!(add(store.clone(), "server.example", "").is_err());
        let all = shown(&store, "server.example", 5003);
        assert_eq!(usable(&all).len(), MAX_CERTIFICATES + 1);
        assert!(check_bundle(&all).is_err());

        let big = |name: &str| {
            let names: Vec<String> = (0..1400)
                .map(|i| format!("{name}-{i:04}.example.com"))
                .collect();
            rcgen::generate_simple_self_signed(names)
                .unwrap()
                .cert
                .pem()
        };
        let (a, b) = (big("a"), big("b"));
        assert!(a.len() < MAX_PEM_BYTES && a.len() + b.len() > MAX_PEM_BYTES);
        let store = saved(&["server.example"], &[("server.example:5003", &a)]);
        assert!(add(store, "server.example", &b).is_err());

        let broken = format!(
            "{}\n-----BEGIN CERTIFICATE-----\nnot base64!\n-----END CERTIFICATE-----",
            certificate("x")
        );
        let store = saved(&["server.example"], &[("SERVER.example:5003", &broken)]);
        assert!(add(store, "server.example", "").is_err());
    }

    /// A value kept with lines ending in a CR alone reads, and a save
    /// carries it.
    #[test]
    fn lines_ending_in_a_cr_alone_read() {
        let good = certificate("server.example");
        let cr = good.replace('\n', "\r");
        let store = saved(&["server.example"], &[("SERVER.example:5003", &cr)]);
        assert_eq!(
            usable(&shown(&store, "server.example", 5003)),
            usable(&good)
        );
        let saved = add(store, "server.example", "").unwrap();
        assert_eq!(
            usable(&shown(&saved.saved, "server.example", 5003)),
            usable(&good)
        );
    }
}
