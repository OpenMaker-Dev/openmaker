//! Read-only inventory of a local file's library images and the buttons and
//! graphics that show them (library-images.md).
//! `cargo run -p om-layout --example library_images -- <file.fmp12>`
use om_hbam::HbamFile;
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let file = HbamFile::open(std::env::args_os().nth(1).ok_or("file required")?)?;
    let tree = file.key_tree()?;
    for raw in &file.catalog()?.layouts {
        let layout = om_layout::Layout::decode_from_tree(&tree, raw.id)?.unwrap();
        for (key, image) in &layout.library_images {
            println!(
                "layout={} image={} format={:?} bytes={}",
                layout.id,
                key,
                image.format,
                image.data.len()
            );
        }
        for object in &layout.objects.objects {
            if let Some(button) = &object.button {
                println!(
                    "layout={} button={} key={} size={} display={}",
                    layout.id, object.id, button.library_key, button.icon_size, button.display_type
                );
            }
            if let Some(graphic) = &object.graphic {
                println!(
                    "layout={} graphic={} key={} format={}",
                    layout.id, object.id, graphic.library_key, graphic.pict_format
                );
            }
        }
    }
    Ok(())
}
