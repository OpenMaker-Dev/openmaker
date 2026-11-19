//! The console's tabs and their views, with FileMaker Server's routes and
//! titles (`docs/re/admin-console.md` section 2). Remote Backups and Claris
//! MCP, Claris cloud services, are left out (`docs/admin-console.md`). The
//! sidebar introductions are OpenMaker's own words.

use crate::icons::Icon;

/// A top-level tab: `/admin-console/app/<route>`.
pub(crate) struct Tab {
    pub(crate) route: &'static str,
    pub(crate) title: &'static str,
    /// The sidebar's introduction; empty for a full-width tab.
    pub(crate) intro: &'static str,
    /// The sidebar's sub-views, `/admin-console/app/<tab>/<view>`; the bare
    /// tab URL opens the first. None for a full-width tab.
    pub(crate) views: &'static [View],
    /// Whether the sidebar items are the shorter kind.
    pub(crate) compact: bool,
}

/// A sub-view in a tab's sidebar.
pub(crate) struct View {
    pub(crate) route: &'static str,
    pub(crate) title: &'static str,
    pub(crate) icon: Icon,
}

const fn view(route: &'static str, title: &'static str, icon: Icon) -> View {
    View { route, title, icon }
}

pub(crate) const TABS: &[Tab] = &[
    Tab {
        route: "dashboard",
        title: "Dashboard",
        intro: "",
        views: &[],
        compact: false,
    },
    Tab {
        route: "activities",
        title: "Databases",
        intro: "",
        views: &[],
        compact: false,
    },
    Tab {
        route: "backups",
        title: "Backups",
        intro: "Backups copy the hosted databases on a schedule or on demand. \
                Each backup schedule keeps its latest backups and deletes \
                older ones, but never a preserved backup. Back Up Now runs \
                the chosen schedule at once; preserve backups under \
                Auto-Backup and On-Demand, and delete them under Preserved.",
        views: &[
            view("allbackups", "Auto-Backup and On-Demand", Icon::Backup),
            view("preservedbackups", "Preserved", Icon::Preserved),
            view("backupsettings", "Backup Settings", Icon::Gear),
        ],
        // Measured on the reference screenshots, the Backups items are as
        // tall as the other tabs' (docs/re/admin-console.md 3).
        compact: false,
    },
    Tab {
        route: "configuration",
        title: "Configuration",
        intro: "Choose a tab below to view or change settings.",
        views: &[
            view("generalsettings", "General Settings", Icon::Gear),
            view("filemakerclients", "FileMaker Clients", Icon::Clients),
            view("folders", "Folders", Icon::Folder),
            view("schedules", "Schedules", Icon::Schedule),
            view("emailnotifications", "Notifications", Icon::Bell),
            view("sslcertificate", "SSL Certificate", Icon::Certificate),
        ],
        compact: false,
    },
    Tab {
        route: "connectors",
        title: "Connectors",
        intro: "Connectors let other software read and change hosted data, or \
                add features to hosted databases. Choose a tab below to set \
                them up.",
        views: &[
            view("webpublishing", "Web Publishing", Icon::Web),
            view("fmdapi", "FileMaker Data API", Icon::Api),
            view("plugins", "Plug-ins", Icon::Plugin),
            view("xdbc", "ODBC / JDBC", Icon::Database),
            view("fmodata", "OData", Icon::Link),
        ],
        compact: false,
    },
    Tab {
        route: "llm",
        title: "AI Services",
        intro: "AI services run language models on this server for hosted \
                databases, and manage the keys that give access to them.",
        views: &[
            view("opensourcellm", "Model Server", Icon::Chip),
            view("llmkeys", "Keys", Icon::Key),
        ],
        compact: false,
    },
    Tab {
        route: "administration",
        title: "Administration",
        intro: "Manage the server's licenses, the Admin Console account, and \
                who may administer the server and how they sign in. Choose a \
                tab below.",
        views: &[
            view("licenses", "FileMaker Licenses", Icon::License),
            view("administrators", "Administrator", Icon::Person),
            view("restrictaccess", "Restrict Access", Icon::Shield),
            view("adminroles", "Administrator Roles", Icon::People),
            view(
                "externalauthentication",
                "External Authentication",
                Icon::Badge,
            ),
        ],
        compact: false,
    },
    Tab {
        route: "logging",
        title: "Logs",
        intro: "View and download the server's logs.",
        views: &[
            view("logviewer", "Log Viewer", Icon::Log),
            view("logsettings", "Log Settings", Icon::Gear),
        ],
        compact: false,
    },
];

/// The tab at `route`.
pub(crate) fn tab(route: &str) -> Option<&'static Tab> {
    TABS.iter().find(|t| t.route == route)
}

impl Tab {
    /// The sub-view at `route`.
    pub(crate) fn view(&self, route: &str) -> Option<&'static View> {
        self.views.iter().find(|v| v.route == route)
    }

    pub(crate) fn url(&self) -> String {
        format!("/admin-console/app/{}", self.route)
    }

    pub(crate) fn view_url(&self, view: &View) -> String {
        format!("/admin-console/app/{}/{}", self.route, view.route)
    }
}
