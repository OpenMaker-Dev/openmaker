//! The shipped Contacts template's web viewer decodes as FileMaker Pro
//! stores it (layout-model.md 9.6): the "website" object on Contact
//! Details is an external object of type `WEBV` with its setup options in
//! `externalObj.flags`, and its web address is the formula under the
//! two-byte attribute key `00 00`.
//!
//! Ignored by default: the template exists only where FileMaker Pro is
//! installed or extracted. Run with
//!   cargo test -p om-layout --test web_viewers -- --ignored
//! `OM_CONTACTS_TEMPLATE` names another copy of `Contacts/en.fmp12`.

use std::path::PathBuf;

use om_hbam::HbamFile;
use om_layout::{ExternalObject, Layout, ObjectKind};

const CANDIDATES: [&str; 3] = [
    "../../re/extracted/mac/FileMaker Pro 26/FileMaker Pro.app/Contents/Resources/DynamicTemplates/Contacts/en.fmp12",
    "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates/Contacts/en.fmp12",
    "/Applications/FileMaker Pro.app/Contents/Resources/DynamicTemplates/Contacts/en.fmp12",
];

fn template() -> Option<PathBuf> {
    if let Some(path) = std::env::var_os("OM_CONTACTS_TEMPLATE").map(PathBuf::from) {
        assert!(
            path.is_file(),
            "OM_CONTACTS_TEMPLATE {path:?} is not a file"
        );
        return Some(path);
    }
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

#[test]
#[ignore = "requires the shipped Contacts template"]
fn the_contacts_web_viewer_decodes_its_type_options_and_address() {
    let Some(path) = template() else {
        eprintln!("no Contacts template; set OM_CONTACTS_TEMPLATE");
        return;
    };
    let file = HbamFile::open(&path).unwrap();
    let tree = file.key_tree().unwrap();
    let catalog = om_hbam::Catalog::from_tree(&tree).unwrap();
    let id = catalog
        .layouts
        .iter()
        .find(|l| l.name == "Contact Details")
        .unwrap()
        .id;
    let layout = Layout::decode_from_tree(&tree, id).unwrap().unwrap();
    let viewers: Vec<_> = layout
        .objects
        .objects
        .iter()
        .filter(|o| o.web_viewer().is_some())
        .collect();
    assert_eq!(viewers.len(), 1);
    let viewer = viewers[0];
    assert_eq!(viewer.kind(), ObjectKind::External);
    assert_eq!(viewer.name.as_deref(), Some("website"));
    assert_eq!(viewer.style.as_ref().map(|s| s.object_type_id), Some(86));
    let external = viewer.external.as_ref().unwrap();
    assert_eq!(
        (external.type_code, external.sub_type, external.flags),
        (ExternalObject::WEB_VIEWER, 0, 0x8003)
    );
    let options = viewer.web_viewer().unwrap();
    assert!(options.allows_interaction());
    assert!(options.shows_in_find_mode());
    assert!(options.encodes_url());
    assert!(!options.shows_progress_bar() && !options.shows_status_messages());
    assert!(!options.allows_javascript_scripts());
    // The address prefixes the Website field with "https://" when it has a
    // value. It stores no context of its own (`01 00`): it is evaluated in
    // the layout's.
    let address = viewer.calcs.web_address.as_ref().unwrap();
    assert_eq!(address.occurrence(), None);
    let polish = om_formula::polish::encode(&address.polish);
    let https: Vec<u8> = b"https://".iter().map(|b| b ^ 0x5a).collect();
    assert!(polish.windows(https.len()).any(|w| w == https));
    assert!(viewer.calcs.undecodable.is_empty());
}
