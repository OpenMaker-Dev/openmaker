//! Send Mail (63) in a server script session: by SMTP only, as Claris
//! documents for server-side scripts, through the client the desktop
//! uses (`om_url::send_smtp`), ending by the script's time limit or
//! abort. The email client and OAuth are 3, as are what the desktop also
//! refuses: attachments, one email per record, and addresses collected
//! across a found set of more than one record.

use om_scripts::runtime::{HostError, HostResult, Mail, MailVia};

use super::ServerHost;

impl ServerHost {
    /// [`om_scripts::runtime::Network::send_mail`].
    pub(super) fn send_mail_step(&mut self, mail: &Mail) -> HostResult {
        if mail.via != MailVia::Smtp || mail.one_per_record || !mail.attachments.trim().is_empty() {
            return Err(HostError::Unsupported);
        }
        if mail.collect.iter().any(|&c| c) && self.window()?.found.len() > 1 {
            return Err(HostError::Unsupported);
        }
        let (account, message) =
            om_url::smtp_parts(mail).map_err(|e| HostError::Error(e.code()))?;
        let deadline = self.control.deadline;
        om_url::send_smtp(&account, &message, deadline, &mut || self.aborted())
            .map_err(|e| HostError::Error(e.code()))
    }
}
