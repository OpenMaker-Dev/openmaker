//! Administration > Administrator (`docs/re/admin-console.md` 4.8): the
//! console's account. OpenMaker Server takes it from its options when it
//! starts (`docs/admin-console.md`, "URLs and options"), so the view shows
//! it without FileMaker Server's Change buttons.

use maud::{html, Markup};

use crate::components::{row, section, title};
use crate::views::Ctx;

const ACCOUNT_HELP: &str = "The account is set when OpenMaker Server starts: the user name \
    with --admin-user, and the password in the file --admin-password-file names or in the \
    OM_ADMIN_PASSWORD environment variable. To change them, restart the server.";

pub(crate) fn page(ctx: &Ctx) -> Markup {
    html! {
        (title("Administrator"))
        (section("User Name and Password", Some(ACCOUNT_HELP)))
        (row("User Name", None, html! { (ctx.session.user) }, html! {}))
        (row("Password", None, html! { "********" }, html! {}))
        (section("FileMaker Admin API Public Keys", None))
        p.unavailable { "Public keys are not available in OpenMaker Server yet." }
    }
}
