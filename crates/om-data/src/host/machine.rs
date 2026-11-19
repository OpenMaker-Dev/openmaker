//! What `Get` says about the machine a client or a server runs on
//! (calc-functions.md 6.1, 6.3 O1): the operating system's version, the
//! machine's name, the network interfaces' hardware and IPv4 addresses, an
//! id that stays the same across sessions, the account's full name, free
//! storage and the plug-in document. The facts are read once per process,
//! except free storage, which changes. The user name in effect follows the
//! User Name setting ([`user_name`]).

use std::path::Path;
use std::sync::OnceLock;

/// The facts that do not change while the process runs.
#[derive(Clone, Debug, Default)]
pub struct Machine {
    /// `Get ( SystemVersion )`: `26.6.2` on macOS, `24.04.5` on Ubuntu.
    pub system_version: String,
    /// The machine's own name (`Get ( HostName )` of a local file).
    pub host_name: String,
    /// The account's full name, else its login name: what Settings' User
    /// Name "System" choice shows and gives ([`user_name`]).
    pub system_user_name: String,
    /// `OPENMAKER_USER_NAME`, read once: when set, [`user_name`] is it,
    /// whatever the User Name setting (for tests and measures).
    pub user_name_override: Option<String>,
    /// `Get ( SystemNICAddress )`: each interface's hardware address,
    /// lowercase hex with colons, in the system's interface order.
    pub nic_addresses: Vec<String>,
    /// `Get ( SystemIPAddress )`: the IPv4 addresses, dotted decimal,
    /// loopback left out.
    pub ip_addresses: Vec<String>,
    /// `Get ( PersistentID )`: 32 upper-case hex digits, the same in every
    /// session on the machine.
    pub persistent_id: String,
}

impl Machine {
    /// This machine, read on first use.
    pub fn current() -> &'static Machine {
        static MACHINE: OnceLock<Machine> = OnceLock::new();
        MACHINE.get_or_init(Machine::read)
    }

    fn read() -> Machine {
        let host_name = host_name();
        let id = machine_id().unwrap_or_else(|| host_name.clone());
        Machine {
            system_version: system_version(),
            system_user_name: system_user_name(),
            user_name_override: std::env::var("OPENMAKER_USER_NAME").ok(),
            nic_addresses: nic_addresses(),
            ip_addresses: ip_addresses(),
            persistent_id: md5_hex(id.as_bytes()),
            host_name,
        }
    }
}

/// `Get ( SystemPlatform )`: 1 macOS, -2 Windows, 8 Linux (FileMaker's
/// Ubuntu host value).
pub fn platform() -> i64 {
    if cfg!(windows) {
        -2
    } else if cfg!(target_os = "macos") {
        1
    } else {
        8
    }
}

/// `Get ( Device )`: 1 Mac, 2 Windows, 6 Linux.
pub fn device() -> i64 {
    if cfg!(windows) {
        2
    } else if cfg!(target_os = "macos") {
        1
    } else {
        6
    }
}

/// `Get ( ApplicationArchitecture )`.
pub fn architecture() -> &'static str {
    if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    }
}

/// `Get ( SystemStorageAvailable )`: the bytes free to this user on the
/// volume holding `path`; `None` when the system does not say.
pub fn storage_available(path: &Path) -> Option<u64> {
    #[cfg(unix)]
    {
        let stat = rustix::fs::statvfs(path).ok()?;
        Some(stat.f_bavail.saturating_mul(stat.f_frsize))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        None
    }
}

/// `Get ( InstalledFMPluginsAsJSON )` with no plug-ins, laid out as
/// FileMaker writes it (one tab per level, CR line ends).
pub fn plugins_json() -> String {
    format!(
        "{{\r\t\"APIVers\" : 2,\r\t\"environment\" : \r\t{{\r\t\t\"architecture\" : \"{}\",\r\t\t\"platform\" : {},\r\t\t\"version\" : \"{}\"\r\t}},\r\t\"plugins\" : null\r}}",
        architecture(),
        platform(),
        Machine::current().system_version
    )
}

/// An MD5 digest as 32 upper-case hex digits.
pub fn md5_hex(bytes: &[u8]) -> String {
    use md5::{Digest, Md5};
    Md5::digest(bytes)
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect()
}

/// The first word of `/etc/os-release`'s `VERSION` on Linux (`24.04.5`),
/// `ProductVersion` of the system version file on macOS; empty elsewhere.
fn system_version() -> String {
    if cfg!(target_os = "macos") {
        let plist = std::fs::read_to_string("/System/Library/CoreServices/SystemVersion.plist")
            .unwrap_or_default();
        return plist_string(&plist, "ProductVersion").unwrap_or_default();
    }
    std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines()
                .find_map(|l| l.strip_prefix("VERSION="))
                .map(|v| {
                    v.trim_matches('"')
                        .split_whitespace()
                        .next()
                        .unwrap_or_default()
                        .to_owned()
                })
        })
        .unwrap_or_default()
}

/// The `<string>` after `<key>key</key>` in a property list.
fn plist_string(plist: &str, key: &str) -> Option<String> {
    let after = &plist[plist.find(&format!("<key>{key}</key>"))?..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")?;
    Some(after[start..start + end].trim().to_owned())
}

fn host_name() -> String {
    #[cfg(unix)]
    {
        rustix::system::uname()
            .nodename()
            .to_string_lossy()
            .into_owned()
    }
    #[cfg(not(unix))]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

/// The system's machine id (systemd's, or D-Bus's on older systems).
fn machine_id() -> Option<String> {
    ["/etc/machine-id", "/var/lib/dbus/machine-id"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_owned())
        .find(|s| !s.is_empty())
}

/// FileMaker Pro's User Name setting (Settings > General > User Name): the
/// system's name for the account, or a name the user typed
/// (calc-functions.md 6.1). `Get ( UserName )`, the user name auto-entry
/// writes and the session name a host lists all come from it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum UserNameSetting {
    /// The account's full name ([`Machine::system_user_name`]).
    #[default]
    System,
    /// This name, as typed (it may be empty).
    Other(String),
}

static USER_NAME_SETTING: std::sync::RwLock<UserNameSetting> =
    std::sync::RwLock::new(UserNameSetting::System);

/// The User Name setting in effect for this process.
pub fn user_name_setting() -> UserNameSetting {
    USER_NAME_SETTING
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

/// Use `setting` from now on: calculations evaluated and sessions opened
/// after this read the new name; a host keeps the name a session signed in
/// with.
pub fn set_user_name_setting(setting: UserNameSetting) {
    *USER_NAME_SETTING
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = setting;
}

/// The user name in effect (`Get ( UserName )`): `OPENMAKER_USER_NAME` when
/// it was set at first use, else the User Name setting's.
pub fn user_name() -> String {
    let machine = Machine::current();
    if let Some(name) = &machine.user_name_override {
        return name.clone();
    }
    match user_name_setting() {
        UserNameSetting::System => machine.system_user_name.clone(),
        UserNameSetting::Other(name) => name,
    }
}

/// The account's full name (the first comma-separated part of its GECOS
/// field, which macOS fills from the account's full name), else its login
/// name. FileMaker Pro's "System" choice shows the full name on macOS
/// (calc-functions.md 6.1).
fn system_user_name() -> String {
    #[cfg(unix)]
    {
        if let Ok(Some(user)) = nix::unistd::User::from_uid(nix::unistd::Uid::current()) {
            let gecos = user.gecos.to_string_lossy();
            let full = gecos.split(',').next().unwrap_or_default().trim();
            if !full.is_empty() {
                return full.to_owned();
            }
            if !user.name.is_empty() {
                return user.name;
            }
        }
    }
    ["USER", "LOGNAME", "USERNAME"]
        .iter()
        .find_map(|v| std::env::var(v).ok())
        .unwrap_or_default()
}

/// The hardware addresses of `/sys/class/net`'s interfaces in index order,
/// without the loopback's zeros.
fn nic_addresses() -> Vec<String> {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Vec::new();
    };
    let mut interfaces: Vec<(u64, String)> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let address = std::fs::read_to_string(path.join("address")).ok()?;
            let address = address.trim().to_lowercase();
            if address.is_empty() || address.chars().all(|c| c == '0' || c == ':') {
                return None;
            }
            let index = std::fs::read_to_string(path.join("ifindex"))
                .ok()
                .and_then(|i| i.trim().parse().ok())
                .unwrap_or(u64::MAX);
            Some((index, address))
        })
        .collect();
    interfaces.sort();
    let mut out: Vec<String> = Vec::new();
    for (_, address) in interfaces {
        if !out.contains(&address) {
            out.push(address);
        }
    }
    out
}

/// The machine's own IPv4 addresses: the `/32 host LOCAL` entries of the
/// kernel's routing trie, loopback left out.
fn ip_addresses() -> Vec<String> {
    let text = std::fs::read_to_string("/proc/net/fib_trie").unwrap_or_default();
    local_addresses(&text)
}

fn local_addresses(fib_trie: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut last: Option<&str> = None;
    for line in fib_trie.lines() {
        let line = line.trim();
        if let Some(address) = line.strip_prefix("|-- ") {
            last = Some(address.trim());
        } else if line.starts_with("/32 host LOCAL") {
            if let Some(address) = last.take() {
                let address = address.to_owned();
                if !address.starts_with("127.") && !out.contains(&address) {
                    out.push(address);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_addresses_are_the_routing_tries_host_entries() {
        let trie = "Main:\n  +-- 0.0.0.0/0 3 0 5\n     |-- 0.0.0.0\n        /0 universe UNICAST\n     \
                    +-- 192.0.2.0/24 2 0 2\n        |-- 192.0.2.60\n           /32 host LOCAL\n\
                    Local:\n  +-- 127.0.0.0/8 2 0 2\n     |-- 127.0.0.1\n        /32 host LOCAL\n     \
                    |-- 192.0.2.60\n        /32 host LOCAL\n     |-- 172.17.0.1\n        /32 host LOCAL\n";
        assert_eq!(local_addresses(trie), ["192.0.2.60", "172.17.0.1"]);
    }

    #[test]
    fn the_plist_names_the_product_version() {
        let plist = "<dict>\n\t<key>ProductName</key>\n\t<string>macOS</string>\n\t\
                     <key>ProductVersion</key>\n\t<string>26.6.2</string>\n</dict>";
        assert_eq!(
            plist_string(plist, "ProductVersion").as_deref(),
            Some("26.6.2")
        );
    }

    #[test]
    fn this_machine_answers_in_filemakers_forms() {
        let machine = Machine::current();
        assert_eq!(machine.persistent_id.len(), 32);
        assert!(machine
            .persistent_id
            .chars()
            .all(|c| c.is_ascii_digit() || c.is_ascii_uppercase()));
        for address in &machine.nic_addresses {
            assert_eq!(address.len(), 17, "{address}");
            assert_eq!(address.to_lowercase(), *address);
        }
        assert!(plugins_json().contains("\"plugins\" : null"));
        #[cfg(unix)]
        assert!(storage_available(Path::new("/")).is_some_and(|n| n > 0));
    }
}
