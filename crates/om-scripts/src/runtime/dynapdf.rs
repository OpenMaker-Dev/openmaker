//! The MBS plug-in subset's DynaPDF commands (`super::plugins`,
//! docs/plugins.md): a document made of pages of other PDFs, saved to a
//! file. OpenMaker's own engine answers them; no DynaPDF library is loaded,
//! and `DynaPDF.Initialize`, `DynaPDF.IsInitialized` and the version and
//! license commands stay unanswered (`?`), as without the plug-in. That the
//! commands here work without `DynaPDF.Initialize`, which the plug-in
//! requires first, is OpenMaker's documented departure.
//!
//! - `DynaPDF.New` opens a document and answers its reference number, from
//!   20000 up [O, docs]; `DynaPDF.Release` closes it.
//! - `DynaPDF.OpenPDFFromFile` and `DynaPDF.OpenPDFFromContainer` open the
//!   PDF the imports read (answering 0 [O, docs]); every one opened stays
//!   with the document until it is released.
//! - `DynaPDF.ImportPDFPage` appends one page of it ("OK" [O, docs]),
//!   `DynaPDF.ImportPDFFile` all its pages after the last ("the last page
//!   number that was touched" [O, docs]); `DynaPDF.GetPageCount` counts the
//!   document's pages.
//! - `DynaPDF.OpenOutputFile` names the file `DynaPDF.Save` writes ("OK").
//! - On a page `DynaPDF.EditPage` opened (until `DynaPDF.EndPage`), text:
//!   `DynaPDF.SetFont`, `SetFillColor`, `RGB`, `SetLineWidth`,
//!   `SetTextRect`, `WriteFText` and `WriteFTextEx` (`super::dynapdf_text`),
//!   and `DynaPDF.SetBBox` for the page's boxes. The host lays the text out
//!   when it is written, so text it would not draw is refused then, and at
//!   Save draws it, a page for each page written on, which the worker
//!   draws over the pages. What is undocumented is refused rather than
//!   guessed: EditPage while a page is open, a fill color outside 0 to 1,
//!   text that passes its rectangle (the plug-in would continue it on a
//!   new page).
//! - `DynaPDF.GetPageWidth` and `GetPageHeight` measure the open page's
//!   media box, and `DynaPDF.GetPageBBox` gives a page's box, from the
//!   boxes the PDF worker reads when a PDF is opened and those SetBBox set.
//!   Refused: a turned page, a box the page does not have, and no open
//!   page (the plug-in's default size is not documented).
//! - `DynaPDF.CreateTextField`, `CreateCheckBox` and `CreateSigField` make
//!   form fields on the open page, in the colors `SetFieldTextColor`,
//!   `SetFieldBorderColor` and `SetFieldBackColor` set and the line width;
//!   the worker writes them as widgets of an interactive form
//!   (`pdf_worker::Field`). Fields of one name and type form one field
//!   whose widgets they are (`pdf_worker::group_refusal`). Refused: a
//!   parent, fields of one name and different types, a turned page.
//! - `DynaPDF.CreateButton` makes a push button whose caption the host
//!   lays out like written text; `DynaPDF.CreateJSAction` keeps a
//!   JavaScript as text (never run here) and `DynaPDF.AddActionToObj`
//!   makes a field's widget run it on `OnMouseUp`.
//!
//! PDF data is read and the new document written in the PDF worker process
//! (`om_calc::functions::pdf_worker`), never here; files are read and
//! written through the host's file operations (`super::host::Files`).

use std::collections::BTreeMap;
use std::sync::Arc;

use om_calc::functions::pdf_worker::{
    self, check_fits, export_ok, group_refusal, name_ok, rect_ok, uri_ok, BoxKind, Field,
    FieldKind, Info, Link, PageGeometry, Planned, INFO_PAGES, MAX_ACTIONS, MAX_CAPTION, MAX_COORD,
    MAX_FIELDS, MAX_LINKS, MAX_SCRIPT, MAX_SOURCES,
};
use om_calc::{Ctx, Number, Value, ValueType};

use super::dynapdf_text::{spans, unpack, Align, Face, Style, TextBlock};

use super::data_files::code;
use super::host::HostError;
use super::plugins::{error, PluginHost, OK};

/// The first document reference number [O, docs: "DynaPDF context
/// reference numbers are starting at 20000 and counting up"].
const FIRST: i64 = 20_000;
/// OpenMaker's bounds on what a session's documents hold.
const MAX_DOCUMENTS: usize = 16;
const MAX_PAGES: usize = 10_000;
const MAX_FILE: u64 = 256 << 20;
const MAX_SOURCE_BYTES: usize = 512 << 20;
/// What a document's written text may hold: its text blocks, and their
/// text in bytes.
const MAX_BLOCKS: usize = 10_000;
const MAX_TEXT: usize = 1 << 20;
/// DynaPDF's transparent color, `NO_COLOR` [O, docs: SetFieldBackColor].
const NO_COLOR: f64 = 4_294_967_281.0;

/// A session's open documents.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Documents {
    issued: i64,
    open: BTreeMap<i64, Document>,
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Document {
    /// Every PDF opened for import, kept until the document is released.
    sources: Vec<Source>,
    /// The one the import commands read.
    import: Option<usize>,
    /// The document's pages: a source and its page, from 0.
    pages: Vec<(u32, u32)>,
    /// The native path `DynaPDF.OpenOutputFile` named.
    output: Option<String>,
    /// The page `DynaPDF.EditPage` opened, if one is open.
    open_page: Option<usize>,
    /// The font and size `DynaPDF.SetFont` set.
    font: Option<(Face, f64)>,
    /// The fonts `DynaPDF.SetFont` loaded, whose positions are their handles.
    fonts: Vec<Face>,
    /// The fill color, red, green and blue from 0 to 1 (black at first).
    fill: [f64; 3],
    /// The line width `DynaPDF.SetLineWidth` set, which a form field's
    /// border takes [O, manual: CreateTextField]; 1 at first [I: PDF's
    /// default line width; DynaPDF's is not documented].
    line_width: f64,
    /// `DynaPDF.SetTextRect`'s rectangle: left, top, width, and height
    /// (`None` for -1, no limit).
    text_rect: Option<(f64, f64, f64, Option<f64>)>,
    /// What was written on each page and the boxes set on it, by page.
    edits: BTreeMap<usize, Edits>,
    /// The text blocks written, and their text in bytes.
    blocks: usize,
    text: usize,
    /// The colors new form fields take, as DynaPDF packs them, or
    /// `NO_COLOR`: text, border and background [O, docs: border black and
    /// background `NO_COLOR` at first; text black at first, I].
    field_colors: [f64; 3],
    /// The form fields created, whose positions are their handles, each on
    /// the document's page `Field::page`.
    fields: Vec<Field>,
    /// Each button's caption as the desktop lays it out, by the button's
    /// position among the fields.
    captions: Vec<(usize, TextBlock)>,
    /// The JavaScript actions `DynaPDF.CreateJSAction` made, whose
    /// positions are their handles, and their bytes.
    actions: Vec<String>,
    action_bytes: usize,
    /// The web links `DynaPDF.WebLink` put on pages, whose positions are
    /// their handles.
    links: Vec<Link>,
    highlights: Vec<pdf_worker::Highlight>,
    /// Includes appearances already absorbed into a content snapshot.
    highlight_count: usize,
}

impl Document {
    fn new() -> Document {
        Document {
            line_width: 1.0,
            field_colors: [0.0, 0.0, NO_COLOR],
            ..Document::default()
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
struct Edits {
    blocks: Vec<TextBlock>,
    boxes: Vec<(BoxKind, [f64; 4])>,
}

#[derive(Clone, Debug, PartialEq)]
struct Source {
    data: Arc<[u8]>,
    /// Its page count, and which pages are turned.
    info: Info,
}

impl Document {
    fn source_bytes(&self) -> usize {
        self.sources.iter().map(|s| s.data.len()).sum()
    }
}

/// The DynaPDF commands of the subset.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Command {
    New,
    Release,
    OpenPdfFromFile,
    OpenPdfFromContainer,
    GetPageCount,
    ImportPdfPage,
    ImportPdfFile,
    OpenOutputFile,
    Save,
    EditPage,
    EndPage,
    SetFont,
    SetFillColor,
    SetLineWidth,
    SetTextRect,
    WriteFText,
    WriteFTextEx,
    SetBBox,
    GetPageWidth,
    GetPageHeight,
    GetPageBBox,
    SetFieldTextColor,
    SetFieldBorderColor,
    SetFieldBackColor,
    CreateTextField,
    CreateCheckBox,
    CreateSigField,
    CreateButton,
    CreateJsAction,
    AddActionToObj,
    WebLink,
    FindPattern,
    HighlightPattern,
    FlattenAnnots,
    Rgb,
}

impl Command {
    /// The command a lowercase name names.
    pub fn of(name: &str) -> Option<Self> {
        Some(match name {
            "dynapdf.new" => Self::New,
            "dynapdf.release" => Self::Release,
            "dynapdf.openpdffromfile" => Self::OpenPdfFromFile,
            "dynapdf.openpdffromcontainer" => Self::OpenPdfFromContainer,
            "dynapdf.getpagecount" => Self::GetPageCount,
            "dynapdf.importpdfpage" => Self::ImportPdfPage,
            "dynapdf.importpdffile" => Self::ImportPdfFile,
            "dynapdf.openoutputfile" => Self::OpenOutputFile,
            "dynapdf.save" => Self::Save,
            "dynapdf.editpage" => Self::EditPage,
            "dynapdf.endpage" => Self::EndPage,
            "dynapdf.setfont" => Self::SetFont,
            "dynapdf.setfillcolor" => Self::SetFillColor,
            "dynapdf.setlinewidth" => Self::SetLineWidth,
            "dynapdf.settextrect" => Self::SetTextRect,
            "dynapdf.writeftext" => Self::WriteFText,
            "dynapdf.writeftextex" => Self::WriteFTextEx,
            "dynapdf.setbbox" => Self::SetBBox,
            "dynapdf.getpagewidth" => Self::GetPageWidth,
            "dynapdf.getpageheight" => Self::GetPageHeight,
            "dynapdf.getpagebbox" => Self::GetPageBBox,
            "dynapdf.setfieldtextcolor" => Self::SetFieldTextColor,
            "dynapdf.setfieldbordercolor" => Self::SetFieldBorderColor,
            "dynapdf.setfieldbackcolor" => Self::SetFieldBackColor,
            "dynapdf.createtextfield" => Self::CreateTextField,
            "dynapdf.createcheckbox" => Self::CreateCheckBox,
            "dynapdf.createsigfield" => Self::CreateSigField,
            "dynapdf.createbutton" => Self::CreateButton,
            "dynapdf.createjsaction" => Self::CreateJsAction,
            "dynapdf.addactiontoobj" => Self::AddActionToObj,
            "dynapdf.weblink" => Self::WebLink,
            "dynapdf.findpattern" => Self::FindPattern,
            "dynapdf.highlightpattern" => Self::HighlightPattern,
            "dynapdf.flattenannots" => Self::FlattenAnnots,
            "dynapdf.rgb" => Self::Rgb,
            _ => return None,
        })
    }
}

/// Answer `command` with `args` (after the command's name), or an error
/// text.
pub fn call<H: PluginHost + ?Sized>(
    host: &mut H,
    cx: &Ctx,
    command: Command,
    args: &[Value],
) -> Value {
    match run(host, cx, command, args) {
        Ok(value) => value,
        Err(reason) => Value::text(error(&reason)),
    }
}

fn run<H: PluginHost + ?Sized>(
    host: &mut H,
    cx: &Ctx,
    command: Command,
    args: &[Value],
) -> Result<Value, String> {
    let number = |i: usize| args.get(i).and_then(|v| v.to_number(cx).to_f64());
    let text = |i: usize| args.get(i).map(|v| v.to_text(cx).into_string());
    if command == Command::Rgb {
        // "Blue * 65536 + Green * 256 + Red"; a missing component is 0 [O, docs].
        let mut packed = 0i64;
        for (i, shift) in [(0, 0), (1, 8), (2, 16)] {
            let component = number(i).unwrap_or(0.0);
            if component.fract() != 0.0 || !(0.0..=255.0).contains(&component) {
                return Err("Invalid color component.".into());
            }
            packed |= (component as i64) << shift;
        }
        return Ok(Value::number(packed));
    }
    if command == Command::New {
        let documents = documents(host)?;
        if documents.open.len() >= MAX_DOCUMENTS {
            return Err("Too many open PDF documents.".into());
        }
        let reference = FIRST + documents.issued;
        documents.issued += 1;
        documents.open.insert(reference, Document::new());
        return Ok(Value::number(reference));
    }
    let reference = number(0)
        .filter(|n| n.fract() == 0.0)
        .map(|n| n as i64)
        .ok_or_else(|| "Missing PDF reference.".to_owned())?;
    // Reading a source and writing the document go to the host and the PDF
    // worker before the document is touched.
    let opened = match command {
        Command::OpenPdfFromFile => {
            // PasswordType (0 open, 1 owner, + 2 to repair) means nothing
            // without a password.
            if text(3).is_some_and(|t| !t.is_empty()) {
                return Err("OpenMaker opens no PDF with a password.".into());
            }
            let path = text(1).ok_or_else(|| "Missing path.".to_owned())?;
            document(host, reference)?;
            let data = host
                .plugin_read_file(&path, MAX_FILE)
                .map_err(|e| file_error(&e, "read"))?;
            Some(data)
        }
        Command::OpenPdfFromContainer => {
            document(host, reference)?;
            let Some(Value::Container(container)) = args.get(1) else {
                return Err("No PDF in the container.".into());
            };
            let data = host
                .plugin_container_data(container)
                .filter(|data| !data.is_empty())
                .ok_or_else(|| "No PDF in the container.".to_owned())?;
            if data.len() as u64 > MAX_FILE {
                return Err("The PDF is too large.".into());
            }
            Some(data)
        }
        _ => None,
    };
    if let Some(data) = opened {
        let info = match pdf_worker::info(&data) {
            Ok(Some(info)) if info.optional_content_kept => info,
            Ok(Some(_)) => return Err("OpenMaker cannot keep the layers of this PDF.".into()),
            Ok(None) => return Err("The file is not a PDF OpenMaker can read.".into()),
            Err(_) => return Err("OpenMaker cannot read PDF data here.".into()),
        };
        let document = document(host, reference)?;
        if document.source_bytes().saturating_add(data.len()) > MAX_SOURCE_BYTES {
            return Err("The PDF documents are too large.".into());
        }
        // The worker reads at most MAX_SOURCES PDFs, one of them the text
        // Save draws.
        if document.sources.len() >= MAX_SOURCES - 1 {
            return Err(format!(
                "OpenMaker opens at most {} PDFs into a document.",
                MAX_SOURCES - 1
            ));
        }
        document.sources.push(Source {
            data: data.into(),
            info,
        });
        document.import = Some(document.sources.len() - 1);
        return Ok(Value::number(0));
    }
    if command == Command::Save {
        let document = document(host, reference)?;
        if document.pages.is_empty() {
            return Err("The PDF document has no pages.".into());
        }
        let Some(path) = document.output.clone() else {
            return Err(
                "OpenMaker saves a PDF document only to the file DynaPDF.OpenOutputFile opened."
                    .into(),
            );
        };
        // "If you have an open page, we close it automatically" [O, docs].
        document.open_page = None;
        let mut sources: Vec<Arc<[u8]>> = document
            .sources
            .iter()
            .map(|s| Arc::clone(&s.data))
            .collect();
        let pages = document.pages.clone();
        let edits = document.edits.clone();
        let mut fields = document.fields.clone();
        let captions = document.captions.clone();
        let actions = document.actions.clone();
        let links = document.links.clone();
        let highlights = document.highlights.clone();
        // The text written on the pages is drawn by the host, one page of
        // its own for each, in one PDF the worker draws over the pages.
        let mut plan: Vec<Planned> = pages.iter().copied().map(Planned::from).collect();
        let mut written: Vec<&[TextBlock]> = Vec::new();
        for (page, edit) in &edits {
            plan[*page].boxes = edit.boxes.clone();
            if !edit.blocks.is_empty() {
                plan[*page].overlay = Some((sources.len() as u32, written.len() as u32));
                written.push(&edit.blocks);
            }
        }
        // Each button's caption is a page of the same PDF, after the pages
        // written on; the worker draws it in its button.
        for (field, block) in &captions {
            if let FieldKind::Button { caption_page, .. } = &mut fields[*field].kind {
                *caption_page = Some((sources.len() as u32, written.len() as u32));
            }
            written.push(std::slice::from_ref(block));
        }
        if !written.is_empty() {
            let drawn = host
                .plugin_render_pdf_text(&written)
                .map_err(|e| text_error(&e))?;
            sources.push(drawn.into());
        }
        let sources: Vec<&[u8]> = sources.iter().map(|s| &s[..]).collect();
        let bytes = match pdf_worker::assemble_highlighted(
            &sources,
            &plan,
            &fields,
            &actions,
            &links,
            &highlights,
        ) {
            Ok(Some(bytes)) => bytes,
            Ok(None) => return Err("The PDF document could not be written.".into()),
            Err(_) => return Err("OpenMaker cannot write PDF data here.".into()),
        };
        host.plugin_write_file(&path, &bytes)
            .map_err(|e| file_error(&e, "write"))?;
        return Ok(Value::text(OK));
    }
    if command == Command::Release {
        return match documents(host)?.open.remove(&reference) {
            Some(_) => Ok(Value::text(OK)),
            None => Err("Invalid PDF reference.".into()),
        };
    }
    if command == Command::FlattenAnnots {
        return flatten(host, reference, &text(1).unwrap_or_default());
    }
    if matches!(command, Command::FindPattern | Command::HighlightPattern) {
        return search(host, cx, reference, command, args);
    }
    let document = document(host, reference)?;
    match command {
        Command::GetPageCount => Ok(Value::number(document.pages.len() as i64)),
        Command::OpenOutputFile => {
            let path = text(1)
                .filter(|p| !p.is_empty())
                .ok_or_else(|| "Missing path.".to_owned())?;
            document.output = Some(path);
            Ok(Value::text(OK))
        }
        Command::ImportPdfPage => {
            unscaled(number(2), number(3))?;
            // An open page is closed first [O, docs].
            document.open_page = None;
            let source = import(document)?;
            let pages = document.sources[source].info.pages;
            let index = number(1).unwrap_or(1.0);
            if index.fract() != 0.0 || index < 1.0 || index > f64::from(pages) {
                return Err("Invalid page number.".into());
            }
            if document.pages.len() >= MAX_PAGES {
                return Err("Too many pages.".into());
            }
            document.pages.push((source as u32, index as u32 - 1));
            Ok(Value::text(OK))
        }
        Command::ImportPdfFile => {
            unscaled(number(2), number(3))?;
            document.open_page = None;
            let source = import(document)?;
            let pages = document.sources[source].info.pages;
            let count = document.pages.len();
            // Pages at a destination that already has pages are drawn over
            // them [O, docs]; OpenMaker imports only after the last page.
            let destination = number(1).unwrap_or(1.0);
            if destination != count as f64 + 1.0 {
                return Err("OpenMaker imports a PDF only after the last page (DestPage = DynaPDF.GetPageCount + 1).".into());
            }
            if count + pages as usize > MAX_PAGES {
                return Err("Too many pages.".into());
            }
            document
                .pages
                .extend((0..pages).map(|page| (source as u32, page)));
            Ok(Value::number(document.pages.len() as i64))
        }
        Command::EditPage => {
            // EditPage while another page is open is not documented, and
            // is not measured: refused, not guessed.
            if document.open_page.is_some() {
                return Err(
                    "A page is open; OpenMaker opens another only after DynaPDF.EndPage.".into(),
                );
            }
            let page = number(1)
                .filter(|n| n.fract() == 0.0 && *n >= 1.0 && *n <= document.pages.len() as f64)
                .ok_or_else(|| "OpenMaker edits only a page the document has.".to_owned())?;
            document.open_page = Some(page as usize - 1);
            Ok(Value::text(OK))
        }
        Command::EndPage => {
            document
                .open_page
                .take()
                .ok_or_else(|| "No page is open.".to_owned())?;
            Ok(Value::text(OK))
        }
        Command::SetFont => {
            let family = text(1).ok_or_else(|| "Missing font name.".to_owned())?;
            let bold = match args.get(2) {
                Some(style) => bold(style, cx)?,
                None => false,
            };
            let size = number(3)
                .filter(|s| s.is_finite() && *s > 0.0 && *s <= 1000.0)
                .ok_or_else(|| "Missing or invalid font size.".to_owned())?;
            // Embed (4): OpenMaker embeds its face either way. CodePage (5):
            // the characters are drawn as Unicode.
            if let Some(page) = text(5) {
                if !matches!(
                    page.trim().to_ascii_lowercase().as_str(),
                    "" | "1252" | "cp1252" | "unicode"
                ) {
                    return Err(format!("Unsupported code page: {page}"));
                }
            }
            let face = Face::of_family(&family, bold)
                .ok_or_else(|| format!("OpenMaker has no font {family}."))?;
            document.font = Some((face, size));
            let handle = match document.fonts.iter().position(|f| *f == face) {
                Some(handle) => handle,
                None => {
                    document.fonts.push(face);
                    document.fonts.len() - 1
                }
            };
            Ok(Value::number(handle as i64))
        }
        Command::SetFillColor => {
            // RGB components from 0 to 1 [O, docs: "must be in the range from
            // 0 through 1"]; other counts or ranges are not drawn.
            let values: Vec<f64> = (1..args.len()).filter_map(number).collect();
            if values.len() != 3
                || args.len() != 4
                || values.iter().any(|v| !(0.0..=1.0).contains(v))
            {
                return Err(
                    "OpenMaker sets an RGB fill color of three components from 0 to 1.".into(),
                );
            }
            document.fill = [values[0], values[1], values[2]];
            Ok(Value::text(OK))
        }
        Command::SetLineWidth => {
            let width = number(1)
                .filter(|w| w.is_finite() && *w >= 0.0)
                .ok_or_else(|| "Invalid line width.".to_owned())?;
            document.line_width = width;
            Ok(Value::text(OK))
        }
        Command::SetTextRect => {
            document.text_rect = Some(rect(number(1), number(2), number(3), number(4))?);
            Ok(Value::text(OK))
        }
        Command::WriteFText | Command::WriteFTextEx => {
            let page = document
                .open_page
                .ok_or_else(|| "No page is open.".to_owned())?;
            // The worker draws nothing over a turned page: where the
            // plug-in puts text on one is not measured.
            let (source, source_page) = document.pages[page];
            if document.sources[source as usize].info.turned(source_page) {
                return Err("OpenMaker does not write on a turned page.".into());
            }
            let (face, size) = document.font.ok_or_else(|| "No font is set.".to_owned())?;
            let (rect, at) = if command == Command::WriteFTextEx {
                (rect(number(1), number(2), number(3), number(4))?, 5)
            } else {
                let rect = document.text_rect.ok_or_else(|| {
                    "OpenMaker writes formatted text only after DynaPDF.SetTextRect.".to_owned()
                })?;
                (rect, 1)
            };
            let align = text(at)
                .as_deref()
                .and_then(Align::of)
                .ok_or_else(|| "OpenMaker aligns text left, center or right.".to_owned())?;
            let body = text(at + 1).unwrap_or_default();
            let plain = number(at + 2).is_some_and(|p| p != 0.0);
            let style = Style {
                face,
                size,
                color: document.fill,
            };
            let spans = spans(&body, style, plain)?;
            let bytes: usize = spans.iter().map(|s| s.text.len()).sum();
            if document.blocks >= MAX_BLOCKS || document.text.saturating_add(bytes) > MAX_TEXT {
                return Err("The PDF document holds as much text as OpenMaker writes.".into());
            }
            let (left, top, width, height) = rect;
            let block = TextBlock {
                left,
                top,
                width,
                height,
                align,
                spans,
                ink_inside: false,
            };
            // Laid out now, so that text Save could not draw is refused
            // here and leaves the document as it was.
            host.plugin_check_pdf_text(&block)
                .map_err(|e| text_error(&e))?;
            let document = self::document(host, reference)?;
            document.blocks += 1;
            document.text += bytes;
            document.edits.entry(page).or_default().blocks.push(block);
            Ok(Value::text(OK))
        }
        Command::SetBBox => {
            let page = document
                .open_page
                .ok_or_else(|| "No page is open.".to_owned())?;
            let kind = match text(1)
                .unwrap_or_default()
                .trim()
                .to_ascii_lowercase()
                .as_str()
            {
                "media" => BoxKind::Media,
                "crop" => BoxKind::Crop,
                "trim" => BoxKind::Trim,
                "bleed" => BoxKind::Bleed,
                "art" => BoxKind::Art,
                _ => return Err("Invalid box name.".into()),
            };
            // Lower-left x and y, upper-right x and y [O, docs].
            let values = [number(2), number(3), number(4), number(5)];
            let [Some(x1), Some(y1), Some(x2), Some(y2)] = values else {
                return Err("Missing box coordinates.".into());
            };
            if [x1, y1, x2, y2].iter().any(|v| !v.is_finite()) || x2 <= x1 || y2 <= y1 {
                return Err("Invalid box coordinates.".into());
            }
            let boxes = &mut document.edits.entry(page).or_default().boxes;
            boxes.retain(|(k, _)| *k != kind);
            boxes.push((kind, [x1, y1, x2, y2]));
            Ok(Value::text(OK))
        }
        Command::GetPageWidth | Command::GetPageHeight => {
            // "the width of the currently open page"; "The page height
            // refers to the media box of a page" [O, docs and manual].
            // With no page open the plug-in answers its default page size,
            // which is not documented: refused.
            let page = document.open_page.ok_or_else(|| {
                "No page is open; OpenMaker does not know DynaPDF's default page size.".to_owned()
            })?;
            let [x0, y0, x1, y1] = boxes(document, page)?[BoxKind::Media as usize]
                .ok_or_else(|| "The page has no MediaBox.".to_owned())?;
            decimal(if command == Command::GetPageWidth {
                x1 - x0
            } else {
                y1 - y0
            })
        }
        Command::GetPageBBox => {
            // Page PageIndex "in range from 1 to DynaPDF.GetPageCount", not
            // the open page; Box "MediaBox, CropBox, ArtBox, BleedBox or
            // TrimBox", MediaBox by default [O, docs].
            let index = number(1)
                .filter(|n| n.fract() == 0.0 && *n >= 1.0 && *n <= document.pages.len() as f64)
                .ok_or_else(|| "Invalid page index.".to_owned())?;
            let name = text(2).unwrap_or_default();
            let kind = match name.trim().to_ascii_lowercase().as_str() {
                "" | "mediabox" => BoxKind::Media,
                "cropbox" => BoxKind::Crop,
                "trimbox" => BoxKind::Trim,
                "bleedbox" => BoxKind::Bleed,
                "artbox" => BoxKind::Art,
                _ => return Err("Invalid box name.".into()),
            };
            // A box the page does not have: the manual's engine reports it
            // unavailable; what the plug-in returns is not documented.
            let [x0, y0, x1, y1] = boxes(document, index as usize - 1)?[kind as usize]
                .ok_or_else(|| "The page has no such box.".to_owned())?;
            // "four numbers for left, top, right and bottom" [O, docs], a
            // line each with six decimals [I: as scripts observed in a
            // hosted solution read them].
            let line = |v: f64| format!("{:.6}", v + 0.0);
            Ok(Value::text(format!(
                "{}\r{}\r{}\r{}",
                line(x0),
                line(y1),
                line(x1),
                line(y0)
            )))
        }
        Command::SetFieldTextColor | Command::SetFieldBorderColor | Command::SetFieldBackColor => {
            // A packed RGB color, "Blue*65536+Green*256+Red", or for the
            // border and background NO_COLOR, transparent [O, docs].
            let transparent = command != Command::SetFieldTextColor;
            let color = number(1)
                .filter(|c| {
                    c.fract() == 0.0
                        && ((0.0..=16_777_215.0).contains(c) || transparent && *c == NO_COLOR)
                })
                .ok_or_else(|| "Invalid color.".to_owned())?;
            let slot = match command {
                Command::SetFieldTextColor => 0,
                Command::SetFieldBorderColor => 1,
                _ => 2,
            };
            document.field_colors[slot] = color;
            Ok(Value::text(OK))
        }
        Command::WebLink => {
            // "Inserts a web link onto the current open page"; PosX, PosY
            // the lower left corner in bottom-up coordinates; the URL "7
            // bit ASCII"; the border drawn "by using the current line
            // width, stroke color and line dash pattern" [O, docs and
            // manual]. OpenMaker sets no stroke color: black [I], solid.
            let page = field_page(document)?;
            let values = [number(1), number(2), number(3), number(4)];
            let [Some(x), Some(y), Some(width), Some(height)] = values else {
                return Err("Missing link rectangle.".into());
            };
            let rect = [x, y, x + width, y + height];
            if !rect_ok(rect) {
                return Err("Invalid link rectangle.".into());
            }
            let uri = text(5).unwrap_or_default();
            if !uri_ok(&uri) {
                return Err("OpenMaker links to a URL of printable 7-bit ASCII only.".into());
            }
            if document.line_width > MAX_COORD {
                return Err("Invalid line width for a link border.".into());
            }
            if document.links.len() >= MAX_LINKS {
                return Err("The PDF document holds as many links as OpenMaker writes.".into());
            }
            document.links.push(Link {
                page: page as u32,
                rect,
                border_width: document.line_width,
                color: [0.0; 3],
                uri,
            });
            Ok(Value::number(document.links.len() as i64 - 1))
        }
        Command::CreateJsAction => {
            // "DynaPDF does not check whether your script is valid" [O,
            // docs]; OpenMaker keeps it as text, and never runs it.
            let script = text(1).ok_or_else(|| "Missing script.".to_owned())?;
            if script.len() > MAX_SCRIPT
                || document.actions.len() >= MAX_ACTIONS
                || document.action_bytes.saturating_add(script.len()) > MAX_TEXT
            {
                return Err("The PDF document holds as many actions as OpenMaker writes.".into());
            }
            document.action_bytes += script.len();
            document.actions.push(script);
            Ok(Value::number(document.actions.len() as i64 - 1))
        }
        Command::AddActionToObj => {
            // ObjectType "Field", ObjectEvent "OnMouseUp", "the standard
            // event supported by all objects" [O, docs]: the action a
            // widget runs when clicked. Other objects and events are not
            // made here; DynaPDF allows more than one action on an object
            // but does not document how they chain, so a second is
            // refused.
            let kind = text(1).unwrap_or_default();
            let event = text(2).unwrap_or_default();
            if !kind.trim().eq_ignore_ascii_case("field")
                || !event.trim().eq_ignore_ascii_case("onmouseup")
            {
                return Err("OpenMaker attaches actions only to a field's OnMouseUp.".into());
            }
            let handle = |i: usize, len: usize| {
                number(i)
                    .filter(|n| n.fract() == 0.0 && *n >= 0.0 && *n < len as f64)
                    .map(|n| n as usize)
            };
            let action = handle(3, document.actions.len())
                .ok_or_else(|| "Invalid action handle.".to_owned())?;
            let field = handle(4, document.fields.len())
                .ok_or_else(|| "Invalid field handle.".to_owned())?;
            let field = &mut document.fields[field];
            if field.action.is_some() {
                return Err("OpenMaker attaches one action to a field's OnMouseUp.".into());
            }
            field.action = Some(action as u32);
            Ok(Value::text(OK))
        }
        Command::CreateTextField
        | Command::CreateCheckBox
        | Command::CreateSigField
        | Command::CreateButton => {
            // The arguments after Name: Parent; Multiline; MaxLen for a
            // text field, ExpValue; Checked; Parent for a check box, Parent
            // for a signature field, Caption; Parent for a button; then
            // PosX; PosY; Width; Height [O, docs].
            let page = field_page(document)?;
            // A period would make the field a child of another, which is
            // not made here (PDF 1.7, 12.7.3.2).
            let name = text(1).unwrap_or_default();
            if !name_ok(&name) {
                return Err("Invalid field name.".into());
            }
            if document.fields.len() >= MAX_FIELDS {
                return Err("The PDF document holds as many fields as OpenMaker writes.".into());
            }
            let flag = |i: usize| match number(i) {
                Some(n) if n == 0.0 || n == 1.0 => Ok(n == 1.0),
                _ => Err("Invalid argument.".to_owned()),
            };
            let (parent, at, kind) = match command {
                Command::CreateTextField => {
                    let multiline = flag(3)?;
                    let max_len = number(4)
                        .filter(|n| n.fract() == 0.0 && (0.0..=f64::from(u32::MAX)).contains(n))
                        .ok_or_else(|| "Invalid maximum length.".to_owned())?;
                    let kind = FieldKind::Text {
                        bold: matches!(document.font, Some((Face::SansBold, _))),
                        multiline,
                        max_len: max_len as u32,
                    };
                    (number(2), 5, kind)
                }
                Command::CreateCheckBox => {
                    // "The export value must not be an empty string" [O,
                    // manual]; nor `Off`, the off state's name, nor hold
                    // NUL, which no PDF name holds.
                    let export = text(2).unwrap_or_default();
                    if !export_ok(&export) {
                        return Err("Invalid export value.".into());
                    }
                    let kind = FieldKind::CheckBox {
                        export,
                        checked: flag(3)?,
                    };
                    (number(4), 5, kind)
                }
                Command::CreateButton => {
                    // "Buttons requires a font" [O, manual]; the size it
                    // draws its caption at without one, or at 1, is not
                    // documented.
                    if !document.font.is_some_and(|(_, size)| size != 1.0) {
                        return Err(
                            "OpenMaker creates a button only after DynaPDF.SetFont, at a size other than 1."
                                .into(),
                        );
                    }
                    let caption = text(2).unwrap_or_default();
                    if caption.len() > MAX_CAPTION {
                        return Err("The caption is too long.".into());
                    }
                    let kind = FieldKind::Button {
                        bold: matches!(document.font, Some((Face::SansBold, _))),
                        caption,
                        caption_page: None,
                    };
                    (number(3), 4, kind)
                }
                _ => (number(2), 3, FieldKind::Signature),
            };
            // A parent group or radio button is not made here.
            if parent != Some(-1.0) {
                return Err("OpenMaker creates only fields without a parent (-1).".into());
            }
            let values = [number(at), number(at + 1), number(at + 2), number(at + 3)];
            let [Some(x), Some(y), Some(width), Some(height)] = values else {
                return Err("Missing field rectangle.".into());
            };
            let rect = [x, y, x + width, y + height];
            if !rect_ok(rect) {
                return Err("Invalid field rectangle.".into());
            }
            if document.line_width > MAX_COORD {
                return Err("Invalid line width for a field border.".into());
            }
            // The size: a text field's is automatic (0) unless it is
            // multiline, or the active font's is other than 1; a check
            // box's unless the active font's is other than 1 [O, manual].
            let active = document.font.map(|(_, size)| size).filter(|s| *s != 1.0);
            let size =
                match &kind {
                    FieldKind::Text {
                        multiline: true, ..
                    } => match document.font {
                        Some((_, size)) => size,
                        // The manual names no size for a multiline field
                        // without a font.
                        None => return Err(
                            "OpenMaker creates a multiline text field only after DynaPDF.SetFont."
                                .into(),
                        ),
                    },
                    FieldKind::Text { .. } | FieldKind::CheckBox { .. } => active.unwrap_or(0.0),
                    FieldKind::Signature => 0.0,
                    FieldKind::Button { .. } => document.font.map_or(0.0, |(_, size)| size),
                };
            // An automatic check needs room inside the border to show.
            if matches!(kind, FieldKind::CheckBox { .. })
                && !check_fits(rect, document.line_width, size)
            {
                return Err("The check box has no room inside its border for its check.".into());
            }
            let [text_color, border, back] = document.field_colors.map(field_color);
            // The point is the lower left corner in bottom-up coordinates,
            // and the rectangle includes the border [O, docs].
            let field = Field {
                page: page as u32,
                name,
                kind,
                rect,
                border_width: document.line_width,
                text: text_color.unwrap_or([0.0; 3]),
                border,
                back,
                size,
                action: None,
            };
            // A field of a name already made joins its group [O, manual:
            // same name and type], or is refused (`group_refusal`).
            let members: Vec<&Field> = document
                .fields
                .iter()
                .filter(|f| f.name == field.name)
                .chain([&field])
                .collect();
            if let Some(reason) = group_refusal(&members) {
                return Err(reason.into());
            }
            // A button's caption, "centred horizontally and vertically" and
            // "drawn on multiple lines if it does not fit into a single
            // line" [O, manual], inside its border, in the field text
            // color: laid out now, so that one Save could not draw is
            // refused here.
            let caption = match (&field.kind, document.font) {
                (FieldKind::Button { caption, .. }, Some((face, size))) if !caption.is_empty() => {
                    let lw = field.border_width;
                    let (w, h) = (width - 2.0 * lw, height - 2.0 * lw);
                    if w <= 0.0 || h <= 0.0 {
                        return Err(
                            "The button has no room inside its border for its caption.".into()
                        );
                    }
                    let style = Style {
                        face,
                        size,
                        color: field.text,
                    };
                    Some(TextBlock {
                        left: 0.0,
                        top: 0.0,
                        width: w,
                        height: Some(h),
                        align: Align::Center,
                        spans: spans(caption, style, true)?,
                        ink_inside: true,
                    })
                }
                _ => None,
            };
            if let Some(block) = &caption {
                host.plugin_check_pdf_text(block).map_err(|e| match e {
                    HostError::Error(code) if code.0 == 3 || code.0 == 4 => {
                        "The caption does not fit its button.".to_owned()
                    }
                    other => text_error(&other),
                })?;
            }
            let document = self::document(host, reference)?;
            if let Some(block) = caption {
                document.captions.push((document.fields.len(), block));
            }
            document.fields.push(field);
            Ok(Value::number(document.fields.len() as i64 - 1))
        }
        _ => Err("Unsupported command.".into()),
    }
}

/// Validate imported annotations before any state change. Pending text must
/// become page content before new highlights are baked, while widgets and
/// their captions/actions remain editable and are written only once at Save.
fn flatten<H: PluginHost + ?Sized>(
    host: &mut H,
    reference: i64,
    flag: &str,
) -> Result<Value, String> {
    let document = document(host, reference)?;
    let flags = match flag.trim().to_ascii_lowercase().as_str() {
        "" | "none" => 0,
        "useviewstate" => 1,
        "markupannots" => 2,
        "visiblemarkupannots" => 3,
        other => other
            .parse::<f64>()
            .ok()
            .filter(|n| n.fract() == 0.0 && (0.0..=3.0).contains(n))
            .map(|n| n as u8)
            .ok_or_else(|| {
                "OpenMaker flattens annotations with flags 0 through 3 only.".to_owned()
            })?,
    };
    let mut selected: BTreeMap<u32, Vec<u32>> = BTreeMap::new();
    for (source, page) in &document.pages {
        selected.entry(*source).or_default().push(*page);
    }
    for (source, mut pages) in selected {
        pages.sort_unstable();
        pages.dedup();
        match pdf_worker::annotation_free(&document.sources[source as usize].data, &pages) {
            Ok(Some(true)) => {}
            Ok(Some(false)) => return Err("OpenMaker cannot flatten imported annotations.".into()),
            Ok(None) => return Err("The PDF annotations could not be checked.".into()),
            Err(_) => return Err("OpenMaker cannot read PDF data here.".into()),
        }
    }
    if document.highlights.iter().any(|h| !h.flattened)
        && document.edits.values().any(|edit| !edit.blocks.is_empty())
    {
        compose_before_flatten(host, reference)?;
    }
    let document = self::document(host, reference)?;
    // All validation precedes mutation. Generated highlights have
    // explicit printable/visible appearances. Links have no AP;
    // default flattening deletes them, markup-only keeps them [O].
    for highlight in &mut document.highlights {
        highlight.flattened = true;
    }
    if flags & 2 == 0 {
        document.links.clear();
    }
    Ok(Value::number(document.links.len() as i64))
}

/// Freeze content at the flatten boundary. Original import sources stay
/// available; the snapshot occupies one bounded source slot. Render, compose
/// and read its geometry before committing any change to the document.
fn compose_before_flatten<H: PluginHost + ?Sized>(
    host: &mut H,
    reference: i64,
) -> Result<(), String> {
    let document = self::document(host, reference)?;
    if document.sources.len() >= MAX_SOURCES - 1 {
        return Err("OpenMaker has no source slot for flattened page content.".into());
    }
    let mut sources: Vec<Arc<[u8]>> = document
        .sources
        .iter()
        .map(|s| Arc::clone(&s.data))
        .collect();
    let mut plan: Vec<Planned> = document.pages.iter().copied().map(Planned::from).collect();
    let edits = document.edits.clone();
    let baked: Vec<_> = document
        .highlights
        .iter()
        .filter(|h| h.flattened)
        .cloned()
        .collect();
    let mut written: Vec<&[TextBlock]> = Vec::new();
    for (page, edit) in &edits {
        plan[*page].boxes = edit.boxes.clone();
        if !edit.blocks.is_empty() {
            plan[*page].overlay = Some((sources.len() as u32, written.len() as u32));
            written.push(&edit.blocks);
        }
    }
    let drawn = host
        .plugin_render_pdf_text(&written)
        .map_err(|e| text_error(&e))?;
    sources.push(drawn.into());
    let source_bytes: Vec<&[u8]> = sources.iter().map(|s| &s[..]).collect();
    let data = match pdf_worker::assemble_highlighted(&source_bytes, &plan, &[], &[], &[], &baked) {
        Ok(Some(data)) => data,
        Ok(None) => return Err("The PDF page content could not be composed for flattening.".into()),
        Err(_) => return Err("OpenMaker cannot write PDF data here.".into()),
    };
    let document = self::document(host, reference)?;
    if data.len() as u64 > MAX_FILE
        || document.source_bytes().saturating_add(data.len()) > MAX_SOURCE_BYTES
    {
        return Err("The PDF documents are too large.".into());
    }
    let info = match pdf_worker::info(&data) {
        Ok(Some(info)) if info.optional_content_kept && info.pages as usize == plan.len() => info,
        Ok(_) => return Err("The flattened page content could not be read.".into()),
        Err(_) => return Err("OpenMaker cannot read PDF data here.".into()),
    };
    let source = document.sources.len() as u32;
    document.sources.push(Source {
        data: data.into(),
        info,
    });
    for (page, source_page) in document.pages.iter_mut().enumerate() {
        *source_page = (source, page as u32);
    }
    document.edits.clear();
    document.highlights.retain(|h| !h.flattened);
    // Text/highlight counters remain cumulative: snapshots must not let a
    // session bypass the document's allocation limits.
    Ok(())
}

/// Search the same page text Save would write, without committing a file
/// or replacing the imported source. Validate arguments before rendering;
/// all glyph locations must be available before adding any annotations.
fn search<H: PluginHost + ?Sized>(
    host: &mut H,
    cx: &Ctx,
    reference: i64,
    command: Command,
    args: &[Value],
) -> Result<Value, String> {
    field_page(document(host, reference)?)?;
    let pattern = args
        .get(1)
        .map(|v| v.to_text(cx).into_string())
        .unwrap_or_default();
    if !pdf_worker::pattern_ok(&pattern) {
        return Err(if command == Command::HighlightPattern {
            "Invalid highlight pattern.".into()
        } else {
            "OpenMaker searches for a nonempty single-line pattern of at most 4096 bytes.".into()
        });
    }
    let number = |i| args.get(i).and_then(|v: &Value| v.to_number(cx).to_f64());
    let packed = number(2).unwrap_or(65535.0);
    let insensitive = number(3).unwrap_or(0.0);
    if command == Command::HighlightPattern {
        if packed.fract() != 0.0 || !(0.0..=16777215.0).contains(&packed) {
            return Err("Invalid highlight color.".into());
        }
        if insensitive != 0.0 && insensitive != 1.0 {
            return Err("Invalid case-insensitive flag.".into());
        }
    }
    let (page, data, source_page) = search_page(host, reference)?;
    if command == Command::FindPattern {
        return match pdf_worker::find(&data, source_page, &pattern) {
            Ok(Some(count)) => Ok(Value::number(i64::from(count))),
            Ok(None) => Err("The PDF page could not be searched.".into()),
            Err(_) => Err("OpenMaker cannot read PDF data here.".into()),
        };
    }
    let rects =
        match pdf_worker::highlight_positions(&data, source_page, &pattern, insensitive == 1.0) {
            Ok(Some(rects)) => rects,
            Ok(None) => {
                return Err(
                    "OpenMaker cannot locate every match as horizontal outline text.".into(),
                )
            }
            Err(_) => return Err("OpenMaker cannot read PDF data here.".into()),
        };
    let document = document(host, reference)?;
    if document.highlight_count + rects.len() > pdf_worker::MAX_HIGHLIGHTS {
        return Err("Too many highlight annotations.".into());
    }
    let count = rects.len();
    document.highlight_count += count;
    document
        .highlights
        .extend(rects.into_iter().map(|rect| pdf_worker::Highlight {
            page: page as u32,
            rect,
            color: unpack(packed as u32),
            flattened: false,
        }));
    Ok(Value::number(count as i64))
}

fn search_page<H: PluginHost + ?Sized>(
    host: &mut H,
    reference: i64,
) -> Result<(usize, Arc<[u8]>, u32), String> {
    let (page, source, source_page, edit, baked) = {
        let document = document(host, reference)?;
        let page = field_page(document)?;
        let (source, source_page) = document.pages[page];
        let baked = document
            .highlights
            .iter()
            .filter(|h| h.flattened && h.page == page as u32)
            .cloned()
            .map(|mut h| {
                h.page = 0;
                h
            })
            .collect::<Vec<_>>();
        (
            page,
            Arc::clone(&document.sources[source as usize].data),
            source_page,
            document.edits.get(&page).cloned().unwrap_or_default(),
            baked,
        )
    };
    if edit.blocks.is_empty() {
        return Ok((page, source, source_page));
    }
    let drawn = host
        .plugin_render_pdf_text(&[&edit.blocks])
        .map_err(|e| text_error(&e))?;
    let plan = [Planned {
        source: 0,
        page: source_page,
        overlay: Some((1, 0)),
        boxes: edit.boxes,
    }];
    let data =
        match pdf_worker::assemble_highlighted(&[&source, &drawn], &plan, &[], &[], &[], &baked) {
            Ok(Some(data)) => data,
            Ok(None) => return Err("The PDF page could not be composed for search.".into()),
            Err(_) => return Err("OpenMaker cannot read PDF data here.".into()),
        };
    Ok((page, data.into(), 0))
}

/// The open page a form field is created on: refused for none, and for a
/// turned page, where the plug-in puts a field is not documented.
fn field_page(document: &Document) -> Result<usize, String> {
    let page = document
        .open_page
        .ok_or_else(|| "No page is open.".to_owned())?;
    let (source, source_page) = document.pages[page];
    if document.sources[source as usize].info.turned(source_page) {
        return Err("OpenMaker does not write on a turned page.".into());
    }
    Ok(page)
}

/// A packed field color as red, green and blue from 0 to 1; `None` for
/// `NO_COLOR`.
fn field_color(color: f64) -> Option<[f64; 3]> {
    (color != NO_COLOR).then(|| unpack(color as u32))
}

/// The boxes of the document's page `page` (from 0): its source page's, as
/// `DynaPDF.SetBBox` changed them. Refused for a turned page, where the
/// plug-in's sizes are not documented, and a page past those the PDF
/// worker describes.
fn boxes(document: &Document, page: usize) -> Result<[Option<[f64; 4]>; 5], String> {
    let (source, source_page) = document.pages[page];
    let geometry: &PageGeometry = document.sources[source as usize]
        .info
        .geometry(source_page)
        .ok_or_else(|| {
            format!("OpenMaker describes only the first {INFO_PAGES} pages of a PDF.")
        })?;
    if geometry.turned {
        return Err("OpenMaker does not measure a turned page.".into());
    }
    let mut boxes = geometry.boxes;
    for (kind, rect) in document.edits.get(&page).map_or(&[][..], |e| &e.boxes) {
        boxes[*kind as usize] = Some(*rect);
    }
    Ok(boxes)
}

/// A number of points as a calculation number, in its shortest decimal
/// form (`595.276`, not the binary double's digits); an error for a size
/// past a double's range.
fn decimal(value: f64) -> Result<Value, String> {
    value
        .is_finite()
        .then(|| Number::parse_pure_text(&format!("{}", value + 0.0), '.', ','))
        .flatten()
        .map(Value::Number)
        .ok_or_else(|| "The page is too large to measure.".to_owned())
}

/// The `[MBS]` error for text the host refused to lay out or draw.
fn text_error(error: &HostError) -> String {
    match error {
        HostError::Error(code) if code.0 == 3 => {
            "The text does not fit its rectangle; OpenMaker does not break pages.".to_owned()
        }
        HostError::Error(code) if code.0 == 4 => {
            "A line of the text is wider than its rectangle.".to_owned()
        }
        HostError::Error(code) if code.0 == 800 => {
            "OpenMaker's font has no glyph for a character of the text.".to_owned()
        }
        HostError::Unsupported => "OpenMaker cannot draw PDF text here.".to_owned(),
        _ => "The text could not be drawn.".to_owned(),
    }
}

/// A text rectangle: left, top, width, and height (`None` for -1, no
/// limit) [I: the top edge, OpenMaker's reading; the documentation names it
/// the "Y-Coordinate of output rectangle"].
fn rect(
    left: Option<f64>,
    top: Option<f64>,
    width: Option<f64>,
    height: Option<f64>,
) -> Result<(f64, f64, f64, Option<f64>), String> {
    let (Some(left), Some(top), Some(width), Some(height)) = (left, top, width, height) else {
        return Err("Missing text rectangle.".into());
    };
    if ![left, top, width, height].iter().all(|v| v.is_finite()) || width <= 0.0 {
        return Err("Invalid text rectangle.".into());
    }
    let height = if height == -1.0 {
        None
    } else if height > 0.0 {
        Some(height)
    } else {
        return Err("Invalid text rectangle.".into());
    };
    Ok((left, top, width, height))
}

/// Whether SetFont's Style asks for bold: 0 or 2, or the words `regular`,
/// `normal` and `bold`. Italic, underline and strikeout are not drawn.
fn bold(style: &Value, cx: &Ctx) -> Result<bool, String> {
    let refused = || "OpenMaker draws regular and bold text only.".to_owned();
    if style.value_type() == ValueType::Number {
        let flags = style.to_number(cx).to_f64();
        return if flags == Some(0.0) {
            Ok(false)
        } else if flags == Some(2.0) {
            Ok(true)
        } else {
            Err(refused())
        };
    }
    match style
        .to_text(cx)
        .into_string()
        .trim()
        .to_ascii_lowercase()
        .as_str()
    {
        "" | "0" | "regular" | "normal" => Ok(false),
        "2" | "bold" => Ok(true),
        _ => Err(refused()),
    }
}

fn documents<H: PluginHost + ?Sized>(host: &mut H) -> Result<&mut Documents, String> {
    host.plugin_state()
        .map(|state| &mut state.pdf)
        .ok_or_else(|| "No plug-in state.".to_owned())
}

fn document<H: PluginHost + ?Sized>(host: &mut H, reference: i64) -> Result<&mut Document, String> {
    documents(host)?
        .open
        .get_mut(&reference)
        .ok_or_else(|| "Invalid PDF reference.".to_owned())
}

/// The source the imports read.
fn import(document: &Document) -> Result<usize, String> {
    document
        .import
        .ok_or_else(|| "No PDF is open for import.".to_owned())
}

/// Scale factors other than 1 change what is imported; OpenMaker does not
/// scale, and refuses rather than ignore them.
fn unscaled(x: Option<f64>, y: Option<f64>) -> Result<(), String> {
    if x.is_some_and(|x| x != 1.0) || y.is_some_and(|y| y != 1.0) {
        return Err("OpenMaker imports pages only at scale 1.".into());
    }
    Ok(())
}

fn file_error(error: &HostError, verb: &str) -> String {
    match error {
        HostError::Error(e) if *e == code::FILE_MISSING => "File not found.".into(),
        HostError::Error(e) if *e == code::INVALID_PATH => "Invalid path.".into(),
        HostError::Error(e) if *e == code::IN_USE => "The file is in use.".into(),
        HostError::Error(e) if *e == code::MEMORY => "The file is too large.".into(),
        _ => format!("Failed to {verb} the file."),
    }
}
