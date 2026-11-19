//! Insert from URL in a server script session (Claris "Insert from URL";
//! script oracle s34-containers-http): HTTP and HTTPS through the client
//! the desktop uses (`om_url`), with its limits (a 256 MB body, timeouts),
//! ending by the script's time limit; the script's abort ends the step at
//! any point of the transfer. A `file:` URL names a file only
//! beneath the Documents and the session's temporary folders, as Claris
//! documents for server-side scripts, opened through their handles like
//! an import ([`super::files`]); any other is 100, as on FileMaker
//! Server's engine (s34-containers-url). The response goes into a
//! container field as container data, into another field as text, into a
//! variable as text or, with `--FM-return-container-variable`, container
//! data ([`om_url::response`]).

use std::io::Read;

use om_hbam::DataType;
use om_scripts::runtime::{ErrorCode, HostError, HostResult, UrlRequest, UrlResponse};

use super::ServerHost;

/// "File is missing".
const FILE_MISSING: i32 = 100;

impl ServerHost {
    /// [`om_scripts::runtime::Files::fetch_url`].
    pub(super) fn fetch_url_step(&mut self, request: &UrlRequest) -> HostResult<UrlResponse> {
        let into = match request.target {
            None => om_url::Into::Variable,
            Some(target) => {
                // A server session has no active field.
                let field = target.field.ok_or(HostError::Unsupported)?;
                let (_, def) = self.target(field)?;
                if def.data_type == DataType::Container {
                    om_url::Into::Container
                } else {
                    om_url::Into::TextField
                }
            }
        };
        let url = om_url::request_url(request);
        let fetched = match om_url::file_path(&url) {
            Some(path) => {
                let file = self
                    .open_for_reading(&[format!("file:{path}")])
                    .ok_or(ErrorCode(FILE_MISSING))?;
                let mut body = Vec::new();
                file.take(om_url::MAX_FILE + 1)
                    .read_to_end(&mut body)
                    .map_err(|_| ErrorCode(FILE_MISSING))?;
                if body.len() as u64 > om_url::MAX_FILE {
                    return Err(ErrorCode(FILE_MISSING).into());
                }
                om_url::Fetched {
                    url,
                    body,
                    container_variable: om_url::container_variable(request),
                    ..Default::default()
                }
            }
            None => om_url::fetch(request, self.control.deadline, &mut || self.aborted())?,
        };
        Ok(om_url::response(fetched, into)?)
    }
}
