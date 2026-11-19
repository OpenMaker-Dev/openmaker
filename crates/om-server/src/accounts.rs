//! What a file says about each account's kind and state, read as FileMaker
//! reads it (docs/re/local-files.md 2.2): the kind `[23, 1, 5, A, 02]` and
//! the status `[23, 1, 5, A, 0a]`, whose flags say whether the account is
//! active and whether its password must be changed. A local file's private
//! host ([`crate::LocalHost`]) admits only accounts these show to be
//! active FileMaker-file accounts with no password change due.

use jiff::civil::Date;

/// An account's kind (`[23, 1, 5, A, 02]`, one byte, absent for 0).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountKind {
    /// 0: an account of the file itself, whose password the file stores:
    /// the only kind FileMaker checks a typed password against.
    FileMakerFile,
    /// Any other value: an account another service authenticates (an
    /// external server, OAuth or Claris ID); FileMaker Pro refuses it a
    /// typed password with 212.
    External(u8),
}

impl AccountKind {
    /// The kind `value` names, `None` (absent) being a FileMaker-file
    /// account; `None` for a value that is not one byte.
    pub fn decode(value: Option<&[u8]>) -> Option<AccountKind> {
        match value {
            None | Some([0]) => Some(AccountKind::FileMakerFile),
            Some([kind]) => Some(AccountKind::External(*kind)),
            Some(_) => None,
        }
    }
}

/// An account's status (`[23, 1, 5, A, 0a]`): a timestamp, when its
/// password was last set, whose microseconds carry the account's flags.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccountStatus {
    pub date: Date,
    /// Seconds since midnight.
    pub seconds: u32,
    pub microseconds: u32,
}

/// The microsecond bit of an account FileMaker keeps from logging in.
const INACTIVE: u32 = 1 << 19;
/// The microsecond bit of an account whose password must be changed at its
/// next login.
const CHANGE_PASSWORD: u32 = 1 << 18;

impl AccountStatus {
    /// The status FileMaker reads for an account without one: 1/1/1024 at
    /// midnight, active, no password change due.
    pub const ABSENT: AccountStatus = AccountStatus {
        date: Date::constant(1024, 1, 1),
        seconds: 0,
        microseconds: 0,
    };

    /// The status `value` holds (`None`: [`AccountStatus::ABSENT`]); `None`
    /// for one that is not a timestamp key of this form.
    ///
    /// The value is a timestamp index key (query-program.md 7.3) after a
    /// byte counting the rest: the date's three bytes `0x800000 | year <<
    /// 10 | month << 6 | day`, then the number key of the seconds since
    /// midnight, fraction included: `80`, a count of integer digits, then
    /// the digits as nibbles each holding `digit + 1`, a zero nibble
    /// ending them. More than six fraction digits, a negative or
    /// out-of-range time and an invalid date are refused.
    pub fn decode(value: Option<&[u8]>) -> Option<AccountStatus> {
        let Some(value) = value else {
            return Some(AccountStatus::ABSENT);
        };
        let (&length, rest) = value.split_first()?;
        if usize::from(length) != rest.len() {
            return None;
        }
        let (&[high, middle, low], time) = rest.split_first_chunk::<3>()?;
        let date = u32::from_be_bytes([0, high, middle, low]);
        if date & 0x80_0000 == 0 {
            return None;
        }
        let year = i16::try_from(date >> 10 & 0x1fff).ok()?;
        let month = i8::try_from(date >> 6 & 0xf).ok()?;
        let day = i8::try_from(date & 0x3f).ok()?;
        if year < 1 {
            return None;
        }
        let date = Date::new(year, month, day).ok()?;
        let (seconds, microseconds) = seconds(time)?;
        Some(AccountStatus {
            date,
            seconds,
            microseconds,
        })
    }

    /// The status FileMaker writes when the account sets its own password
    /// at `date`, `seconds` past midnight (`SetPassword`, local-files.md
    /// 2.4): the time to the second, the account's other flags kept in the
    /// microseconds, and no change due any more.
    pub fn password_set(&self, date: Date, seconds: u32) -> AccountStatus {
        AccountStatus {
            date,
            seconds,
            microseconds: self.microseconds & !CHANGE_PASSWORD,
        }
    }

    /// The value FileMaker stores for this status (`0a`), the form
    /// [`Self::decode`] reads: a byte counting the rest, the date's three
    /// bytes, `80` and the count of the seconds' integer digits (none for
    /// 0), then those digits and the microseconds' (without trailing
    /// zeros) as nibbles each holding `digit + 1`, a zero nibble padding an
    /// odd count. Matches every observed form (the tests).
    pub fn encode(&self) -> Vec<u8> {
        let year = u32::try_from(self.date.year()).unwrap_or(0);
        let month = u32::try_from(self.date.month()).unwrap_or(0);
        let day = u32::try_from(self.date.day()).unwrap_or(0);
        let date = 0x80_0000 | year << 10 | month << 6 | day;
        let digits = |text: String| -> Vec<u8> { text.bytes().map(|b| b - b'0').collect() };
        let integer = if self.seconds == 0 {
            Vec::new()
        } else {
            digits(self.seconds.to_string())
        };
        let fraction = digits(format!("{:06}", self.microseconds % 1_000_000));
        let fraction = &fraction[..fraction.iter().rposition(|&d| d != 0).map_or(0, |i| i + 1)];
        let mut nibbles: Vec<u8> = integer.iter().chain(fraction).map(|d| d + 1).collect();
        if nibbles.len() % 2 == 1 {
            nibbles.push(0);
        }
        let mut value = vec![0];
        value.extend_from_slice(&date.to_be_bytes()[1..]);
        value.extend_from_slice(&(0x8000 | integer.len() as u16).to_be_bytes());
        value.extend(nibbles.chunks(2).map(|pair| pair[0] << 4 | pair[1]));
        value[0] = (value.len() - 1) as u8;
        value
    }

    /// Whether the account may log in at all.
    pub fn is_active(&self) -> bool {
        self.microseconds & INACTIVE == 0
    }

    /// Whether its password must be changed before it logs in again.
    pub fn must_change_password(&self) -> bool {
        self.microseconds & CHANGE_PASSWORD != 0
    }

    /// Whether a privilege set that has passwords changed every `days`
    /// days (0: never) wants this password changed by `today`: FileMaker
    /// counts elapsed calendar days from the status's date; it expires
    /// when that age reaches the configured lifetime.
    pub fn password_expired(&self, days: u16, today: Date) -> bool {
        days != 0
            && self
                .date
                .until((jiff::Unit::Day, today))
                .map_or(true, |since| since.get_days() >= i32::from(days))
    }
}

/// What of a file and one of its account records FileMaker's account
/// checksum `04` covers (local-files.md 2.4), as stored.
pub struct ChecksumInput<'a> {
    /// The file's version `[2, 5]`.
    pub version: Option<u16>,
    /// The file access catalog's UUID `[23, 4, 1, 10]`.
    pub access_uuid: Option<&'a [u8]>,
    /// The file's `mInternalCreation` `[2, 254]`.
    pub creation: Option<&'a [u8]>,
    /// The account's record key under `[23, 1, 5]`.
    pub key: &'a [u8],
    /// `02`.
    pub kind: Option<u8>,
    /// `10`, as text.
    pub name: &'a str,
    /// `06`.
    pub password: &'a [u8],
    /// `03`, as text.
    pub description: Option<&'a str>,
    /// `0a`.
    pub status: Option<&'a [u8]>,
    /// `0b`.
    pub set: Option<&'a [u8]>,
}

/// FileMaker's account checksum `04` (`DBUserAccount::ComputeCRC`,
/// local-files.md 2.4): MD5 of, in order, the file access catalog's UUID
/// and the file's creation stamp (in a file of version above 10), the
/// account's key as a path key, its kind, its name and description as
/// big-endian UTF-16, the first 31 bytes of its password, and its status
/// and set each after a byte holding its length (1/1/1024 at midnight and
/// set 0 when absent). FileMaker Pro refuses a file whose account
/// checksums do not hold (808).
pub fn checksum(input: &ChecksumInput<'_>) -> [u8; 16] {
    use md5::{Digest, Md5};
    let mut md5 = Md5::new();
    if input.version.is_some_and(|version| version > 10) {
        if let Some(uuid) = input.access_uuid.filter(|uuid| uuid.len() == 16) {
            md5.update(uuid);
        }
        if let Some(creation) = input.creation {
            md5.update(creation);
        }
    }
    let key_len = input.key.len() as u8;
    md5.update([key_len.wrapping_add(1), key_len]);
    md5.update(input.key);
    md5.update([input.kind.unwrap_or(0)]);
    let utf16 = |md5: &mut Md5, text: &str| {
        for unit in text.encode_utf16() {
            md5.update(unit.to_be_bytes());
        }
    };
    utf16(&mut md5, input.name);
    md5.update(&input.password[..input.password.len().min(31)]);
    if let Some(description) = input.description {
        utf16(&mut md5, description);
    }
    let counted = |md5: &mut Md5, value: Option<&[u8]>, absent: &[u8]| {
        let value = value.unwrap_or(absent);
        md5.update([value.len() as u8]);
        md5.update(value);
    };
    counted(
        &mut md5,
        input.status,
        &[0x05, 0x90, 0x00, 0x41, 0x80, 0x00],
    );
    counted(&mut md5, input.set, &[0x01, 0x00]);
    md5.finalize().into()
}

/// The seconds and microseconds of the non-negative number key `key`.
fn seconds(key: &[u8]) -> Option<(u32, u32)> {
    let (&[high, low], packed) = key.split_first_chunk::<2>()?;
    let mark = u16::from_be_bytes([high, low]);
    // Bit 15 marks a non-negative number; the rest count its integer
    // digits.
    if mark & 0x8000 == 0 {
        return None;
    }
    let whole = usize::from(mark & 0x7fff);
    let nibbles: Vec<u8> = packed.iter().flat_map(|b| [b >> 4, b & 0xf]).collect();
    let end = nibbles
        .iter()
        .position(|&n| n == 0)
        .unwrap_or(nibbles.len());
    // Only the last byte's second nibble may pad the digits.
    if end + 1 < nibbles.len() || nibbles[..end].iter().any(|&n| n > 10) {
        return None;
    }
    let digits: Vec<u32> = nibbles[..end].iter().map(|&n| u32::from(n) - 1).collect();
    if digits.len() < whole || digits.len() - whole > 6 {
        return None;
    }
    let (integer, fraction) = digits.split_at(whole);
    let seconds = integer
        .iter()
        .try_fold(0u32, |n, d| n.checked_mul(10)?.checked_add(*d))
        .filter(|&s| s < 86_400)?;
    let microseconds = fraction
        .iter()
        .chain(std::iter::repeat(&0))
        .take(6)
        .fold(0, |n, d| n * 10 + d);
    Some((seconds, microseconds))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(value: &[u8]) -> Option<AccountStatus> {
        AccountStatus::decode(Some(value))
    }

    #[test]
    fn statuses_read_as_filemaker_reads_them() {
        // Every account of a file FileMaker made with accounts that logged
        // in (calc-functions.md s33): no date set, active.
        let s33 = status(&[0x05, 0x90, 0x00, 0x41, 0x80, 0x00]).unwrap();
        assert_eq!(s33, AccountStatus::ABSENT);
        assert!(s33.is_active() && !s33.must_change_password());
        // The starter solutions' Admin, its password set 2020-04-03 at
        // 11:24:22: active.
        let admin = status(&[0x08, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x30]).unwrap();
        assert_eq!(admin.date, Date::constant(2020, 4, 3));
        assert_eq!((admin.seconds, admin.microseconds), (41_062, 0));
        assert!(admin.is_active() && !admin.must_change_password());
        // Their [Guest]: 0.524288 seconds, bit 19, inactive.
        let guest = status(&[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x63, 0x53, 0x99]).unwrap();
        assert_eq!(guest.microseconds, 1 << 19);
        assert!(!guest.is_active());
        // 0.262144 seconds, bit 18: the password must be changed.
        let change = status(&[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x37, 0x32, 0x55]).unwrap();
        assert!(change.is_active() && change.must_change_password());
        // Integer and fraction digits in one key: 11:24:22.524288.
        let both = status(&[
            0x0b, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x36, 0x35, 0x39, 0x90,
        ])
        .unwrap();
        assert_eq!((both.seconds, both.microseconds), (41_062, 1 << 19));
        assert_eq!(AccountStatus::decode(None), Some(AccountStatus::ABSENT));
    }

    /// Every observed form reads back to itself, and a password set clears
    /// only the change-due flag.
    #[test]
    fn statuses_are_written_as_filemaker_writes_them() {
        for value in [
            &[0x05, 0x90, 0x00, 0x41, 0x80, 0x00][..],
            &[0x08, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x30],
            &[0x08, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x51, 0x91, 0x40],
            &[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x63, 0x53, 0x99],
            &[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x37, 0x32, 0x55],
            &[
                0x0b, 0x9f, 0x91, 0x03, 0x80, 0x05, 0x52, 0x17, 0x36, 0x35, 0x39, 0x90,
            ],
        ] {
            assert_eq!(status(value).unwrap().encode(), value, "{value:02x?}");
        }
        let change = status(&[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x37, 0x32, 0x55]).unwrap();
        let set = change.password_set(Date::constant(2026, 11, 14), 3_600);
        assert!(set.is_active() && !set.must_change_password());
        assert_eq!(status(&set.encode()), Some(set));
        let inactive = status(&[0x08, 0x90, 0x00, 0x41, 0x80, 0x00, 0x63, 0x53, 0x99]).unwrap();
        assert!(!inactive
            .password_set(Date::constant(2026, 11, 14), 0)
            .is_active());
    }

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len() / 2)
            .map(|i| u8::from_str_radix(&text[2 * i..2 * i + 2], 16).unwrap())
            .collect()
    }

    /// local-files.md 2.4's vector (the English Tasks starter's Admin), and
    /// the English Contacts starter of FileMaker Pro 26.0.3: its Admin (30
    /// check bytes after the hash, left out) and its [Guest] (a name
    /// outside ASCII, inactive) checksum to their stored `04`.
    #[test]
    fn account_checksums_are_filemakers() {
        let tasks = checksum(&ChecksumInput {
            version: Some(1208),
            access_uuid: Some(&hex("1a40e6a143b54e24be9884c5323d1a41")),
            creation: Some(&hex("dadd00d6")),
            key: &[0x02],
            kind: None,
            name: "Admin",
            password: &hex("01bf821c8e1e120aeda6413ac7b8a64b4ded8d15bbeeaa8bf89f02c6e7709b"),
            description: None,
            status: Some(&hex("089f91038005521730")),
            set: Some(&[0x01, 0x01]),
        });
        assert_eq!(tasks.to_vec(), hex("fd7ee9c1858689de75a95ebf99b187da"));
        let uuid = hex("7a71446525b444cfa8b8faeb246c1f8a");
        let creation = hex("dadcffd3");
        let contacts = |key: u8, name: &str, password: &str, status: &str, set: u8| {
            checksum(&ChecksumInput {
                version: Some(1208),
                access_uuid: Some(&uuid),
                creation: Some(&creation),
                key: &[key],
                kind: None,
                name,
                password: &hex(password),
                description: None,
                status: Some(&hex(status)),
                set: Some(&[0x01, set]),
            })
            .to_vec()
        };
        assert_eq!(
            contacts(
                0x02,
                "Admin",
                "01997d057fc14f708478cfc8fe9de31e08301255df87577357746441b6129a5183989cdf4740962d104fa9eeb46a6c71a44745d6d4ebcbab2301203f8a",
                "089f91038005519140",
                0x01,
            ),
            hex("11754b547c459cf732b27191da25bd38")
        );
        assert_eq!(
            contacts(
                0x01,
                "[\u{e002}]",
                "01c199bf4a6099526048471b68f3314c255feba4bfa498a5446d89595c5873daf14c7b2cbc0d8b",
                "089000418000635399",
                0x03,
            ),
            hex("a48fa408b41c6767b18221202ed9cd67")
        );
    }

    #[test]
    fn statuses_of_other_forms_are_unreadable() {
        for value in [
            &[][..],
            // The count of the rest is wrong.
            &[0x06, 0x90, 0x00, 0x41, 0x80, 0x00],
            // No time.
            &[0x03, 0x90, 0x00, 0x41],
            // The date's mark bit is missing; month 13; year 0.
            &[0x05, 0x10, 0x00, 0x41, 0x80, 0x00],
            &[0x05, 0x90, 0x03, 0x41, 0x80, 0x00],
            &[0x05, 0x80, 0x00, 0x41, 0x80, 0x00],
            // A negative time; a day's worth of seconds; a digit nibble
            // above 10; digits after the end; seven fraction digits.
            &[0x05, 0x90, 0x00, 0x41, 0x7f, 0xff],
            &[0x08, 0x90, 0x00, 0x41, 0x80, 0x05, 0x97, 0x51, 0x10],
            &[0x06, 0x90, 0x00, 0x41, 0x80, 0x00, 0xb0],
            &[0x06, 0x90, 0x00, 0x41, 0x80, 0x00, 0x06],
            &[0x09, 0x90, 0x00, 0x41, 0x80, 0x00, 0x22, 0x22, 0x22, 0x20],
            // Fewer integer digits than counted.
            &[0x06, 0x90, 0x00, 0x41, 0x80, 0x02, 0x20],
        ] {
            assert_eq!(status(value), None, "{value:02x?}");
        }
    }

    #[test]
    fn kinds_and_password_expiry() {
        assert_eq!(AccountKind::decode(None), Some(AccountKind::FileMakerFile));
        assert_eq!(
            AccountKind::decode(Some(&[0])),
            Some(AccountKind::FileMakerFile)
        );
        assert_eq!(
            AccountKind::decode(Some(&[2])),
            Some(AccountKind::External(2))
        );
        assert_eq!(AccountKind::decode(Some(&[0, 0])), None);
        let set = AccountStatus {
            date: Date::constant(2026, 1, 1),
            ..AccountStatus::ABSENT
        };
        let today = Date::constant(2026, 1, 31);
        assert!(!set.password_expired(0, today));
        assert!(!set.password_expired(32, today));
        // Expiration is inclusive at the configured elapsed-day boundary.
        assert!(set.password_expired(30, today));
        assert!(!set.password_expired(31, today));
        assert!(!set.password_expired(1, set.date));
        assert!(set.password_expired(1, Date::constant(2026, 1, 2)));
    }
}
