//! Read-only object tree of a local file's layouts, in stored order: each
//! object's depth, id, kind, name, stored and absolute bounds, rotation,
//! field, repetitions and flags, for comparing with the Design functions
//! (`LayoutObjectNames`, `FieldNames`, `FieldBounds`) and
//! `GetLayoutObjectAttribute`.
//! `cargo run -p om-layout --example objects -- <file.fmp12> [layout name]`
use om_hbam::HbamFile;
use om_layout::{Layout, LayoutObject};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let path = args.next().ok_or("usage: objects <file.fmp12> [layout]")?;
    let only = args.next().map(|a| a.to_string_lossy().into_owned());
    let file = HbamFile::open(path)?;
    let tree = file.key_tree()?;
    let catalog = om_hbam::Catalog::from_tree(&tree)?;
    for raw in &catalog.layouts {
        if only.as_ref().is_some_and(|name| *name != raw.name) {
            continue;
        }
        let layout = Layout::decode_from_tree(&tree, raw.id)?.ok_or("missing layout")?;
        println!("layout {} {:?}", layout.id, layout.name);
        for part in &layout.parts {
            println!(
                "  part {:?} top {} height {}",
                part.kind(),
                part.top,
                part.height
            );
        }
        println!(
            "  objectDef order: {:?}",
            layout
                .objects
                .objects
                .iter()
                .map(|o| o.id)
                .collect::<Vec<_>>()
        );
        for id in &layout.objects.root {
            print_tree(&layout, *id, 1);
        }
    }
    Ok(())
}

fn print_tree(layout: &Layout, id: u64, depth: usize) {
    let Some(object) = layout.objects.get(id) else {
        println!("{}{id} missing", "  ".repeat(depth));
        return;
    };
    println!("{}{}", "  ".repeat(depth), describe(layout, object));
    for child in &object.children {
        print_tree(layout, *child, depth + 1);
    }
}

fn describe(layout: &Layout, object: &LayoutObject) -> String {
    let rect = |r: Option<&om_layout::Rect>| {
        r.map_or("-".to_string(), |r| {
            format!("{} {} {} {}", r.left, r.top, r.right, r.bottom)
        })
    };
    let field = object.field_obj.as_ref().map_or(String::new(), |f| {
        format!(
            " field={:?} reps={} flags={:#x} display={} vl={}",
            f.field_spec
                .as_ref()
                .map(|s| (s.table_key, s.field_key, s.repetition)),
            f.num_of_reps,
            f.flags,
            f.display_type,
            f.value_list_key
        )
    });
    let border = if object.kind() == om_layout::ObjectKind::Portal {
        let b = |c| {
            let s = layout.object_style(object, c, 1);
            format!("{:?}/{:?}/{:?}", s.border_width, s.border_style, s.padding)
        };
        format!(
            " portal={:?} self={} inner={} contents={} row={}",
            object.portal.as_ref().map(|p| (
                p.flags,
                p.initial_row,
                p.num_rows,
                p.scrollbar_style,
                p.other.len()
            )),
            b(1),
            b(2),
            b(8),
            b(12)
        )
    } else {
        String::new()
    };
    format!(
        "{} {:?}{border} name={:?} stored=[{}] abs=[{}] rot={} flags={:#x}{field} text={:?}",
        object.id,
        object.kind(),
        object.name,
        rect(object.bounds.as_ref()),
        rect(layout.absolute_bounds(object.id).as_ref()),
        object.rotation,
        object.flags,
        object
            .text
            .as_deref()
            .map(|t| t.chars().take(30).collect::<String>()),
    )
}
