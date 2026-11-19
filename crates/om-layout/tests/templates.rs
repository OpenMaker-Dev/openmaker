//! Decode the layouts of a shipped FileMaker template.
//!
//! Ignored by default: the templates are git-ignored extractions that exist
//! only on the analysis host. Run with:
//!   cargo test -p om-layout --test templates -- --ignored
use std::collections::HashSet;

use om_hbam::HbamFile;
use om_layout::{ObjectKind, PartKind};

const CONTACTS: &str = "../../re/extracted/win/msi/PFiles64/FileMaker/FileMaker Pro/DynamicTemplates/Contacts/en.fmp12";

#[test]
#[ignore = "requires the git-ignored extracted templates"]
fn contacts_layouts() {
    let catalog = HbamFile::open(CONTACTS).unwrap().catalog().unwrap();
    let layouts = om_layout::decode_all(&catalog).unwrap();
    let names: Vec<_> = layouts.iter().map(|l| l.name.as_str()).collect();
    assert_eq!(names, ["Contacts", "Contact List", "Contact Details", "--"]);

    for layout in &layouts {
        let name = &layout.name;
        assert_eq!(layout.width(), 1024.0, "{name}");
        // Parts stack top to bottom with no gaps.
        let mut bottom = 0.0;
        for part in &layout.parts {
            assert_eq!(part.top, bottom, "{name} part {}", part.id);
            assert!(part.height > 0.0, "{name} part {}", part.id);
            bottom = part.bottom();
        }
        assert_eq!(layout.height(), bottom, "{name}");

        // The object tree: unique ids, every object reachable from the root
        // list or a container, and children inside their parent's box.
        let store = &layout.objects;
        let ids: HashSet<_> = store.objects.iter().map(|o| o.id).collect();
        assert_eq!(ids.len(), store.len(), "{name}");
        for obj in &store.objects {
            let ctx = format!("{name} object {}", obj.id);
            let b = obj.bounds.as_ref().expect(&ctx);
            assert!(b.width() > 0.0 && b.height() > 0.0, "{ctx}");
            assert!(obj.children.iter().all(|c| ids.contains(c)), "{ctx}");
            match obj.parent.map(|p| store.get(p).unwrap()) {
                None => {
                    assert!(store.root.contains(&obj.id), "{ctx}");
                    assert!(
                        b.right <= layout.width() && b.bottom <= layout.height(),
                        "{ctx}"
                    );
                }
                Some(parent) => {
                    assert!(parent.children.contains(&obj.id), "{ctx}");
                    let pb = parent.bounds.as_ref().unwrap();
                    assert!(b.top >= 0.0 && b.left >= 0.0, "{ctx}");
                    assert!(b.bottom <= pb.height() && b.right <= pb.width(), "{ctx}");
                }
            }
        }
    }
    // "Contacts" is a blank layout.
    assert!(layouts[0].parts.is_empty() && layouts[0].objects.is_empty());

    let details = layouts
        .iter()
        .find(|l| l.name == "Contact Details")
        .unwrap();
    let def = details.def.as_ref().unwrap();
    let parts: Vec<_> = details
        .parts
        .iter()
        .map(|p| (p.kind(), p.top, p.height))
        .collect();
    assert_eq!(
        parts,
        [
            (PartKind::TopNavigation, 0.0, 44.0),
            (PartKind::Body, 44.0, 528.0)
        ]
    );

    let fields: Vec<_> = details
        .objects
        .objects
        .iter()
        .filter(|o| o.kind() == ObjectKind::Field)
        .collect();
    assert_eq!(fields.len(), 19);
    assert!(fields.iter().all(|f| f.field_spec().is_some()));

    // A top-level field on the layout's own table occurrence: Contacts
    // "First Name" (field 1), in layout coordinates inside the body.
    let contacts = catalog.table_by_name("Contacts").unwrap();
    let first_name = fields
        .iter()
        .find(|f| {
            f.parent.is_none()
                && f.field_spec().unwrap().table_key == def.table_alias_key
                && contacts
                    .field(f.field_spec().unwrap().field_key)
                    .unwrap()
                    .name
                    == "First Name"
        })
        .unwrap();
    let b = first_name.bounds.as_ref().unwrap();
    assert_eq!(
        (b.top, b.left, b.bottom, b.right),
        (393.0, 32.0, 437.0, 323.0)
    );
    let body = &details.parts[1];
    assert!(b.top >= body.top && b.bottom <= body.bottom());

    // A field nested in tab control > tab panel > portal is stored relative
    // to its portal and bound to another table occurrence.
    let nested = fields
        .iter()
        .find(|f| f.field_spec().unwrap().table_key != def.table_alias_key)
        .unwrap();
    let portal = details.objects.get(nested.parent.unwrap()).unwrap();
    let panel = details.objects.get(portal.parent.unwrap()).unwrap();
    let tabs = details.objects.get(panel.parent.unwrap()).unwrap();
    assert_eq!(
        (portal.kind(), panel.kind(), tabs.kind(), tabs.parent),
        (
            ObjectKind::Portal,
            ObjectKind::TabPanel,
            ObjectKind::TabControl,
            None
        )
    );
}
