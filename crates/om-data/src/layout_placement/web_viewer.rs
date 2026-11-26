//! Placing a web viewer on a layout's body (#896): a web viewer with a
//! custom web address and Web Viewer Setup's options, as FileMaker Pro
//! 26.1.1 stores one drawn with the Web Viewer tool (layout-model.md 9.6,
//! 10.19).
//!
//! The viewer is the object Pro wrote (`encode::web_viewer_object`), on top
//! of the body's objects, with:
//! - its attribute record: the web address as a stored calculation under
//!   `00 00` (context `01 00`, the formatted and the Polish streams) and a
//!   data format `0d` of `00 0a`; the host adds its UUID `d8`. Pro wrote no
//!   name `10` and no `d9` for its viewers;
//! - its entries in the body's list of objects, the object index `0a` and
//!   the tab order `15` (after the stored entries);
//! - a field reference `01, <occurrence>, <field>, <viewer>` of value `0d`
//!   for each field the address reads;
//! - the last object key issued `05, 00` raised to its key.
//!
//! The options are the measured ones only: what Pro stores for each other
//! combination is unmeasured, since `0x0080` came with some of them and not
//! others (layout-model.md 9.6).

use super::*;

/// Web Viewer Setup's six options.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebViewerSetup {
    /// "Allow interaction with web viewer content".
    pub interaction: bool,
    /// "Display content in Find mode".
    pub find_mode: bool,
    /// "Display progress bar".
    pub progress_bar: bool,
    /// "Display status messages".
    pub status_messages: bool,
    /// "Automatically encode URL".
    pub encode_url: bool,
    /// "Allow JavaScript to perform FileMaker scripts".
    pub javascript: bool,
}

impl WebViewerSetup {
    /// The options FileMaker Pro 26.1.1's Web Viewer Setup starts a new
    /// viewer with (#896): interaction, progress bar, status messages and
    /// encoding on; Find mode and JavaScript scripts off.
    pub const DEFAULT: Self = Self {
        interaction: true,
        find_mode: false,
        progress_bar: true,
        status_messages: true,
        encode_url: true,
        javascript: false,
    };

    /// The `externalObj.flags` FileMaker Pro stored for these options with
    /// a custom web address, when they are a combination it was seen to
    /// store (#896): the defaults `0x800d` (W2); interaction, Find mode and
    /// JavaScript scripts on, the rest off, `0x80e3` (W1); and that with the
    /// progress bar on, `0x80e7` (W3). Each has `0x8000`, which marks a
    /// custom address (inferred: a template's viewer lacked it), and the
    /// last two `0x0080`, which no setter writes. `None` for the others,
    /// whose `0x0080` is unknown.
    pub fn flags(&self) -> Option<u32> {
        let setters = u32::from(self.interaction)
            | u32::from(self.find_mode) << 1
            | u32::from(self.progress_bar) << 2
            | u32::from(self.status_messages) << 3
            | u32::from(!self.encode_url) << 5
            | u32::from(self.javascript) << 6;
        // The custom address bit, and 0x0080 where Pro set it.
        match setters {
            0x0d => Some(0x800d),
            0x63 => Some(0x80e3),
            0x67 => Some(0x80e7),
            _ => None,
        }
    }

    /// The options `flags` hold, when they are a combination
    /// [`Self::flags`] writes.
    pub fn from_flags(flags: u32) -> Option<Self> {
        let setup = Self {
            interaction: flags & 0x1 != 0,
            find_mode: flags & 0x2 != 0,
            progress_bar: flags & 0x4 != 0,
            status_messages: flags & 0x8 != 0,
            encode_url: flags & 0x20 == 0,
            javascript: flags & 0x40 != 0,
        };
        (setup.flags() == Some(flags)).then_some(setup)
    }
}

/// The viewer's web address under its attribute record:
/// `[4, 5, L, 05, <viewer>, 00 00]`.
const WEB_ADDRESS: [u8; 2] = [0, 0];
/// The formula node below it.
const FORMULA: u8 = 0x05;
/// The data format `0d` Pro wrote for its viewers.
const VIEWER_FORMAT: [u8; 2] = [0x00, 0x0a];
/// The value of a field reference a viewer's address makes.
const VIEWER_REFERENCE: u8 = 0x0d;

/// The most bytes of a web address's formatted and Polish streams together
/// (OpenMaker's bound, as the wire's: a value above 1000 bytes is stored in
/// chunks).
pub const MAX_ADDRESS_BYTES: usize = om_protocol_bound::MAX_WEB_ADDRESS_BYTES;

/// The wire's bound, kept equal to the protocol's
/// (`om_protocol::authoring::MAX_WEB_ADDRESS_BYTES`; om-data does not
/// depend on om-protocol).
mod om_protocol_bound {
    pub const MAX_WEB_ADDRESS_BYTES: usize = 64 * 1024;
}

/// A field reference's index keys: the occurrence's and the field's key
/// segments.
type FieldKeys = (Vec<u8>, Vec<u8>);

/// A web viewer to place on a layout's body.
#[derive(Clone, Debug, PartialEq)]
pub struct WebViewerPlacement {
    /// Its frame in layout coordinates (points).
    pub frame: Rect,
    /// `externalObj.flags`: one of [`WebViewerSetup::flags`].
    pub flags: u32,
    /// The custom web address: a calculation's formatted stream (empty
    /// for none) and its Polish stream, compiled in the layout's context.
    pub formatted: Vec<u8>,
    pub polish: Vec<u8>,
}

/// A web viewer placement asked of the open file's provider
/// ([`crate::DataProvider::place_layout_web_viewer`]) on layout `layout`,
/// if it is still at revision `expected`.
#[derive(Clone, Debug, PartialEq)]
pub struct LayoutWebViewerPlacement {
    pub layout: u32,
    pub viewer: WebViewerPlacement,
    pub expected: Revision,
}

/// A web viewer the host placed, or found already placed by an earlier
/// request whose answer was lost (`changed` false).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedLayoutWebViewer {
    pub changed: bool,
    pub viewer: u64,
    /// The layout's revision after the placement.
    pub revision: Revision,
}

/// A web viewer planned into a layout.
#[derive(Clone, Debug, PartialEq)]
pub struct PlannedWebViewer {
    /// The viewer's key: the next after every key the layout uses.
    pub viewer: u64,
    /// The part it is in, and its place in the root stack (on top).
    pub part: u64,
    pub stacking: usize,
    /// The stored objects it overlaps (they stay beneath it).
    pub overlaps: Vec<u64>,
    /// The layout member `[4, 5, L]` with the viewer, without its UUID or
    /// the stamps.
    pub member: OwnedNode,
    /// What changed in the member, as [`Planned::changes`].
    pub changes: Vec<(String, String)>,
}

/// Compile a web address typed as calculation text, as Specify
/// Calculation does for Web Viewer Setup: unqualified field names are the
/// layout's occurrence's (`occurrence`), with the file's decimal
/// separator. It is stored with no context of its own (`01 00`), as Pro
/// stored its viewers' addresses (layout-model.md 9.6). Returns the
/// formatted and Polish streams, or FileMaker's message for the error.
pub fn compile_address(
    text: &str,
    schema: &crate::host::CalcSchema,
    occurrence: Option<u64>,
) -> Result<(Vec<u8>, Vec<u8>), Refusal> {
    let ctx = om_formula::Context {
        occurrence,
        decimal_comma: schema.locale.decimal_separator == ',',
        ..om_formula::Context::new(schema.names())
    };
    let compiled = om_formula::compile(text, &ctx).map_err(|error| {
        let code = i32::from(error.code.number());
        Refusal::Invalid(format!(
            "The web address is not a calculation FileMaker accepts: {} (error {code}).",
            crate::runtime::ErrorCode(code)
                .message()
                .unwrap_or("there is an error in this calculation")
        ))
    })?;
    Ok((
        om_formula::formatted::encode(&compiled.tokens),
        om_formula::polish::encode(&compiled.expr),
    ))
}

/// The address's expression: its formatted stream (the text Web Viewer
/// Setup shows) compiled again must give its Polish stream (what Browse
/// evaluates), as FileMaker Pro's did (layout-model.md 10.19), so the two
/// cannot disagree. Fields are keys in the formatted stream; an address
/// that needs the file's names for more (a custom or plug-in function) is
/// refused.
fn address(viewer: &WebViewerPlacement) -> Result<om_formula::Expr, Refusal> {
    let invalid = |why: String| Refusal::Invalid(format!("The web address {why}."));
    if viewer.polish.is_empty() || viewer.formatted.is_empty() {
        return Err(invalid("is empty, or has no text".into()));
    }
    if viewer.polish.len() + viewer.formatted.len() > MAX_ADDRESS_BYTES {
        return Err(invalid(format!(
            "takes more than {MAX_ADDRESS_BYTES} bytes"
        )));
    }
    let tokens = om_formula::formatted::decode(&viewer.formatted)
        .map_err(|e| invalid(format!("text does not read ({e:?})")))?;
    let names = om_formula::NoNames;
    let expr =
        om_formula::compile_tokens(&tokens, &om_formula::Context::new(&names)).map_err(|_| {
            Refusal::Unsupported(
                "The web address's text does not compile on its own; this version places \
                 addresses that use the file's fields and FileMaker's functions only."
                    .into(),
            )
        })?;
    if om_formula::polish::encode(&expr) != viewer.polish {
        return Err(invalid(
            "text and the calculation evaluated are not the same calculation".into(),
        ));
    }
    Ok(expr)
}

/// The fields `expr` reads, as the field reference index keys them: each
/// occurrence's and field's key segments, once.
fn referenced_fields(expr: &om_formula::Expr) -> Result<Vec<FieldKeys>, Refusal> {
    let mut out = Vec::new();
    for field in expr.field_references() {
        let (Some(occurrence), Some(id)) = (field.occurrence.id(), field.field.id()) else {
            return Err(Refusal::Invalid(
                "The web address names a field by a key OpenMaker does not read.".into(),
            ));
        };
        let entry = (uint_segment(occurrence), uint_segment(id));
        if !out.contains(&entry) {
            out.push(entry);
        }
    }
    Ok(out)
}

/// Plan `viewer` into `member`, a layout member `[4, 5, L]` as stored.
pub fn plan_web_viewer(
    member: &OwnedNode,
    viewer: &WebViewerPlacement,
) -> Result<PlannedWebViewer, Refusal> {
    let unsupported = |why: String| Refusal::Unsupported(why);
    let def = placeable(member)?;
    check_frame(&viewer.frame, def.base_width, "web viewer")?;
    if WebViewerSetup::from_flags(viewer.flags).is_none() {
        return Err(unsupported(format!(
            "OpenMaker does not write web viewer options {:#06x} yet: what FileMaker stores for \
             that combination is not measured.",
            viewer.flags
        )));
    }
    let expr = address(viewer)?;
    let fields = referenced_fields(&expr)?;
    let frame = &viewer.frame;
    let OnBody {
        body,
        stored,
        objects,
        overlaps,
    } = on_body(member, &[("web viewer", frame)])?;
    let issued = last_issued(member)?;
    let key = first_new_key(member, &objects)?;

    let unreadable_list = || {
        unsupported(
            "The body part has no list of its objects that OpenMaker reads; every part FileMaker \
             saves has one, and OpenMaker does not place objects without it."
                .into(),
        )
    };
    let body_node = part_node(member, body.id).ok_or_else(unreadable_list)?;
    let stored_list_node = body_node
        .child(&[PART_OBJECTS])
        .ok_or_else(unreadable_list)?;
    let stored_list = stored_list_node
        .value
        .as_deref()
        .ok_or_else(unreadable_list)?;
    let listed = part_list(stored_list).ok_or_else(unreadable_list)?;
    let mut part_list_value = Vec::with_capacity(stored_list.len() + 4);
    part_list_value.extend(
        u32::try_from(listed.len() + 1)
            .map_err(|_| unreadable_list())?
            .to_be_bytes(),
    );
    part_list_value.extend_from_slice(&stored_list[4..]);
    part_list_value.extend(part_list_entry(key));
    if part_list_value.len() > MAX_INLINE_VALUE {
        return Err(unsupported(
            "The body part lists too many objects for OpenMaker to add another.".into(),
        ));
    }

    let sequence = tree_size(&objects)?;
    if largest_indexed(member).is_some_and(|largest| largest >= sequence) {
        return Err(unsupported(
            "The layout's object index does not match its objects; OpenMaker does not place \
             objects on it."
                .into(),
        ));
    }
    let (index_key, index_value) = object_index_entry(frame.top, frame.left, sequence, key)?;
    let position = next_tab_position(member)?;

    let store = encode::append_to_store(
        &stored,
        &[key],
        &[encode::web_viewer_object(key, frame, viewer.flags)],
    );
    let mut next = member.clone();
    let mut changes = vec![(
        "[4, 5, L, 07]".to_owned(),
        format!(
            "the object store: web viewer {key} added on top of the root stack ({} bytes \
             appended to the {} stored, which are kept)",
            store.len() - stored.len(),
            stored.len()
        ),
    )];
    next.set_child(stored_value(&[OBJECTS], &store));

    let formula = om_scripts::Calculation::new(
        (!viewer.formatted.is_empty()).then(|| viewer.formatted.clone()),
        viewer.polish.clone(),
    )
    .encode(&[FORMULA]);
    let mut record = OwnedNode::list(serial::encode(key), Vec::new());
    record.set_child(OwnedNode::list(WEB_ADDRESS.to_vec(), vec![formula]));
    record.set_child(OwnedNode::leaf(vec![FORMAT], VIEWER_FORMAT.to_vec()));
    changes.push((
        format!("[4, 5, L, 05, {}]", hex(&record.segment)),
        format!(
            "web viewer {key}'s attributes: the web address (00 00), the data format 00 0a, and \
             the UUID the host makes"
        ),
    ));
    next.put(&[&[ATTRIBUTES], &record.segment.clone()], record);

    let object_segment = serial::encode(key);
    for (occurrence, field) in &fields {
        changes.push((
            format!(
                "[4, 5, L, 01, {}, {}, {}]",
                hex(occurrence),
                hex(field),
                hex(&object_segment)
            ),
            format!("the field reference index: the web address reads this field ({VIEWER_REFERENCE:02x})"),
        ));
        next.put(
            &[&[FIELD_INDEX], occurrence, field, &object_segment],
            OwnedNode::leaf(Vec::new(), vec![VIEWER_REFERENCE]),
        );
    }

    changes.push((
        format!("[4, 5, L, 0a, {}]", hex(&index_key)),
        format!(
            "the object index: web viewer {key} at top {}, left {}, pre-order index {sequence}",
            frame.top, frame.left
        ),
    ));
    next.put(
        &[&[OBJECT_INDEX], &index_key],
        OwnedNode::leaf(Vec::new(), index_value.clone()),
    );
    changes.push((
        format!("[4, 5, L, 15, {}]", hex(&position)),
        format!("the tab order: web viewer {key} after the existing entries"),
    ));
    next.put(
        &[&[TAB_ORDER], &position],
        OwnedNode::leaf(Vec::new(), index_value),
    );

    changes.push((
        "[4, 5, L, 05, 00]".to_owned(),
        format!(
            "the last object key issued: {key} (was {})",
            issued.map_or_else(|| "none".into(), |k| k.to_string())
        ),
    ));
    let mut counter = member
        .at(&[&[ATTRIBUTES], &[LAST_KEY]])
        .cloned()
        .unwrap_or_else(|| OwnedNode::leaf(Vec::new(), Vec::new()));
    counter.value = Some(key_string(key));
    next.put(&[&[ATTRIBUTES], &[LAST_KEY]], counter);

    changes.push((
        format!("[4, 5, L, 03, {}, 07]", hex(&body_node.segment)),
        format!(
            "the body part's list of its objects: web viewer {key} added at its end ({} listed \
             before)",
            listed.len()
        ),
    ));
    let mut list_node = stored_list_node.clone();
    list_node.value = Some(part_list_value);
    next.put(
        &[&[PARTS], &body_node.segment.clone(), &[PART_OBJECTS]],
        list_node,
    );
    Ok(PlannedWebViewer {
        viewer: key,
        part: body.id,
        stacking: objects.root.len(),
        overlaps,
        member: next,
        changes,
    })
}

/// The web viewer an earlier placement of `viewer` made on `member`, if it
/// is on top of its root stack with that frame, those options and that
/// address (both streams), and is the last object the layout issued: its
/// key. As for the other placements, an identical viewer the layout had
/// on top before is not told apart.
pub fn already_placed_web_viewer(member: &OwnedNode, viewer: &WebViewerPlacement) -> Option<u64> {
    let layout = decode(member)?;
    let top = layout.objects.get(*layout.objects.root.last()?)?;
    let made = encode::web_viewer_object(top.id, &viewer.frame, viewer.flags);
    if LayoutObject::decode(&made).ok()? != strip(top) {
        return None;
    }
    let formula = member.at(&[
        &[ATTRIBUTES],
        &serial::encode(top.id),
        &WEB_ADDRESS,
        &[FORMULA],
    ])?;
    let stored = om_scripts::Calculation::decode(formula)?;
    let issued = last_issued(member).ok().flatten();
    (stored.polish.as_deref() == Some(&viewer.polish[..])
        && stored.formatted.as_deref() == Some(&viewer.formatted[..])
        && issued == Some(top.id))
    .then_some(top.id)
}
