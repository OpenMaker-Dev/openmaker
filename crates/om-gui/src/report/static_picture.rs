//! Static report pictures, separate from button icon tinting/rasterization.
use super::svg_worker::Unsupported;
use super::{LibraryImage, LibraryImageFormat, Picture, ReportError, MAX_PICTURE_SIDE};
use image::{DynamicImage, ImageDecoder, ImageFormat, ImageReader};
use resvg::{tiny_skia, usvg};
use std::io::{Cursor, Read};
use std::sync::{Arc, OnceLock};

pub(super) const MAX_BYTES: u64 = 256 << 20;
const MAX_SVG_BYTES: u64 = 4 << 20;
const MAX_SVG_NODES: usize = 50_000;
const MAX_DEPTH: usize = 64;
pub(super) const MAX_SVG_PIXELS: f64 = 16_777_216.0;

pub(super) fn raster_size(width: f64, height: f64) -> Result<[u32; 2], ReportError> {
    if !width.is_finite() || !height.is_finite() || width <= 0.0 || height <= 0.0 {
        return Err(ReportError::unsupported(
            "A report SVG picture has invalid dimensions",
        ));
    }
    // 288 dpi where possible, capped by both side and pixel count.
    let scale = 4.0_f64
        .min(f64::from(MAX_PICTURE_SIDE) / width.max(height))
        .min((MAX_SVG_PIXELS / width / height).sqrt());
    Ok([
        (width * scale).floor().max(1.0) as u32,
        (height * scale).floor().max(1.0) as u32,
    ])
}

pub(super) fn decode(
    key: u64,
    source: &LibraryImage,
    size: Option<[u32; 2]>,
) -> Result<Picture, ReportError> {
    let decoded = std::panic::catch_unwind(|| {
        if source.data.len() as u64 > MAX_BYTES {
            return Err("encoded picture exceeds 256 MiB".into());
        }
        if source.format == LibraryImageFormat::Svg {
            svg(&source.data, size, Unsupported::Refuse)
        } else {
            raster(&source.data, format(source.format))
        }
    });
    decoded
        .unwrap_or_else(|_| Err("picture decoder failed".into()))
        .map_err(|error| {
            ReportError::unsupported(format!(
                "Library image {key} could not be decoded for the report: {error}"
            ))
        })
}

fn format(format: LibraryImageFormat) -> ImageFormat {
    match format {
        LibraryImageFormat::Png => ImageFormat::Png,
        LibraryImageFormat::Jpeg => ImageFormat::Jpeg,
        LibraryImageFormat::Gif => ImageFormat::Gif,
        LibraryImageFormat::Bmp => ImageFormat::Bmp,
        LibraryImageFormat::Tiff => ImageFormat::Tiff,
        LibraryImageFormat::WebP => ImageFormat::WebP,
        LibraryImageFormat::Svg => unreachable!("SVG has its own rasterizer"),
    }
}

fn raster(data: &[u8], format: ImageFormat) -> Result<Picture, String> {
    let mut reader = ImageReader::with_format(Cursor::new(data), format);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(MAX_PICTURE_SIDE);
    limits.max_image_height = Some(MAX_PICTURE_SIDE);
    limits.max_alloc = Some(MAX_BYTES);
    reader.limits(limits);
    let mut decoder = reader.into_decoder().map_err(|e| e.to_string())?;
    let (width, height) = decoder.dimensions();
    let bytes = u64::from(width) * u64::from(height) * 4;
    if width == 0
        || height == 0
        || width > MAX_PICTURE_SIDE
        || height > MAX_PICTURE_SIDE
        || bytes > MAX_BYTES
        || decoder.total_bytes() > MAX_BYTES
    {
        return Err("picture dimensions or decoded samples exceed the report limit".into());
    }
    let orientation = decoder.orientation().map_err(|e| e.to_string())?;
    let mut decoded = DynamicImage::from_decoder(decoder).map_err(|e| e.to_string())?;
    decoded.apply_orientation(orientation);
    let rgba = decoded.into_rgba8();
    Ok(picture(rgba.width(), rgba.height(), rgba.into_raw()))
}

fn picture(width: u32, height: u32, rgba: Vec<u8>) -> Picture {
    Picture {
        width,
        height,
        opaque: rgba.as_chunks::<4>().0.iter().all(|p| p[3] == 255),
        rgba,
        texture: OnceLock::new(),
    }
}

/// An SVG picture, drawn in the worker process (`svg_worker`). Unit tests
/// draw it here, with the same code the worker runs; the worker itself is
/// tested from `tests/svg_worker.rs`.
pub(super) fn svg(
    data: &[u8],
    size: Option<[u32; 2]>,
    unsupported: Unsupported,
) -> Result<Picture, String> {
    #[cfg(test)]
    {
        svg_here(data, size, unsupported)
    }
    #[cfg(not(test))]
    {
        let (width, height, rgba) = super::svg_worker::draw(data, size, unsupported)?;
        Ok(picture(width, height, rgba))
    }
}

/// Draw an SVG picture in this process: only the worker (and unit tests)
/// do, since nothing here bounds what usvg and resvg allocate.
pub(super) fn svg_here(
    data: &[u8],
    size: Option<[u32; 2]>,
    unsupported: Unsupported,
) -> Result<Picture, String> {
    let tree = svg_tree(data, unsupported)?;
    let size = match size {
        Some(size) => size,
        None => raster_size(
            f64::from(tree.size().width()),
            f64::from(tree.size().height()),
        )
        .map_err(|e| e.message)?,
    };
    if size[0] == 0
        || size[1] == 0
        || size[0] > MAX_PICTURE_SIDE
        || size[1] > MAX_PICTURE_SIDE
        || f64::from(size[0]) * f64::from(size[1]) > MAX_SVG_PIXELS
    {
        return Err("SVG raster size exceeds the report limit".into());
    }
    let transform = tiny_skia::Transform::from_scale(
        size[0] as f32 / tree.size().width(),
        size[1] as f32 / tree.size().height(),
    );
    let mut pixmap =
        tiny_skia::Pixmap::new(size[0], size[1]).ok_or("SVG raster allocation failed")?;
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    let rgba = pixmap
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .flat_map(|p| {
            egui::Color32::from_rgba_premultiplied(p[0], p[1], p[2], p[3]).to_srgba_unmultiplied()
        })
        .collect();
    Ok(picture(size[0], size[1], rgba))
}

/// The SVG's tree. Nothing outside the picture's own bytes is read: an
/// image's reference is never resolved (and resvg is built without raster
/// decoding), and the only fonts are the bundled ones. With
/// [`Unsupported::Refuse`], what reports do not draw refuses the picture.
/// With [`Unsupported::Skip`] (Browse) the rest is drawn, entity
/// declarations included (Illustrator writes them), and only the
/// worker's memory and time limits bound it.
fn svg_tree(data: &[u8], unsupported: Unsupported) -> Result<usvg::Tree, String> {
    let refuse = unsupported == Unsupported::Refuse;
    let limit = if refuse { MAX_SVG_BYTES } else { MAX_BYTES };
    if data.len() as u64 > limit {
        return Err("SVG input exceeds the picture limit".into());
    }
    let expanded;
    let data = if data.starts_with(&[0x1f, 0x8b]) {
        let mut bytes = Vec::new();
        flate2::read::GzDecoder::new(data)
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| e.to_string())?;
        if bytes.len() as u64 > limit {
            return Err("SVG input exceeds the picture limit".into());
        }
        expanded = bytes;
        expanded.as_slice()
    } else {
        data
    };
    let text = std::str::from_utf8(data).map_err(|e| e.to_string())?;
    // An entity can expand to far more text than the input holds; a DOCTYPE
    // without one (as editors write) is fine.
    if refuse && text.contains("<!ENTITY") {
        return Err("SVG entity declarations are not supported in reports".into());
    }
    let doc = usvg::roxmltree::Document::parse_with_options(
        text,
        usvg::roxmltree::ParsingOptions {
            allow_dtd: true,
            nodes_limit: if refuse {
                MAX_SVG_NODES as u32
            } else {
                u32::MAX
            },
        },
    )
    .map_err(|e| e.to_string())?;
    if refuse {
        check_xml(doc.root_element(), 0)?;
    }
    let options = usvg::Options {
        fontdb: svg_fonts(),
        font_family: "Inter".into(),
        image_href_resolver: usvg::ImageHrefResolver {
            resolve_string: Box::new(|_, _| None),
            resolve_data: Box::new(|_, _, _| None),
        },
        ..Default::default()
    };
    usvg::Tree::from_xmltree(&doc, &options).map_err(|e| e.to_string())
}

/// Refuse what reports do not draw: images, and `use` or `feImage`
/// references outside the picture (both `href` and `xlink:href`).
fn check_xml(node: usvg::roxmltree::Node<'_, '_>, depth: usize) -> Result<(), String> {
    if !node.is_element() {
        return Ok(());
    }
    if depth > MAX_DEPTH {
        return Err("SVG element nesting exceeds the report limit".into());
    }
    let tag = node.tag_name().name();
    if tag == "image" {
        return Err("SVG pictures with images are not supported in reports".into());
    }
    if matches!(tag, "use" | "feImage")
        && node
            .attributes()
            .any(|a| a.name() == "href" && !a.value().starts_with('#'))
    {
        return Err("SVG references outside the picture are not supported in reports".into());
    }
    for child in node.children() {
        check_xml(child, depth + 1)?;
    }
    Ok(())
}

/// The bundled fonts: Inter where the SVG names no family, and Liberation
/// Sans for every generic family, which is what usvg falls back to for a
/// family that is not here (otherwise the text would vanish).
fn svg_fonts() -> Arc<usvg::fontdb::Database> {
    static FONTS: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();
    FONTS
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            for data in [super::REGULAR, super::BOLD, super::SANS, super::SANS_BOLD] {
                db.load_font_data(data.to_vec());
            }
            const FALLBACK: &str = "Liberation Sans";
            db.set_sans_serif_family(FALLBACK);
            db.set_serif_family(FALLBACK);
            db.set_monospace_family(FALLBACK);
            db.set_cursive_family(FALLBACK);
            db.set_fantasy_family(FALLBACK);
            Arc::new(db)
        })
        .clone()
}
