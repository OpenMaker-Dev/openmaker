//! FileMaker error codes as the script runtime produces, stores and reports
//! them (script-runtime.md 4, catalogs/error-codes.csv).

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use crate::catalog::csv_rows;

const CODES: &str = include_str!("../../../../docs/re/catalogs/error-codes.csv");

/// A FileMaker error code: 0 is success, -1 "Unknown error", 1-1999 the
/// documented codes, 3000-4999 internal codes that never reach
/// `Get ( LastError )`, 5000-5499 custom Revert Transaction codes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ErrorCode(pub i32);

impl ErrorCode {
    pub const NONE: ErrorCode = ErrorCode(0);
    pub const UNKNOWN: ErrorCode = ErrorCode(-1);
    /// User canceled action.
    pub const CANCELED: ErrorCode = ErrorCode(1);
    pub const MEMORY: ErrorCode = ErrorCode(2);
    /// Command is unavailable: a step the runtime has no handler for, a
    /// step the client does not support (server), transaction misuse.
    pub const UNAVAILABLE: ErrorCode = ErrorCode(3);
    /// Command is unknown: obsolete and invalid step ids.
    pub const UNKNOWN_COMMAND: ErrorCode = ErrorCode(4);
    /// Command is invalid: an invalid variable name, an unusable window.
    pub const INVALID: ErrorCode = ErrorCode(5);
    /// File is read-only: a step that would change a file hosted read
    /// only (script-runtime.md 13.28).
    pub const FILE_READ_ONLY: ErrorCode = ErrorCode(6);
    pub const INSUFFICIENT_PRIVILEGES: ErrorCode = ErrorCode(9);
    /// Command/operation canceled by triggered script.
    pub const TRIGGER_CANCELED: ErrorCode = ErrorCode(20);
    pub const FILE_MISSING: ErrorCode = ErrorCode(100);
    pub const RECORD_MISSING: ErrorCode = ErrorCode(101);
    pub const FIELD_MISSING: ErrorCode = ErrorCode(102);
    pub const SCRIPT_MISSING: ErrorCode = ErrorCode(104);
    pub const LAYOUT_MISSING: ErrorCode = ErrorCode(105);
    /// "Table is missing".
    pub const TABLE_MISSING: ErrorCode = ErrorCode(106);
    pub const WINDOW_MISSING: ErrorCode = ErrorCode(112);
    /// PSOS parameter longer than 1,000,000 characters.
    pub const PARAMETER_TOO_LONG: ErrorCode = ErrorCode(513);
    /// Internal: retry the step after the pending trigger action.
    pub const RETRY: ErrorCode = ErrorCode(3000);
    /// Internal: a multi-stage step is in progress.
    pub const MULTI_STAGE: ErrorCode = ErrorCode(3001);
    /// Internal: a button's single step needs a target field and none is
    /// active (script-runtime.md 2.3 check 3).
    pub const NO_TARGET_FIELD: ErrorCode = ErrorCode(4100);

    pub fn is_error(self) -> bool {
        self.0 != 0
    }

    /// Custom Revert Transaction codes, 5000-5499.
    pub fn is_custom(self) -> bool {
        (5000..5500).contains(&self.0)
    }

    /// Internal codes never become `Get ( LastError )`: 3000-4999 and 5500
    /// and above (`SetLastError` step 2), as FileMaker's unsigned 16-bit
    /// codes, so -1 (65535, step id 0) is one too (script-runtime.md 13.2).
    pub fn is_internal(self) -> bool {
        (3000..5000).contains(&self.0) || self.0 >= 5500 || self.0 < 0
    }

    /// The catalog's message, if the code is documented.
    pub fn message(self) -> Option<&'static str> {
        messages().get(&self.0).map(String::as_str)
    }
}

impl fmt::Display for ErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.message() {
            Some(message) => write!(f, "{message} (error {})", self.0),
            None => write!(f, "error {}", self.0),
        }
    }
}

impl From<i32> for ErrorCode {
    fn from(code: i32) -> Self {
        ErrorCode(code)
    }
}

fn messages() -> &'static BTreeMap<i32, String> {
    static MESSAGES: OnceLock<BTreeMap<i32, String>> = OnceLock::new();
    MESSAGES.get_or_init(|| {
        csv_rows(CODES)
            .into_iter()
            .filter_map(|mut row| {
                let code = row.get("code")?.parse().ok()?;
                Some((code, row.remove("message").unwrap_or_default()))
            })
            .collect()
    })
}

/// `MapError`: internal engine codes to documented ones (script-runtime.md
/// 4.5). OpenMaker hosts produce public codes; the table is applied so a
/// host that reports engine codes gets FileMaker's stored values. Code 802
/// maps to 100 only with a file-error detail the runtime does not model, so
/// it stays 802.
pub fn map_error(code: ErrorCode) -> ErrorCode {
    ErrorCode(match code.0 {
        2008 => 212,
        2009 | 2020 => 409,
        2023 | 2056 | 2059 => 3,
        2024 | 2045 => 100,
        2034 => 720,
        2035 => 718,
        2040 => 201,
        2047 => 711,
        2048 => 730,
        2050 => 130,
        2051 => 600,
        2052 | 2062 => 401,
        2053 => 400,
        2054 | 2068 => 102,
        2055 => 407,
        2058 => 821,
        2060 | 2061 => 714,
        other => other,
    })
}

/// `NeverSuppressError`: errors the client alerts even with error capture
/// on (script-runtime.md 4.4): internal codes outside 5000-5499, and file
/// errors of Export Records shown "With dialog".
pub fn never_suppressed(code: ErrorCode, step: u16, with_dialog: bool) -> bool {
    code.is_internal()
        || (step == 36
            && with_dialog
            && matches!(code.0, 200 | 800 | 801 | 804 | 805 | 808 | 811 | 814))
}

/// `SharedErrorsThatCauseConfirmAlert`: errors the client reports with a
/// continue/cancel alert rather than a plain one (script-runtime.md 4.4).
pub fn confirm_alert(code: ErrorCode) -> bool {
    matches!(
        code.0,
        2 | 4 | 100 | 104 | 204 | 300 | 404 | 406 | 802 | 809 | 810 | 814
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_have_catalog_messages_and_ranges() {
        assert_eq!(ErrorCode(4).message(), Some("Command is unknown"));
        assert_eq!(
            ErrorCode::TRIGGER_CANCELED.to_string(),
            "Command/operation canceled by triggered script (error 20)"
        );
        assert!(ErrorCode(3001).is_internal() && ErrorCode(6000).is_internal());
        assert!(!ErrorCode(5001).is_internal() && ErrorCode(5001).is_custom());
        assert_eq!(map_error(ErrorCode(2052)), ErrorCode(401));
        assert_eq!(map_error(ErrorCode(401)), ErrorCode(401));
        assert!(never_suppressed(ErrorCode(4100), 7, false));
        assert!(never_suppressed(ErrorCode(800), 36, true));
        assert!(!never_suppressed(ErrorCode(800), 36, false));
        assert!(confirm_alert(ErrorCode(4)) && !confirm_alert(ErrorCode(101)));
    }
}
