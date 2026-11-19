//! What the console shows of the server's certificate: the SSL Certificate
//! view's Certificate Information and Details, and the Dashboard's expiry
//! (`docs/re/admin-console.md` 4.1 and 4.5).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use x509_parser::prelude::{FromDer, X509Certificate, X509Name};

/// The parts of a certificate the console shows.
#[allow(dead_code)] // For the SSL Certificate view and the Dashboard, still to come.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Summary {
    /// The subject's common name: "Issued To".
    pub(crate) issued_to: Option<String>,
    /// The issuer's common name, or its organization: "Issued By".
    pub(crate) issued_by: Option<String>,
    pub(crate) not_before: SystemTime,
    pub(crate) expires: SystemTime,
    /// The subject's country, state or province, locality and organization.
    pub(crate) country: Option<String>,
    pub(crate) state: Option<String>,
    pub(crate) locality: Option<String>,
    pub(crate) organization: Option<String>,
    /// Issued by itself, as a certificate the server made is.
    pub(crate) self_signed: bool,
}

/// The summary of the DER certificate `der`, or `None` when it cannot be
/// read.
#[allow(dead_code)] // For the SSL Certificate view and the Dashboard, still to come.
pub(crate) fn summarize(der: &[u8]) -> Option<Summary> {
    let (_, cert) = X509Certificate::from_der(der).ok()?;
    let time = |seconds: i64| {
        u64::try_from(seconds)
            .ok()
            .map(|s| UNIX_EPOCH + Duration::from_secs(s))
    };
    let validity = cert.validity();
    Some(Summary {
        issued_to: common_name(cert.subject()),
        issued_by: common_name(cert.issuer()).or_else(|| organization(cert.issuer())),
        not_before: time(validity.not_before.timestamp())?,
        expires: time(validity.not_after.timestamp())?,
        country: first(cert.subject().iter_country()),
        state: first(cert.subject().iter_state_or_province()),
        locality: first(cert.subject().iter_locality()),
        organization: organization(cert.subject()),
        self_signed: cert.subject() == cert.issuer(),
    })
}

fn first<'a>(
    mut values: impl Iterator<Item = &'a x509_parser::x509::AttributeTypeAndValue<'a>>,
) -> Option<String> {
    values
        .next()
        .and_then(|value| value.as_str().ok())
        .map(str::to_owned)
}

fn common_name(name: &X509Name) -> Option<String> {
    first(name.iter_common_name())
}

fn organization(name: &X509Name) -> Option<String> {
    first(name.iter_organization())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_made_certificate_reads_back() {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
        let summary = summarize(generated.cert.der()).unwrap();
        assert!(summary.self_signed);
        assert!(summary.expires > summary.not_before);
        assert_eq!(summarize(b"not a certificate"), None);
    }
}
