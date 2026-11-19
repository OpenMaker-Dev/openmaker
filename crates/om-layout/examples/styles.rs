//! Read-only style inspection of a local file: with object ids, each object's
//! resolved style per component and state (`OPENMAKER_INSPECT_ATTRIBUTES=1`
//! adds its stored attributes); without, the file's fonts and the theme's
//! enumerated properties.
//! `cargo run -p om-layout --example styles -- <file.fmp12> <layout id> [object id...]`
use om_hbam::HbamFile;
use om_layout::{style::resolve_property, Layout};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("missing file")?;
    let layout_id = args
        .next()
        .ok_or("missing layout id")?
        .to_string_lossy()
        .parse()?;
    let file = HbamFile::open(path)?;
    let tree = file.key_tree()?;
    let layout = Layout::decode_from_tree(&tree, layout_id)?.ok_or("missing layout")?;
    let ids: Vec<_> = args.collect();
    if ids.is_empty() {
        if let Some(fonts) = tree.at(&[25]) {
            fonts.walk(&mut |p, n| {
                if let Some(b) = n.assembled() {
                    println!("FONT {p:?} {:02x?} text={:?}", b, n.text())
                }
            })
        }
        for node in tree.at(&[6, 5]).into_iter().flat_map(|n| n.children.iter()) {
            let Some(bytes) = node.get(22).and_then(|n| n.assembled()) else {
                continue;
            };
            let theme = om_layout::ThemeStyles::decode(&bytes)?;
            for set in theme.type_styles.iter().chain(&theme.custom_styles) {
                if let Some(styles) = &set.styles {
                    for c in &styles.components {
                        for p in &c.properties {
                            if [64, 65, 67, 74, 75, 611].contains(&p.id) {
                                println!(
                                    "ENUM T{:?} {:?} {:?} C{} S{} ID{} int={:?} string={:?}",
                                    node.id(),
                                    set.name,
                                    set.object_types,
                                    c.component,
                                    c.state,
                                    p.id,
                                    p.int,
                                    p.string
                                )
                            }
                        }
                    }
                }
            }
        }
    }
    for id in ids {
        let id = id.to_string_lossy().parse()?;
        let object = layout.objects.get(id).ok_or("missing object")?;
        println!(
            "OBJECT {id} {:?} {:?}",
            object.kind(),
            object.style.as_ref().map(|s| &s.custom_style_names)
        );
        println!(
            "FLAGS object={:#x} field={:?}",
            object.flags, object.field_obj
        );
        if std::env::var_os("OPENMAKER_INSPECT_ATTRIBUTES").is_some() {
            if let Some(attrs) = tree.at(&[4, 5, layout.id, 5, id]) {
                attrs.walk(&mut |p, n| {
                    if let Some(b) = n.assembled() {
                        println!(
                            "ATTR {p:?} len={} hex={:02x?} text={:?}",
                            b.len(),
                            &b[..b.len().min(40)],
                            n.text()
                        )
                    }
                })
            }
        }
        for state in [1, 7, 18] {
            for component in [1, 2, 3, 9, 10, 18, 24] {
                let s = layout.object_style(object, component, state);
                if s != Default::default() {
                    println!("C{component} S{state} {s:?}")
                }
                for prop in [64, 65, 67, 74, 75, 611] {
                    if let Some(p) = resolve_property(
                        object.style.as_ref(),
                        1,
                        layout.def.as_ref().and_then(|d| d.local_styles.as_ref()),
                        &layout.theme_styles,
                        component,
                        state,
                        prop,
                    ) {
                        println!(" C{component} S{state} property{prop} int={:?}", p.int)
                    }
                }
            }
        }
    }
    Ok(())
}
