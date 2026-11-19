//! Read-only layout inventory of a local file: each layout's size, default
//! view and theme, then every object's kind, bounds, field and resolved
//! style. `cargo run -p om-layout --example inspect -- <file.fmp12>`
use om_hbam::HbamFile;
use om_layout::{Layout, LayoutObject};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: inspect <file.fmp12>")?;
    let file = HbamFile::open(path)?;
    let tree = file.key_tree()?;
    let catalog = om_hbam::Catalog::from_tree(&tree)?;
    for raw in &catalog.layouts {
        let layout = Layout::decode_from_tree(&tree, raw.id)?.ok_or("missing layout")?;
        let flags = layout.def.as_ref().map_or(0, |def| def.flags);
        let view = if flags & 0x4000 != 0 {
            "Table"
        } else if flags & 0x200 != 0 {
            "List"
        } else {
            "Form"
        };
        println!(
            "{} {} {}x{} view={view} flags={flags:#x} theme={:?} type styles={} custom={}",
            layout.id,
            layout.name,
            layout.width(),
            layout.height(),
            layout.theme_key,
            layout.theme_styles.type_styles.len(),
            layout.theme_styles.custom_styles.len()
        );
        for object in &layout.objects.objects {
            println!("  {}", describe(&layout, object));
        }
    }
    Ok(())
}

/// One object: kind, bounds, field, text and its resolved box and text style.
fn describe(layout: &Layout, object: &LayoutObject) -> String {
    let style = layout.object_style(object, 1, 1);
    let text = layout.object_style(object, 3, 1);
    format!(
        "{} {:?} {:?} {:?} text={:?} bg={:?} fg={:?} size={:?} font={:?} align={:?} weight={:?}",
        object.id,
        object.kind(),
        layout.absolute_bounds(object.id),
        object.field_spec(),
        object.text,
        style.background,
        text.color.or(style.color),
        text.font_size.or(style.font_size),
        text.font_family.or(style.font_family),
        text.text_align.or(style.text_align),
        text.font_weight.or(style.font_weight)
    )
}
