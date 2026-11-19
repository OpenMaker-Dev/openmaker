//! FileMaker's XML for script steps: `fmxmlsnippet` clipboard snippets and
//! the `<Step>` elements of Save as XML (script-steps.md, "FileMaker XML on
//! paste"). A small reader turns the text into elements; every `<Step>` in
//! document order becomes one pasted step.
//!
//! A step's `<Options>` is its stored flags word; its Booleans spell out
//! some of the same bits and stand in for it when it is missing. Each
//! `Calculation` names the slot holding it (`position`) and is compiled
//! against the open file's names. Fields, table occurrences, layouts,
//! scripts, base tables and value lists are found by name in the open
//! file. The XML's ids are another file's: a name the file lacks, or that
//! several of its objects share, fails the step rather than take an id that
//! may name a different object here. Anything the reader does not map
//! fails the step, so a step never comes back half filled.

use super::parse::{compile, id_key, path_stream, serial_key};
use super::{Lookup, Pasted, StepNames};
use crate::key::FieldSpec;
use crate::schema::{step_schema, Location, SlotKind};
use crate::specs::{encode_requests, SortEntry, SortSpec, SpecField, StoredRequest};
use crate::step::{flag, ScriptStep, StepData};
use crate::value::bytes_node;
use om_hbam::OwnedNode;
use std::collections::HashSet;
use std::ops::Range;

/// The steps of FileMaker's XML: every `<Step>` element in document order,
/// each a step or, when it holds something that does not map,
/// [`Pasted::Failed`] with that step's XML. A text that is not well-formed
/// XML, or holds no step, is one failed record. Elements nested deeper
/// than [`MAX_XML_DEPTH`], an element with more than
/// [`MAX_XML_ATTRIBUTES`] attributes, or more than
/// [`super::MAX_PASTE_STEPS`] steps refuse the text whole.
pub fn parse_xml_steps(text: &str, names: &dyn StepNames) -> Result<Vec<Pasted>, String> {
    let failed = |reason: String| {
        Ok(vec![Pasted::Failed {
            text: text.to_owned(),
            reason,
        }])
    };
    let nodes = match Document::parse(text) {
        Ok(nodes) => nodes,
        Err(XmlError::TooDeep) => {
            return Err(format!(
            "The XML nests elements more than {MAX_XML_DEPTH} deep; FileMaker's step XML does not."
        ))
        }
        Err(XmlError::TooManyAttributes) => {
            return Err(format!(
                "The XML gives an element more than {MAX_XML_ATTRIBUTES} attributes; \
                 FileMaker's step XML does not."
            ))
        }
        Err(XmlError::Malformed(reason)) => return failed(reason),
    };
    let mut steps = Vec::new();
    for node in &nodes {
        if let Node::Element(element) = node {
            element.collect("Step", &mut steps);
        }
    }
    if steps.is_empty() {
        return failed("The XML holds no script step".into());
    }
    super::text::too_many_steps(steps.len())?;
    Ok(steps
        .into_iter()
        .map(|element| match read_step(element, names) {
            Ok(step) => Pasted::Step(step),
            Err(reason) => Pasted::Failed {
                text: text[element.span.clone()].to_owned(),
                reason,
            },
        })
        .collect())
}

type Read<T> = Result<T, String>;

const UNREAD_CALLBACK: &str = "The callback of Perform Script on Server with Callback is not read";

/// Attributes that carry nothing to read anywhere: UUIDs, member counts
/// and FileMaker's hashes.
const INFORMATIONAL: [&str; 3] = ["UUID", "membercount", "hash"];

/// One `<Step>` element as a step.
fn read_step(element: &Element, names: &dyn StepNames) -> Read<ScriptStep> {
    element.known(&["index", "id", "name", "enable"])?;
    element.no_text()?;
    let id = element
        .attr("id")
        .and_then(|v| v.trim().parse::<u16>().ok())
        .filter(|id| (1..=i16::MAX as u16).contains(id))
        .ok_or("The step has no valid id")?;
    let enabled = match element.attr("enable") {
        None | Some("True") => true,
        Some("False") => false,
        Some(other) => return Err(format!("enable=\"{other}\" is not True or False")),
    };
    if id == 117 {
        // Its data source and query are not in FileMaker's XML.
        return Err("Execute SQL's query is not in FileMaker's XML".into());
    }
    let mut reader = StepReader {
        names,
        step: ScriptStep::new(id),
        on: 0,
        off: 0,
        chosen: false,
    };
    let mut options = None;
    for child in element.elements() {
        match child.name.as_str() {
            // A pasted step is a new step: it gets its own UUIDs.
            "UUID" | "SourceUUID" | "OwnerID" => {}
            "Options" => {
                child.known(&[])?;
                // Written as a signed 32-bit number (`-2147483648`).
                let text = child.text();
                let word = text
                    .trim()
                    .parse::<i64>()
                    .ok()
                    .filter(|w| (i64::from(i32::MIN)..=i64::from(u32::MAX)).contains(w))
                    .ok_or_else(|| format!("<Options>{text}</Options> is not a flags word"))?;
                options = Some(word as u32);
            }
            "ParameterValues" => reader.parameter_values(child)?,
            "External" => return Err("Plug-in steps are not read".into()),
            other => return Err(format!("<{other}> is not read")),
        }
    }
    // `<Options>` is the stored flags word; the Booleans spell out some of
    // its bits (and in two steps not as the schema reads them), so they
    // only stand in when it is missing.
    match options {
        Some(options) => reader.step.flags = hidden(&reader.step, options, reader.chosen)?,
        None => reader.step.flags = (reader.step.flags | reader.on) & !reader.off,
    }
    reader.step.set_enabled(enabled);
    Ok(reader.step)
}

/// The flags bits that say a step holds data of their kind.
const DATA: u32 = flag::FIELD_SPEC
    | flag::TEXT_DATA
    | flag::MEMBER_KEY
    | flag::FILE_REFERENCE
    | flag::FILE_PATH
    | flag::SCRIPT_KEY
    | flag::CALCULATION
    | flag::EXTENDED_DATA;

/// `options` for the step read, or why the step would come back half
/// filled: FileMaker's XML leaves out data a step's choices do not use
/// (and Go to List of Records' and Execute SQL's always), while the flags
/// still say it is there. The step's own flags say what was read, and
/// `chosen` whether the XML gave the step's choice.
fn hidden(step: &ScriptStep, mut options: u32, chosen: bool) -> Read<u32> {
    let mut missing = options & DATA & !step.flags;
    // Reported for Save a Copy as XML even when it stores no calculation.
    if step.id() == 3 {
        missing &= !flag::CALCULATION;
        options &= !(flag::CALCULATION & !step.flags);
    }
    // Save a Copy as XML, Read from Data File and Set Data File Position
    // set `kExtendedData` beside a calculation: a flag of its own there.
    let marker = step_schema(step.id()).is_some_and(|s| {
        s.locations()
            .any(|l| matches!(*l, Location::Flag { bits, .. } if bits & flag::EXTENDED_DATA != 0))
    });
    if marker {
        missing &= !flag::EXTENDED_DATA;
    }
    // A spec kept without Restore is not used (script-steps.md 11.2):
    // FileMaker's XML importer drops it too.
    if missing & flag::EXTENDED_DATA != 0 && options & flag::RESTORE == 0 {
        missing &= !flag::EXTENDED_DATA;
        options &= !flag::EXTENDED_DATA;
    }
    let mut what: Vec<&str> = [
        (flag::CALCULATION, "a calculation"),
        (flag::FIELD_SPEC, "a field"),
        (flag::MEMBER_KEY, "a layout, table or menu set"),
        (flag::SCRIPT_KEY, "a script"),
        (flag::TEXT_DATA, "a text"),
        (flag::FILE_PATH, "file paths"),
        (flag::FILE_REFERENCE, "a file reference"),
        (flag::EXTENDED_DATA, "a stored spec"),
    ]
    .into_iter()
    .filter(|(bits, _)| missing & bits != 0)
    .map(|(_, what)| what)
    .collect();
    // Block steps' step value is an index the editor recomputes.
    if options & flag::MENU_SELECTION != 0
        && !chosen
        && step.value == 0
        && !crate::flow::caches_index(step.id())
    {
        what.push("a choice");
    }
    if what.is_empty() {
        Ok(options)
    } else {
        Err(format!(
            "The step holds {} that FileMaker's XML does not show",
            what.join(", ")
        ))
    }
}

/// Reads a step's parameters into the step.
struct StepReader<'a> {
    names: &'a dyn StepNames,
    step: ScriptStep,
    /// Flag bits the Booleans turn on and off, for a step without
    /// `<Options>`.
    on: u32,
    off: u32,
    /// Whether the XML gave the step's choice (its step value), 0 included.
    chosen: bool,
}

/// Whether a Boolean's `value` is `True`.
fn truth(element: &Element) -> Read<bool> {
    match element.attr("value") {
        Some("True") => Ok(true),
        Some("False") => Ok(false),
        other => Err(format!(
            "<{}> value {other:?} is not True or False",
            element.name
        )),
    }
}

/// A number attribute.
fn number<T: std::str::FromStr>(element: &Element, attr: &str) -> Read<T> {
    element
        .attr(attr)
        .and_then(|v| v.trim().parse().ok())
        .ok_or_else(|| format!("<{}> has no number {attr}", element.name))
}

/// The text of a calculation element: `Calculation` > `Calculation` >
/// `Text`; `None` when it is empty.
fn calculation_text(element: &Element) -> Read<Option<String>> {
    element.no_text()?;
    let mut text = None;
    for inner in element.elements() {
        if inner.name != "Calculation" || text.is_some() {
            return Err(format!("<{}> in a calculation is not read", inner.name));
        }
        inner.known(&[])?;
        inner.no_text()?;
        for part in inner.elements() {
            if part.name != "Text" || text.is_some() {
                return Err(format!("<{}> in a calculation is not read", part.name));
            }
            part.known(&[])?;
            text = Some(part.text());
        }
    }
    Ok(text.filter(|t| !t.is_empty()))
}

/// A repetition as the XML writes it: an integer (the field spec's
/// repetition) or a calculation in the slot it names.
enum Repetition {
    Number(i16),
    Calculation(u16, String),
}

/// The `<repetition>` of a field or variable target; `None` when empty.
fn repetition(element: Option<&Element>) -> Read<Option<Repetition>> {
    let Some(element) = element else {
        return Ok(None);
    };
    element.known(&[])?;
    element.no_text()?;
    let mut out = None;
    for calc in element.elements() {
        if calc.name != "Calculation" || out.is_some() {
            return Err(format!("<{}> in a repetition is not read", calc.name));
        }
        calc.known(&["datatype", "position"])?;
        let Some(text) = calculation_text(calc)? else {
            continue;
        };
        // FileMaker shows a stored repetition number as its digits, and a
        // calculated repetition (spec repetition 0) as the calculation.
        out = Some(match text.trim().parse::<i16>() {
            Ok(n) if n >= 1 => Repetition::Number(n),
            _ => Repetition::Calculation(number(calc, "position")?, text),
        });
    }
    Ok(out)
}

impl StepReader<'_> {
    fn id(&self) -> u16 {
        self.step.id()
    }

    fn flag(&mut self, bits: u32, on: bool) {
        if on {
            self.on |= bits;
            self.off &= !bits;
        } else {
            self.off |= bits;
            self.on &= !bits;
        }
    }

    /// Set the step value (a list choice, `kMenuSelection`; `<Options>`
    /// gives the stored flags when present).
    fn choice(&mut self, value: u16) {
        self.chosen = true;
        self.step.value = value;
        if value != 0 {
            self.step.flags |= flag::MENU_SELECTION;
        }
    }

    fn compile_into(&mut self, slot: u16, text: &str) -> Read<()> {
        let calc = compile(text, self.names)?;
        self.step.set_calculation(slot, Some(&calc));
        Ok(())
    }

    /// Whether a Boolean of `bits` is true when the bit is clear, as "With
    /// dialog", Perform Script on Server's "Wait for completion" and
    /// Convert File's "Open File" (oracle: True with `kOption` clear).
    fn inverted(&self, bits: u32) -> bool {
        if (self.id(), bits) == (139, flag::OPTION) {
            return true;
        }
        step_schema(self.id())
            .and_then(|s| {
                s.locations().find_map(|l| match *l {
                    Location::Flag { bits: b, inverted } if b == bits => Some(inverted),
                    _ => None,
                })
            })
            .unwrap_or(bits == flag::NO_INTERACT)
    }

    fn parameter_values(&mut self, values: &Element) -> Read<()> {
        values.known(&[])?;
        values.no_text()?;
        for parameter in values.elements() {
            match parameter.name.as_str() {
                "Parameter" => self.parameter(parameter)?,
                // Read from Data File, Write to Data File.
                "Encoding" => {
                    parameter.known(&["type", "name"])?;
                    let value = number(parameter, "type")?;
                    self.choice(value);
                }
                other => return Err(format!("<{other}> is not read")),
            }
        }
        Ok(())
    }

    /// One `<Parameter>`: its `type` names it; its content is read by the
    /// step's rule or by the elements it holds.
    fn parameter(&mut self, parameter: &Element) -> Read<()> {
        parameter.known(&["type", "value"])?;
        parameter.no_text()?;
        let kind = parameter.attr("type").unwrap_or_default();
        let id = self.id();
        match (id, kind) {
            (87, "Button1" | "Button2" | "Button3") => return self.button(parameter, kind),
            (87, "Field1" | "Field2" | "Field3") => return self.input(parameter, kind),
            (1 | 164 | 210, "List") => return self.script_choice(parameter),
            // Pro's XML names the callback `CallbackScriptReference id=n`
            // and keeps its name and parameter calculations at positions 7
            // and 4, but the elements' full shape (and the state's) is not
            // established; a nested `ScriptReference` would otherwise
            // replace the script to run (script-steps.md 12.2).
            (210, kind) if kind.starts_with("Callback") => return Err(UNREAD_CALLBACK.into()),
            (16 | 99, "Records" | "Portal") => return self.go_to_choice(parameter),
            (71, "List") => return self.flush(parameter),
            (62, "Options") => return self.pause(parameter),
            (74, "Related") => return self.related(parameter),
            // Its "Skip auto-enter options" reads back differently in
            // FileMaker's writer and importer (script-steps.md 11.8).
            (91 | 249, "replace") => {
                return Err("Replace Field Contents' options are not read".into());
            }
            (175, "Parameter") => return self.javascript_parameters(parameter),
            (182, "List") => return self.truncate_table(parameter),
            (134 | 136 | 137, "Boolean") => {
                if let Some(b) = parameter.elements().find(|b| b.attr("id").is_none()) {
                    return self.account_choice(b);
                }
            }
            _ => {}
        }
        if parameter.attr("value").is_some() {
            return Err(format!("Parameter “{kind}” value is not read"));
        }
        for child in parameter.elements() {
            self.element(child, kind == "Target")?;
        }
        Ok(())
    }

    /// An element of a parameter, by its tag. `target` marks the content of
    /// a `Target` parameter, where a variable may stand for the field.
    fn element(&mut self, e: &Element, target: bool) -> Read<()> {
        match e.name.as_str() {
            name if self.id() == 210 && name.starts_with("Callback") => Err(UNREAD_CALLBACK.into()),
            "Boolean" => self.boolean(e),
            "Calculation" => self.calculation(e),
            "Restore" => {
                e.known(&["value"])?;
                let on = truth(e)?;
                self.flag(flag::RESTORE, on);
                Ok(())
            }
            "Comment" | "Text" | "Flow" => {
                e.known(&["value"])?;
                e.no_children()?;
                if let Some(text) = e.attr("value") {
                    self.step.set_text(0, Some(text));
                }
                Ok(())
            }
            // Set Variable, Get Folder Path: the variable's name.
            "Name" if e.attr("value").is_some() => {
                e.known(&["value"])?;
                e.no_children()?;
                self.step.set_text(0, e.attr("value"));
                Ok(())
            }
            "FieldReference" => self.field_target(e, 0),
            "Variable" if target => self.variable_target(e, 0),
            "UniversalPathList" => self.paths(e),
            "LayoutReferenceContainer" => self.layout_container(e, true),
            "Animation" => self.animation(e),
            "List" => self.list(e),
            "ScriptReference" => self.script(e),
            "SortSpecification" => self.sort(e),
            "FindRequestSet" => self.requests(e),
            "WindowReference" => self.window(e),
            "URL" => {
                e.known(&["autoEncode"])?;
                if let (160 | 211, Some(auto)) = (self.id(), e.attr("autoEncode")) {
                    // Insert from URL: kOption = "Do not automatically
                    // encode URL" (script-steps.md 11.8).
                    self.flag(flag::OPTION, auto == "False");
                }
                self.container(e)
            }
            "DataSourceReference" => {
                e.known(&["id", "name"])?;
                e.no_children()?;
                match number::<u64>(e, "id")? {
                    0 => Ok(()),
                    _ => Err("References to other files are not read".into()),
                }
            }
            "PrivilegeSetReference" => {
                e.known(&["id", "name"])?;
                e.no_children()?;
                match number::<u64>(e, "id")? {
                    0 => Ok(()),
                    _ => Err("Privilege sets are not read".into()),
                }
            }
            "CustomMenuSetReference" => self.menu_set(e),
            // Derived from other values; nothing of its own.
            "Label" | "HiddenStepsCount" => Ok(()),
            // Wrappers named after what they hold (`Name`, `value`,
            // `repetition`, `Start`, `Bounds`, a nested `Parameter`, ...):
            // read what is inside.
            _ if e.attrs.is_empty() || e.name == "Parameter" => {
                if e.name == "Parameter" {
                    if e.attr("type") == Some("Target") {
                        return Err("A target inside another parameter is not read".into());
                    }
                    e.known(&["type"])?;
                }
                self.container(e)
            }
            other => Err(format!("<{other}> is not read")),
        }
    }

    /// The elements inside a wrapper.
    fn container(&mut self, e: &Element) -> Read<()> {
        e.no_text()?;
        for child in e.elements() {
            self.element(child, false)?;
        }
        Ok(())
    }

    fn boolean(&mut self, b: &Element) -> Read<()> {
        b.known(&["type", "id", "value", "position"])?;
        b.no_children()?;
        let on = truth(b)?;
        if b.attr("id").is_some() {
            let bits: u32 = number(b, "id")?;
            let inverted = self.inverted(bits);
            self.flag(bits, on != inverted);
            return Ok(());
        }
        match b.attr("type").unwrap_or_default() {
            "Exit after last" => self.flag(flag::EXIT, on),
            "With dialog" => self.flag(flag::NO_INTERACT, !on),
            label => return Err(format!("The option “{label}” is not read")),
        }
        Ok(())
    }

    /// A calculation in the slot its `position` names.
    fn calculation(&mut self, c: &Element) -> Read<()> {
        c.known(&["datatype", "position"])?;
        let text = calculation_text(c)?;
        match (c.attr("position"), text) {
            (_, None) => Ok(()),
            (None, Some(_)) => Err("A calculation has no position".into()),
            (Some(_), Some(text)) => {
                let slot = number(c, "position")?;
                self.compile_into(slot, &text)
            }
        }
    }

    /// The occurrence of a `TableOccurrenceReference`, by name.
    fn occurrence(&mut self, to: &Element) -> Read<u64> {
        to.known(&["id", "name"])?;
        to.no_children()?;
        let name = to.attr("name").unwrap_or_default();
        self.names
            .find_occurrence(name)
            .require(&format!("the table occurrence “{name}”"))
    }

    /// The occurrence and field of a `FieldReference`.
    fn field(&mut self, f: &Element) -> Read<(u64, u64)> {
        let to = f
            .child("TableOccurrenceReference")
            .ok_or("A field has no table occurrence")?;
        let occurrence = self.occurrence(to)?;
        let name = f.attr("name").unwrap_or_default();
        let what = format!(
            "the field “{}::{name}”",
            to.attr("name").unwrap_or_default()
        );
        let id = self.names.find_field(occurrence, name).require(&what)?;
        Ok((occurrence, id))
    }

    /// A field target of slot `slot` (`mReserved` for 0): the field and its
    /// repetition, a number or a calculation (spec repetition 0).
    fn field_target(&mut self, f: &Element, slot: u16) -> Read<()> {
        f.known(&["id", "name"])?;
        f.no_text()?;
        let mut rep = None;
        for child in f.elements() {
            match child.name.as_str() {
                "TableOccurrenceReference" => {}
                "repetition" => rep = Some(child),
                other => return Err(format!("<{other}> in a field reference is not read")),
            }
        }
        let (occurrence, field) = self.field(f)?;
        let rep = repetition(rep)?;
        let number = match &rep {
            Some(Repetition::Number(n)) => *n,
            Some(Repetition::Calculation(..)) => 0,
            None => 1,
        };
        let spec = FieldSpec::new(
            id_key(occurrence).ok_or("The table occurrence's id does not fit")?,
            id_key(field).ok_or("The field's id does not fit")?,
            number,
        );
        self.step.set_field_spec(slot, Some(spec));
        if let Some(Repetition::Calculation(slot, text)) = rep {
            self.compile_into(slot, &text)?;
        }
        Ok(())
    }

    /// A variable target of slot `slot`: its name and repetition.
    fn variable_target(&mut self, v: &Element, slot: u16) -> Read<()> {
        v.known(&["value"])?;
        v.no_text()?;
        let mut rep = None;
        for child in v.elements() {
            match child.name.as_str() {
                "repetition" => rep = Some(child),
                other => return Err(format!("<{other}> in a variable is not read")),
            }
        }
        let name = v.attr("value").ok_or("A variable has no name")?;
        self.step.set_variable_target(slot, name);
        let number = match repetition(rep)? {
            Some(Repetition::Number(n)) => n,
            Some(Repetition::Calculation(at, text)) => {
                self.compile_into(at, &text)?;
                0
            }
            None => 1,
        };
        self.step
            .set_field_spec(slot, Some(FieldSpec::variable(number)));
        Ok(())
    }

    /// File paths, one `Location` each, at `[00 0, 82]`.
    fn paths(&mut self, list: &Element) -> Read<()> {
        list.known(&[])?;
        list.no_text()?;
        let mut paths = Vec::new();
        for objects in list.elements() {
            if objects.name != "ObjectList" {
                return Err(format!("<{}> in a path list is not read", objects.name));
            }
            objects.known(&[])?;
            objects.no_text()?;
            for location in objects.elements() {
                if location.name != "Location" {
                    return Err(format!("<{}> in a path list is not read", location.name));
                }
                location.known(&[])?;
                if let Some(child) = location.elements().next() {
                    return Err(format!("<{}> in a path is not read", child.name));
                }
                paths.push(location.text());
            }
        }
        if paths.is_empty() {
            return Ok(());
        }
        if paths.iter().any(|p| p.contains('¶')) {
            return Err("A file path holds ¶".into());
        }
        let stream = path_stream(&paths.join("¶"))?;
        self.step.set_slot_child(
            0,
            StepData::PATH_STRING,
            Some(bytes_node(&[StepData::PATH_STRING], &stream)),
        );
        self.step.flags |= flag::FILE_PATH;
        Ok(())
    }

    /// A layout choice: `value` is the step value; a `LayoutReference` the
    /// layout key in `mReserved`, a calculation its name or number. `own`
    /// is false for the copy a new window repeats.
    fn layout_container(&mut self, c: &Element, own: bool) -> Read<()> {
        c.known(&["value"])?;
        c.no_text()?;
        let value: u16 = number(c, "value")?;
        if !own {
            return Ok(());
        }
        self.chosen = true;
        self.step.value = value;
        if self.id() == 6 && !matches!(value, 0 | 5) {
            self.step.flags |= flag::MENU_SELECTION;
        }
        for child in c.elements() {
            match child.name.as_str() {
                "LayoutReference" => self.layout(child)?,
                "Calculation" => self.calculation(child)?,
                "Label" => {}
                other => return Err(format!("<{other}> in a layout choice is not read")),
            }
        }
        Ok(())
    }

    fn layout(&mut self, l: &Element) -> Read<()> {
        l.known(&["id", "name"])?;
        l.no_children()?;
        let name = l.attr("name").unwrap_or_default();
        let id = self
            .names
            .find_layout(name, None)
            .require(&format!("the layout “{name}”"))?;
        self.step
            .set_member_key(Some(id_key(id).ok_or("The layout's id does not fit")?));
        Ok(())
    }

    /// A layout change's animation: the list choice of slot 6, `None`
    /// stored as 0 (as FileMaker stores it).
    fn animation(&mut self, a: &Element) -> Read<()> {
        a.known(&["name", "value"])?;
        a.no_children()?;
        let value = number(a, "value")?;
        self.step.set_list_value(6, Some(value));
        Ok(())
    }

    /// A list choice the step stores as its step value: the steps whose
    /// schema has one step value and no slot choices.
    fn list(&mut self, l: &Element) -> Read<()> {
        l.known(&["name", "value"])?;
        let value: u16 = number(l, "value")?;
        let schema = step_schema(self.id());
        let values = schema.map_or(0, |s| {
            s.params
                .iter()
                .filter(|p| p.storage.contains(&Location::Value))
                .count()
        });
        let slot_lists = schema.is_some_and(|s| {
            s.locations().any(|l| {
                matches!(
                    l,
                    Location::Slot {
                        kind: SlotKind::List,
                        ..
                    }
                )
            })
        });
        match value {
            _ if values != 1 || slot_lists => {
                if value != 0 {
                    let name = l.attr("name").unwrap_or_default();
                    return Err(format!("The choice “{name}” ({value}) is not read"));
                }
            }
            0 => self.chosen = true,
            _ => self.choice(value),
        }
        self.container(l)
    }

    /// A script, by name.
    fn script(&mut self, s: &Element) -> Read<()> {
        s.known(&["id", "name"])?;
        let name = s.attr("name").unwrap_or_default();
        let id = self
            .names
            .find_script(name)
            .require(&format!("the script “{name}”"))?;
        self.step
            .set_script_key(Some(serial_key(id).ok_or("The script's id does not fit")?));
        // Install OnTimer Script and the callback steps hold the script's
        // parameter inside the reference.
        self.container(s)
    }

    /// Perform Script and Perform Script on Server (with Callback): "From
    /// list" (1) with a script, or "By name" (2) with the name calculation
    /// in slot 2 and `[00 1]` = 1 (script-steps.md 10, 12.2).
    fn script_choice(&mut self, p: &Element) -> Read<()> {
        let mut lists = p.elements();
        let (Some(list), None) = (lists.next(), lists.next()) else {
            return Err("The script choice is not one list".into());
        };
        if list.name != "List" {
            return Err(format!("<{}> as the script choice is not read", list.name));
        }
        list.known(&["name", "value"])?;
        list.no_text()?;
        // FileMaker's importer stores the choice with `kMenuSelection`.
        self.chosen = true;
        match number::<u16>(list, "value")? {
            1 => {}
            2 => self.step.set_list_value(1, Some(1)),
            other => return Err(format!("The script choice {other} is not read")),
        }
        for child in list.elements() {
            match child.name.as_str() {
                "ScriptReference" => self.script(child)?,
                "Calculation" => self.calculation(child)?,
                "DataSourceReference" => {
                    return Err("Scripts in other files are not read".into());
                }
                other => return Err(format!("<{other}> in the script choice is not read")),
            }
        }
        Ok(())
    }

    /// Go to Record/Request/Page, Go to Portal Row: the step value, with
    /// "Exit after last", "With dialog" and the number calculation inside.
    fn go_to_choice(&mut self, p: &Element) -> Read<()> {
        for list in p.elements() {
            if list.name != "List" {
                return Err(format!("<{}> as the record choice is not read", list.name));
            }
            list.known(&["name", "value"])?;
            let value = number(list, "value")?;
            self.choice(value);
            self.container(list)?;
        }
        Ok(())
    }

    /// Loop's flush: Always (1) is not stored; Minimum and Defer are the
    /// list value of slot 1, read with `kOption`.
    fn flush(&mut self, p: &Element) -> Read<()> {
        for list in p.elements() {
            if list.name != "List" {
                return Err(format!("<{}> as the flush choice is not read", list.name));
            }
            list.known(&["name", "value"])?;
            list.no_children()?;
            match number::<u32>(list, "value")? {
                0 | 1 => {}
                value => {
                    self.step.set_list_value(1, Some(value));
                    self.step.flags |= flag::OPTION;
                }
            }
        }
        Ok(())
    }

    /// Pause/Resume Script: a duration is step value 1.
    fn pause(&mut self, p: &Element) -> Read<()> {
        for options in p.elements() {
            if options.name != "Options" {
                return Err(format!("<{}> as the pause is not read", options.name));
            }
            options.known(&["type"])?;
            self.chosen = true;
            match options.attr("type").unwrap_or_default() {
                "Indefinitely" => options.no_children()?,
                kind if kind.starts_with("Duration") => {
                    self.step.value = 1;
                    self.container(options)?;
                }
                kind => return Err(format!("The pause “{kind}” is not read")),
            }
        }
        Ok(())
    }

    /// Go to Related Record: the related occurrence at `[00 0, 11]`, the
    /// layout, the animation, a new window's style and calculations.
    fn related(&mut self, p: &Element) -> Read<()> {
        for child in p.elements() {
            match child.name.as_str() {
                "TableOccurrenceReference" => {
                    let occurrence = self.occurrence(child)?;
                    let key = om_formula::Key::from_id(occurrence).0;
                    self.step
                        .set_slot_child(0, 0x11, Some(OwnedNode::leaf([0x11], key)));
                }
                "LayoutReferenceContainer" => self.layout_container(child, true)?,
                "Animation" => self.animation(child)?,
                "WindowReference" => self.window(child)?,
                // "Show only related records" and "Match found set": the
                // flags.
                "Options" => {
                    child.known(&["matchFoundSet", "ShowRelated"])?;
                    child.no_children()?;
                }
                other => return Err(format!("<{other}> in Go to Related Record is not read")),
            }
        }
        Ok(())
    }

    /// A window: which one (the step value: 0 current, 1 or 2 by name),
    /// a new window's style word (`[00 0, 03]`), name, layout and bounds.
    fn window(&mut self, w: &Element) -> Read<()> {
        w.known(&[])?;
        w.no_text()?;
        let new_window = self.id() == 122;
        for child in w.elements() {
            match child.name.as_str() {
                "Select" => {
                    child.known(&["kind", "type"])?;
                    let kind = number(child, "kind")?;
                    self.choice(kind);
                    for name in child.elements() {
                        if name.name != "Name" {
                            return Err(format!("<{}> in a window is not read", name.name));
                        }
                        name.known(&["current"])?;
                        self.container(name)?;
                    }
                }
                "Style" => {
                    child.known(&["name", "value"])?;
                    child.no_children()?;
                    let word: u32 = number(child, "value")?;
                    self.step.set_number(0, Some(word as i32));
                }
                "LayoutReferenceContainer" => self.layout_container(child, new_window)?,
                // The style word's options, spelled out.
                "Options" => child.known(&["value"])?,
                "Name" | "Bounds" | "Rename" => {
                    child.known(&[])?;
                    self.container(child)?;
                }
                other => return Err(format!("<{other}> in a window is not read")),
            }
        }
        Ok(())
    }

    /// Show Custom Dialog's button n: its label text (slot n - 1) or label
    /// calculation (slot n + 4) and commit flag (`[00 n + 2, 03]`).
    fn button(&mut self, p: &Element, kind: &str) -> Read<()> {
        let n = u16::from(kind.as_bytes()[kind.len() - 1] - b'1');
        // A text label is the `value`; a calculated one the calculation
        // (FileMaker's XML importer stores no calculation beside a text).
        if let Some(label) = p.attr("value") {
            self.step.set_text(n, Some(label));
        }
        for child in p.elements() {
            match child.name.as_str() {
                "Boolean" if child.attr("type") == Some("Commit") => {
                    child.known(&["type", "value"])?;
                    child.no_children()?;
                    let commit = truth(child)?;
                    self.step.set_number_bool(3 + n, Some(commit));
                }
                "Calculation" => self.calculation(child)?,
                other => return Err(format!("<{other}> in a button is not read")),
            }
        }
        Ok(())
    }

    /// Show Custom Dialog's input n: its target (field or variable), the
    /// password flag (`[00 n - 1, 03]`) and the label calculation.
    fn input(&mut self, p: &Element, kind: &str) -> Read<()> {
        let n = u16::from(kind.as_bytes()[kind.len() - 1] - b'1');
        for child in p.elements() {
            match (child.name.as_str(), child.attr("type")) {
                ("Parameter", Some("Target")) => {
                    child.known(&["type"])?;
                    child.no_text()?;
                    for target in child.elements() {
                        match target.name.as_str() {
                            "FieldReference" => self.field_target(target, n)?,
                            "Variable" => self.variable_target(target, n)?,
                            other => return Err(format!("<{other}> as an input is not read")),
                        }
                    }
                }
                ("Boolean", Some("Password")) => {
                    child.known(&["type", "value"])?;
                    child.no_children()?;
                    let password = truth(child)?;
                    self.step.set_number_bool(n, Some(password));
                }
                ("Parameter", Some("Label")) => {
                    child.known(&["type"])?;
                    self.container(child)?;
                }
                (other, _) => return Err(format!("<{other}> in an input is not read")),
            }
        }
        Ok(())
    }

    /// Perform JavaScript in Web Viewer: parameter i is the calculation in
    /// slot 10 + i; `[00 0, 03]` holds their count (script-steps.md 10).
    fn javascript_parameters(&mut self, p: &Element) -> Read<()> {
        let mut count = 0;
        for calc in p.elements() {
            if calc.name != "Calculation" {
                return Err(format!("<{}> as a parameter is not read", calc.name));
            }
            self.calculation(calc)?;
            count += 1;
        }
        if count > 0 {
            self.step.set_number(0, Some(count));
        }
        Ok(())
    }

    /// Truncate Table: the step value and the base table (`mReserved`).
    fn truncate_table(&mut self, p: &Element) -> Read<()> {
        for list in p.elements() {
            if list.name != "List" {
                return Err(format!("<{}> as the table is not read", list.name));
            }
            list.known(&["name", "value"])?;
            let value = number(list, "value")?;
            if value == 0 {
                list.no_children()?;
                self.chosen = true;
                continue;
            }
            self.choice(value);
            let table = list
                .child("BaseTableReference")
                .ok_or("The table has no reference")?;
            table.known(&["id", "name"])?;
            let name = table.attr("name").unwrap_or_default();
            let id = self
                .names
                .find_table(name)
                .require(&format!("the table “{name}”"))?;
            self.step
                .set_member_key(Some(id_key(id).ok_or("The table's id does not fit")?));
        }
        Ok(())
    }

    /// Add Account's "Expire password", Reset Account Password's and
    /// Enable Account's choice: shown from the step value, whose stored
    /// number for the other choice (1, 2 or 4) the XML does not tell.
    fn account_choice(&mut self, b: &Element) -> Read<()> {
        b.known(&["type", "value", "name"])?;
        b.no_children()?;
        let set = truth(b)?;
        // Enable Account shows Activate (True) for step value 0.
        let stored = if self.id() == 137 { !set } else { set };
        if stored {
            let label = b.attr("name").or(b.attr("type")).unwrap_or_default();
            return Err(format!(
                "The choice “{label}” is not read: its stored value is not known"
            ));
        }
        Ok(())
    }

    /// Install Menu Set: the menu set key in `mReserved`. Menu sets are not
    /// found by name, so the XML's id is taken only when the open file's
    /// menu set of that id has the XML's name.
    fn menu_set(&mut self, m: &Element) -> Read<()> {
        m.known(&["id", "name"])?;
        let id: u64 = number(m, "id")?;
        if id != 0 {
            let name = m.attr("name").unwrap_or_default();
            let id = raw(id)?;
            let named = self
                .names
                .menu_set_name(id)
                .is_some_and(|here| here.to_lowercase() == name.to_lowercase());
            if !named {
                return Err(format!(
                    "The menu set “{name}” is not known by that name in this file"
                ));
            }
            self.step
                .set_member_key(Some(id_key(id).ok_or("The menu set's id does not fit")?));
        }
        self.container(m)
    }

    /// A sort order (`0a`, script-steps.md 11.1.2), written as FileMaker's
    /// XML importer writes it ([`SortSpec::encode`]).
    fn sort(&mut self, s: &Element) -> Read<()> {
        s.known(&["value", "blanksLast", "maintain"])?;
        s.no_text()?;
        let mut fields = Vec::new();
        for list in s.elements() {
            if list.name != "SortList" {
                return Err(format!("<{}> in a sort order is not read", list.name));
            }
            list.known(&[])?;
            list.no_text()?;
            for sort in list.elements() {
                if sort.name != "Sort" {
                    return Err(format!("<{}> in a sort order is not read", sort.name));
                }
                fields.push(self.sort_entry(sort)?);
            }
        }
        let spec = SortSpec {
            keep_sorted: s.attr("maintain") != Some("False"),
            blanks_last: s.attr("blanksLast") == Some("True"),
            fields: fields.iter().map(|(entry, _)| entry.clone()).collect(),
        };
        let stream = sort_stream(&spec, fields.iter().map(|(_, key)| *key));
        self.step.set_data_leaf(
            StepData::SORT_ORDER,
            Some(bytes_node(&[StepData::SORT_ORDER], &stream)),
        );
        Ok(())
    }

    /// One `Sort`: the entry and how its value list key is written.
    fn sort_entry(&mut self, sort: &Element) -> Read<(SortEntry, ValueListKey)> {
        sort.known(&["type"])?;
        sort.no_text()?;
        let mut entry = SortEntry {
            field: SpecField {
                occurrence: None,
                field: 0,
                repetition: 1,
            },
            summary: None,
            value_list: None,
            descending: false,
            locale: 0,
        };
        let mut primary = false;
        let mut key = ValueListKey::Absent;
        for child in sort.elements() {
            match child.name.as_str() {
                "PrimaryField" | "SummaryField" => {
                    child.known(&[])?;
                    child.no_text()?;
                    let f = child
                        .child("FieldReference")
                        .filter(|_| child.elements().count() == 1)
                        .ok_or("A sort field is not one field reference")?;
                    f.known(&["id", "name"])?;
                    if f.elements().any(|e| e.name != "TableOccurrenceReference") {
                        return Err("A sort field's repetition is not read".into());
                    }
                    let (occurrence, field) = self.field(f)?;
                    let spec = SpecField {
                        occurrence: Some(occurrence),
                        field,
                        repetition: 1,
                    };
                    if child.name == "PrimaryField" {
                        entry.field = spec;
                        primary = true;
                    } else {
                        entry.summary = Some(spec);
                    }
                }
                "ValueListReference" => {
                    child.known(&["id", "name"])?;
                    child.no_children()?;
                    let name = child.attr("name").unwrap_or_default();
                    let what = format!("the value list “{name}”");
                    let id = number::<i64>(child, "id")?;
                    key = match self.names.find_value_list(name) {
                        // FileMaker's XML shows the key `02 01 00` (a key
                        // naming no list) as id -1.
                        Lookup::Missing if id <= 0 => ValueListKey::Empty,
                        found => ValueListKey::List(found.require(&what)?),
                    };
                    if let ValueListKey::List(id) = key {
                        entry.value_list = Some(id);
                    }
                }
                "LanguageReference" => {
                    child.known(&["id", "name"])?;
                    child.no_children()?;
                    entry.locale = number(child, "id")?;
                }
                other => return Err(format!("<{other}> in a sort field is not read")),
            }
        }
        if !primary {
            return Err("A sort entry has no field".into());
        }
        match sort.attr("type") {
            Some("Ascending") => {}
            Some("Descending") => entry.descending = true,
            Some("Custom") => {}
            other => return Err(format!("The sort type {other:?} is not read")),
        }
        Ok((entry, key))
    }

    /// Find requests (`06`, script-steps.md 11.2.2), written as a new
    /// request set ([`encode_requests`]).
    fn requests(&mut self, set: &Element) -> Read<()> {
        set.known(&[])?;
        set.no_text()?;
        let mut requests = Vec::new();
        for request in set.elements() {
            if request.name != "FindRequest" {
                return Err(format!("<{}> in find requests is not read", request.name));
            }
            request.known(&["action", "index"])?;
            request.no_text()?;
            let omit = match request.attr("action") {
                Some("find") => false,
                Some("omit") => true,
                other => return Err(format!("The find action {other:?} is not read")),
            };
            let mut criteria = Vec::new();
            for find in request.elements() {
                if find.name != "find" {
                    return Err(format!("<{}> in a find request is not read", find.name));
                }
                find.known(&["criteria"])?;
                find.no_text()?;
                let f = find
                    .child("FieldReference")
                    .filter(|_| find.elements().count() == 1)
                    .ok_or("A criterion is not one field reference")?;
                f.known(&["id", "name", "repetition"])?;
                if f.elements().any(|e| e.name != "TableOccurrenceReference") {
                    return Err("<FieldReference> in a criterion is not read".into());
                }
                let (occurrence, field) = self.field(f)?;
                let repetition = match f.attr("repetition") {
                    None => 1,
                    Some(_) => number(f, "repetition")?,
                };
                criteria.push((
                    SpecField {
                        occurrence: Some(occurrence),
                        field,
                        repetition,
                    },
                    find.attr("criteria").unwrap_or_default().to_owned(),
                ));
            }
            requests.push(StoredRequest { omit, criteria });
        }
        self.step
            .set_data_leaf(StepData::REQUESTS, Some(encode_requests(&requests)));
        Ok(())
    }
}

/// How a sort entry's value list key is stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ValueListKey {
    /// No key (`00`): FileMaker's XML shows the entry's own direction.
    Absent,
    /// The key `02 01 00`: FileMaker's XML shows "Custom" with id -1.
    Empty,
    List(u64),
}

/// A sort order stream (script-steps.md 11.1.1) as FileMaker writes one
/// from its XML: absent summary fields as `02 01 00` keys and repetition 1,
/// the value list key as `keys` say.
fn sort_stream(spec: &SortSpec, keys: impl Iterator<Item = ValueListKey>) -> Vec<u8> {
    fn key(out: &mut Vec<u8>, id: u64) {
        let segment = om_hbam::slot::uint_segment(id);
        out.push(segment.len() as u8 + 1);
        out.push(segment.len() as u8);
        out.extend(segment);
    }
    fn field(out: &mut Vec<u8>, field: Option<SpecField>) {
        key(out, field.and_then(|f| f.occurrence).unwrap_or(0));
        key(out, field.map_or(0, |f| f.field));
        out.extend(field.map_or(1, |f| f.repetition).to_be_bytes());
    }
    let mut out = Vec::new();
    let flags = u16::from(!spec.keep_sorted) | (u16::from(spec.blanks_last) << 1);
    out.extend(flags.to_be_bytes());
    out.extend((spec.fields.len() as u16).to_be_bytes());
    for (entry, list) in spec.fields.iter().zip(keys) {
        field(&mut out, Some(entry.field));
        field(&mut out, entry.summary);
        match list {
            ValueListKey::Absent => out.push(0),
            ValueListKey::Empty => key(&mut out, 0),
            ValueListKey::List(id) => key(&mut out, id),
        }
        out.push(u8::from(entry.descending));
        out.extend(entry.locale.to_be_bytes());
    }
    out
}

/// An XML id as the big-endian id of its key segment, the numbering
/// [`StepNames`] and stored keys use.
fn raw(id: u64) -> Read<u64> {
    om_hbam::serial::raw_of_id(id).ok_or_else(|| format!("The id {id} does not fit a key"))
}

// ----- the reader ------------------------------------------------------------

/// Deepest element nesting read. FileMaker's step XML nests about 20
/// deep (a whole Save as XML document with layout groups somewhat more);
/// the bound keeps the reader's recursion, and the drop of what it built,
/// far from the stack's end.
pub const MAX_XML_DEPTH: usize = 128;

/// Most attributes read on one element. FileMaker's step XML writes a
/// handful; the bound keeps one start tag's work small.
pub const MAX_XML_ATTRIBUTES: usize = 256;

/// Why a text is not read as XML.
#[derive(Debug)]
enum XmlError {
    /// Not well-formed: the reason, with the byte it was found at.
    Malformed(String),
    /// Nested past [`MAX_XML_DEPTH`].
    TooDeep,
    /// An element with more than [`MAX_XML_ATTRIBUTES`] attributes.
    TooManyAttributes,
}

/// An XML element: its name, attributes (entities decoded, values as
/// written) and content, and where it lies in the source.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Element {
    name: String,
    attrs: Vec<(String, String)>,
    children: Vec<Node>,
    /// Byte range of the element, start and end tags included.
    span: Range<usize>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum Node {
    Element(Element),
    /// Character data, entities decoded; a CDATA section is text too.
    Text(String),
}

impl Element {
    fn attr(&self, name: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_str())
    }

    /// The child elements, in order.
    fn elements(&self) -> impl Iterator<Item = &Element> {
        self.children.iter().filter_map(|n| match n {
            Node::Element(e) => Some(e),
            Node::Text(_) => None,
        })
    }

    /// The first child element named `name`.
    fn child(&self, name: &str) -> Option<&Element> {
        self.elements().find(|e| e.name == name)
    }

    /// The character data directly inside the element.
    fn text(&self) -> String {
        self.children
            .iter()
            .filter_map(|n| match n {
                Node::Text(t) => Some(t.as_str()),
                Node::Element(_) => None,
            })
            .collect()
    }

    /// Fail on an attribute this reader does not know for the element.
    fn known(&self, known: &[&str]) -> Read<()> {
        match self
            .attrs
            .iter()
            .find(|(n, _)| !known.contains(&n.as_str()) && !INFORMATIONAL.contains(&n.as_str()))
        {
            Some((name, value)) => Err(format!("<{} {name}=\"{value}\"> is not read", self.name)),
            None => Ok(()),
        }
    }

    /// Fail on character data other than white space between elements.
    fn no_text(&self) -> Read<()> {
        match self.children.iter().find_map(|n| match n {
            Node::Text(t) if !t.trim().is_empty() => Some(t),
            _ => None,
        }) {
            Some(text) => Err(format!(
                "The text “{}” in <{}> is not read",
                text.trim(),
                self.name
            )),
            None => Ok(()),
        }
    }

    /// Fail on any content.
    fn no_children(&self) -> Read<()> {
        self.no_text()?;
        match self.elements().next() {
            Some(child) => Err(format!("<{}> in <{}> is not read", child.name, self.name)),
            None => Ok(()),
        }
    }

    /// Every element named `name` at or below this one, in document order
    /// (not looking inside a match).
    fn collect<'e>(&'e self, name: &str, out: &mut Vec<&'e Element>) {
        if self.name == name {
            out.push(self);
            return;
        }
        for child in self.elements() {
            child.collect(name, out);
        }
    }
}

/// A cursor over the source text.
struct Document<'a> {
    text: &'a str,
    at: usize,
    /// Elements open around the cursor.
    depth: usize,
}

impl<'a> Document<'a> {
    /// The top-level nodes of `text`: elements and text, with the XML
    /// declaration, processing instructions, comments and a document type
    /// declaration skipped.
    fn parse(text: &'a str) -> Result<Vec<Node>, XmlError> {
        let mut doc = Document {
            text,
            at: 0,
            depth: 0,
        };
        let mut nodes = Vec::new();
        while doc.at < text.len() {
            if let Some(node) = doc.node()? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    fn rest(&self) -> &'a str {
        &self.text[self.at..]
    }

    fn error(&self, what: &str) -> XmlError {
        XmlError::Malformed(format!("The XML does not read: {what} at byte {}", self.at))
    }

    /// Skip past `end`, which must follow.
    fn skip_past(&mut self, end: &str, what: &str) -> Result<(), XmlError> {
        match self.rest().find(end) {
            Some(n) => {
                self.at += n + end.len();
                Ok(())
            }
            None => Err(self.error(&format!("{what} does not end"))),
        }
    }

    /// The next node; `None` for markup that is skipped. An end tag here
    /// has no element to close.
    fn node(&mut self) -> Result<Option<Node>, XmlError> {
        let rest = self.rest();
        if rest.starts_with("<?") {
            self.skip_past("?>", "a processing instruction")?;
            Ok(None)
        } else if rest.starts_with("<!--") {
            self.skip_past("-->", "a comment")?;
            Ok(None)
        } else if let Some(body) = rest.strip_prefix("<![CDATA[") {
            let end = body
                .find("]]>")
                .ok_or_else(|| self.error("a CDATA section does not end"))?;
            self.at += "<![CDATA[".len() + end + "]]>".len();
            Ok(Some(Node::Text(body[..end].to_owned())))
        } else if rest.starts_with("<!") {
            // A document type declaration, with its internal subset.
            let mut depth = 0i32;
            for (i, c) in rest.char_indices() {
                match c {
                    '[' => depth += 1,
                    ']' => depth -= 1,
                    '>' if depth <= 0 => {
                        self.at += i + 1;
                        return Ok(None);
                    }
                    _ => {}
                }
            }
            Err(self.error("a declaration does not end"))
        } else if rest.starts_with("</") {
            Err(self.error("an end tag closes no element"))
        } else if rest.starts_with('<') {
            self.element().map(|e| Some(Node::Element(e)))
        } else {
            let end = rest.find('<').unwrap_or(rest.len());
            let raw = &rest[..end];
            let text = decode(raw).map_err(|e| self.error(&e))?;
            self.at += end;
            Ok(Some(Node::Text(text)))
        }
    }

    /// An element starting at `<`.
    fn element(&mut self) -> Result<Element, XmlError> {
        // An empty element nests as deep as one with content.
        if self.depth >= MAX_XML_DEPTH {
            return Err(XmlError::TooDeep);
        }
        let start = self.at;
        self.at += 1;
        let name = self.name()?.to_owned();
        let mut attrs = Vec::new();
        let mut seen = HashSet::new();
        loop {
            self.skip_space();
            let rest = self.rest();
            if let Some(after) = rest.strip_prefix("/>") {
                let _ = after;
                self.at += 2;
                return Ok(Element {
                    name,
                    attrs,
                    children: Vec::new(),
                    span: start..self.at,
                });
            }
            if rest.starts_with('>') {
                self.at += 1;
                break;
            }
            if rest.is_empty() {
                return Err(self.error(&format!("the start tag of <{name}> does not end")));
            }
            if attrs.len() == MAX_XML_ATTRIBUTES {
                return Err(XmlError::TooManyAttributes);
            }
            let attr = self.name()?;
            self.skip_space();
            if !self.rest().starts_with('=') {
                return Err(self.error(&format!("attribute {attr} has no value")));
            }
            self.at += 1;
            self.skip_space();
            let quote = match self.rest().chars().next() {
                Some(q @ ('"' | '\'')) => q,
                _ => return Err(self.error(&format!("attribute {attr} is not quoted"))),
            };
            self.at += 1;
            let end = self
                .rest()
                .find(quote)
                .ok_or_else(|| self.error(&format!("attribute {attr} does not end")))?;
            let raw = &self.rest()[..end];
            if raw.contains('<') {
                return Err(self.error(&format!("attribute {attr} holds a <")));
            }
            let value = decode(raw).map_err(|e| self.error(&e))?;
            self.at += end + 1;
            if !seen.insert(attr) {
                return Err(self.error(&format!("attribute {attr} is repeated")));
            }
            attrs.push((attr.to_owned(), value));
        }
        self.depth += 1;
        let mut children = Vec::new();
        loop {
            if self.at >= self.text.len() {
                return Err(self.error(&format!("<{name}> is not closed")));
            }
            if let Some(rest) = self.rest().strip_prefix("</") {
                let end = rest
                    .find('>')
                    .ok_or_else(|| self.error("an end tag does not end"))?;
                let closing = rest[..end].trim_end();
                if closing != name {
                    return Err(self.error(&format!("</{closing}> closes <{name}>")));
                }
                self.at += 2 + end + 1;
                self.depth -= 1;
                return Ok(Element {
                    name,
                    attrs,
                    children,
                    span: start..self.at,
                });
            }
            if let Some(node) = self.node()? {
                children.push(node);
            }
        }
    }

    fn skip_space(&mut self) {
        let rest = self.rest();
        self.at += rest.len() - rest.trim_start().len();
    }

    /// An element or attribute name.
    fn name(&mut self) -> Result<&'a str, XmlError> {
        let rest = self.rest();
        let end = rest
            .find(|c: char| c.is_whitespace() || matches!(c, '/' | '>' | '=' | '<'))
            .unwrap_or(rest.len());
        if end == 0 {
            return Err(self.error("a name is missing"));
        }
        self.at += end;
        Ok(&rest[..end])
    }
}

/// Character data with the five entities and character references decoded.
fn decode(raw: &str) -> Result<String, String> {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        let after = &rest[amp + 1..];
        let semi = after
            .find(';')
            .ok_or_else(|| "an entity has no ;".to_owned())?;
        let entity = &after[..semi];
        let c = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ => {
                let number = match entity.strip_prefix('#') {
                    Some(hex) if hex.starts_with(['x', 'X']) => {
                        u32::from_str_radix(&hex[1..], 16).ok()
                    }
                    Some(dec) => dec.parse().ok(),
                    None => None,
                };
                number
                    .and_then(char::from_u32)
                    .ok_or_else(|| format!("&{entity}; is not an entity"))?
            }
        };
        out.push(c);
        rest = &after[semi + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::display::{parse_steps, Origin};
    use crate::specs;
    use om_formula::{NameTable, Names};

    /// A Tasks-like file: occurrence `Tasks` (0xd00002) on table 0x8002
    /// with Task (1) and Status (4), occurrence `Assignees` (0xd00004),
    /// layout 3 `Task Details`, script 7 `Sort by Name`, value list 2
    /// `Status`. Its ids differ from the XML's below where a test says so.
    struct Tasks {
        names: NameTable,
    }

    impl Tasks {
        fn new() -> Self {
            let mut names = NameTable::default();
            names.add_occurrence(0xd00002, "Tasks", 0x8002);
            names.add_occurrence(0xd00004, "Assignees", 0x8004);
            names.add_field(0x8002, 1, "Task");
            names.add_field(0x8002, 4, "Status");
            names.add_field(0x8004, 1, "First Name");
            Self { names }
        }
    }

    impl StepNames for Tasks {
        fn calc_names(&self) -> &dyn Names {
            &self.names
        }
        fn find_layout(&self, name: &str, _occurrence: Option<&str>) -> Lookup {
            Lookup::by_name([(3, "Task Details")], name)
        }
        fn find_script(&self, name: &str) -> Lookup {
            Lookup::by_name([(7, "Sort by Name")], name)
        }
        fn find_value_list(&self, name: &str) -> Lookup {
            Lookup::by_name([(2, "Status")], name)
        }
    }

    /// Tasks where two scripts and two layouts share a name.
    struct Twins(Tasks);

    impl StepNames for Twins {
        fn calc_names(&self) -> &dyn Names {
            self.0.calc_names()
        }
        fn find_layout(&self, name: &str, _occurrence: Option<&str>) -> Lookup {
            Lookup::by_name([(3, "Task Details"), (4, "Task Details")], name)
        }
        fn find_script(&self, name: &str) -> Lookup {
            Lookup::by_name([(7, "Sort by Name"), (8, "Sort by Name")], name)
        }
    }

    #[test]
    fn names_the_file_lacks_fail_even_when_the_ids_name_other_objects() {
        // Each XML id below names a different object here (field 4 is
        // Status, script 7 Sort by Name, layout 3 Task Details, value list
        // 2 Status): taking the id would select it under another name.
        for (xml, why) in [
            (
                r#"<Step id="17"><Options>1</Options><ParameterValues><Parameter type="FieldReference"><FieldReference id="4" name="Nickname"><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></Parameter></ParameterValues></Step>"#,
                "The field “Tasks::Nickname” is not in this file",
            ),
            (
                r#"<Step id="17"><Options>1</Options><ParameterValues><Parameter type="FieldReference"><FieldReference id="1" name="Task"><TableOccurrenceReference id="1065090" name="Projects"></TableOccurrenceReference></FieldReference></Parameter></ParameterValues></Step>"#,
                "The table occurrence “Projects” is not in this file",
            ),
            (
                r#"<Step id="1"><Options>64</Options><ParameterValues><Parameter type="List"><List name="From list" value="1"><ScriptReference id="7" name="Cleanup"></ScriptReference></List></Parameter></ParameterValues></Step>"#,
                "The script “Cleanup” is not in this file",
            ),
            (
                r#"<Step id="6"><Options>8</Options><ParameterValues><Parameter type="LayoutReferenceContainer"><LayoutReferenceContainer value="5"><LayoutReference id="3" name="Elsewhere"></LayoutReference></LayoutReferenceContainer></Parameter></ParameterValues></Step>"#,
                "The layout “Elsewhere” is not in this file",
            ),
            (
                r#"<Step id="39"><Options>33587328</Options><ParameterValues><Parameter type="Restore"><Restore value="True"></Restore></Parameter><Parameter type="SortSpecification"><SortSpecification value="True" blanksLast="False" maintain="True"><SortList membercount="1"><Sort type="Custom"><PrimaryField><FieldReference id="4" name="Status"><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></PrimaryField><ValueListReference id="2" name="Priorities"></ValueListReference></Sort></SortList></SortSpecification></Parameter></ParameterValues></Step>"#,
                "The value list “Priorities” is not in this file",
            ),
            (
                r#"<Step id="142"><Options>8</Options><ParameterValues><Parameter type="CustomMenuSetReference"><CustomMenuSetReference id="2" name="Minimal"></CustomMenuSetReference></Parameter></ParameterValues></Step>"#,
                "The menu set “Minimal” is not known by that name in this file",
            ),
        ] {
            match one(xml) {
                Pasted::Failed { reason, .. } => assert_eq!(reason, why),
                Pasted::Step(step) => panic!("{why}: read as {step:?}"),
            }
        }
    }

    #[test]
    fn shared_names_fail_the_step() {
        let names = Twins(Tasks::new());
        for xml in [
            r#"<Step id="1"><Options>64</Options><ParameterValues><Parameter type="List"><List name="From list" value="1"><ScriptReference id="7" name="Sort by Name"></ScriptReference></List></Parameter></ParameterValues></Step>"#,
            r#"<Step id="6"><Options>8</Options><ParameterValues><Parameter type="LayoutReferenceContainer"><LayoutReferenceContainer value="5"><LayoutReference id="3" name="Task Details"></LayoutReference></LayoutReferenceContainer></Parameter></ParameterValues></Step>"#,
        ] {
            match parse_xml_steps(xml, &names).unwrap().remove(0) {
                Pasted::Failed { reason, .. } => {
                    assert!(reason.contains("is ambiguous"), "{reason}")
                }
                Pasted::Step(step) => panic!("read as {step:?}"),
            }
        }
    }

    fn one(xml: &str) -> Pasted {
        let mut steps = parse_xml_steps(xml, &Tasks::new()).unwrap();
        assert_eq!(steps.len(), 1, "{steps:?}");
        steps.remove(0)
    }

    fn step(xml: &str) -> ScriptStep {
        match one(xml) {
            Pasted::Step(step) => step,
            Pasted::Failed { reason, .. } => panic!("{reason}"),
        }
    }

    fn calc(text: &str) -> crate::Calculation {
        compile(text, &Tasks::new()).unwrap()
    }

    /// FMDeveloperTool's Save as XML of Go to Object (Contacts template).
    const GO_TO_OBJECT: &str = r#"<Step hash="A472EABC05326AED273663AC19BE9D9B" index="0" id="145" name="Go to Object" enable="True"><UUID>05E2D270-6CC6-4A8E-AF68-733FA17F7657</UUID><SourceUUID>1D58DE02-B419-412A-8EC0-B93394F619CD</SourceUUID><OwnerID></OwnerID><Options>16384</Options><ParameterValues membercount="1"><Parameter type="Object"><Name><Calculation datatype="1" position="0"><Calculation><Text><![CDATA[Get ( ScriptParameter )]]></Text></Calculation></Calculation></Name><repetition></repetition></Parameter></ParameterValues></Step>"#;

    #[test]
    fn save_as_xml_steps_read_with_their_calculation_slots() {
        let go = step(GO_TO_OBJECT);
        assert_eq!((go.id(), go.flags, go.value), (145, flag::CALCULATION, 0));
        assert_eq!(go.calculation(0), Some(calc("Get ( ScriptParameter )")));
        assert!(go.is_enabled());
        // The data key and UUIDs are not taken: a pasted step is new.
        assert!(go.data_key.is_empty());
        assert_eq!(go.unique_id(), None);
    }

    #[test]
    fn snippets_give_every_step_in_document_order() {
        let snippet = r##"<?xml version="1.0" encoding="UTF-8"?>
<fmxmlsnippet type="FMObjectList">
  <Step index="0" id="141" name="Set Variable" enable="True"><Options>16388</Options>
    <ParameterValues membercount="1"><Parameter type="Variable"><value><Calculation datatype="1" position="1"><Calculation><Text><![CDATA[Tasks::Task & "¶"]]></Text></Calculation></Calculation></value><Name value="$$parent"></Name><repetition></repetition></Parameter></ParameterValues></Step>
  <Step index="1" id="89" name="# (comment)" enable="False"><Options>4</Options><ParameterValues membercount="1"><Parameter type="Comment"><Comment value="two&#13;lines &amp; &quot;more&quot;"></Comment></Parameter></ParameterValues></Step>
  <Step index="2" id="7" name="New Record/Request" enable="True"><Options>0</Options></Step>
</fmxmlsnippet>"##;
        let steps: Vec<ScriptStep> = parse_steps(snippet, &Tasks::new(), Origin::Unknown)
            .unwrap()
            .into_iter()
            .map(|p| match p {
                Pasted::Step(step) => step,
                Pasted::Failed { reason, .. } => panic!("{reason}"),
            })
            .collect();
        assert_eq!(
            steps.iter().map(ScriptStep::id).collect::<Vec<_>>(),
            [141, 89, 7]
        );
        assert_eq!(steps[0].text(0).as_deref(), Some("$$parent"));
        assert_eq!(steps[0].calculation(1), Some(calc("Tasks::Task & \"¶\"")));
        assert_eq!(steps[0].calculation(2), None);
        assert!(!steps[1].is_enabled());
        assert_eq!(steps[1].text(0).as_deref(), Some("two\rlines & \"more\""));
        // The step text's paste routes XML here.
        assert_eq!(
            super::super::parse_steps(snippet, &Tasks::new(), Origin::Unknown)
                .unwrap()
                .len(),
            3
        );
    }

    #[test]
    fn references_are_found_by_name_before_the_xml_ids() {
        // Another file's ids: the names find this file's field and layout.
        let set = step(
            r#"<Step id="76" enable="True"><Options>16385</Options><ParameterValues><Parameter type="FieldReference"><FieldReference id="9" name="Status"><repetition><Calculation datatype="1" position="10"><Calculation><Text><![CDATA[2]]></Text></Calculation></Calculation></repetition><TableOccurrenceReference id="1065111" name="Tasks"></TableOccurrenceReference></FieldReference></Parameter><Parameter type="Calculation"><Calculation datatype="1" position="0"><Calculation><Text><![CDATA["Done"]]></Text></Calculation></Calculation></Parameter></ParameterValues></Step>"#,
        );
        let spec = set.field_spec().unwrap();
        assert_eq!(
            (spec.table().id(), spec.field().id(), spec.repetition()),
            (Some(0xd00002), Some(4), 2)
        );
        assert_eq!(set.calculation(0), Some(calc("\"Done\"")));
        // A calculated repetition: spec repetition 0 and the calculation.
        let calculated = step(
            r#"<Step id="76"><Options>16385</Options><ParameterValues><Parameter type="FieldReference"><FieldReference id="1" name="Task"><repetition><Calculation position="10"><Calculation><Text><![CDATA[$i]]></Text></Calculation></Calculation></repetition><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(calculated.field_spec().unwrap().repetition(), 0);
        assert_eq!(calculated.calculation(10), Some(calc("$i")));
        let layout = step(
            r#"<Step id="6"><Options>8</Options><ParameterValues><Parameter type="LayoutReferenceContainer"><LayoutReferenceContainer value="5"><LayoutReference id="12" name="Task Details"></LayoutReference></LayoutReferenceContainer></Parameter><Parameter type="Animation"><Animation name="None" value="0"></Animation></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(layout.member_key().and_then(|k| k.id()), Some(3));
        assert_eq!((layout.value, layout.list_value(6)), (5, Some(0)));
    }

    #[test]
    fn script_choices_and_variable_targets_read() {
        let perform = step(
            r#"<Step id="1"><Options>16448</Options><ParameterValues><Parameter type="List"><List name="From list" value="1"><ScriptReference id="99" name="Sort by Name"></ScriptReference></List></Parameter><Parameter type="Parameter"><Parameter><Calculation position="0"><Calculation><Text><![CDATA[1]]></Text></Calculation></Calculation></Parameter></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(perform.script_id(), Some(7));
        assert_eq!(perform.calculation(0), Some(calc("1")));
        let by_name = step(
            r#"<Step id="1"><Options>33572864</Options><ParameterValues><Parameter type="List"><List name="By name" value="2"><Calculation position="2"><Calculation><Text><![CDATA["Sort by Name"]]></Text></Calculation></Calculation></List></Parameter><Parameter type="Parameter"><Parameter></Parameter></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(by_name.list_value(1), Some(1));
        assert_eq!(by_name.flags, 0x200_4800);
        let insert = step(
            r#"<Step id="61"><Options>4101</Options><ParameterValues><Parameter type="Boolean"><Boolean type="Select" id="4096" value="True"></Boolean></Parameter><Parameter type="Target"><Variable value="$v"><repetition><Calculation position="32"><Calculation><Text><![CDATA[$i]]></Text></Calculation></Calculation></repetition></Variable></Parameter><Parameter type="Text"><Text value="hello"></Text></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(insert.variable_name(0).as_deref(), Some("$v"));
        assert_eq!(insert.field_spec().unwrap().repetition(), 0);
        assert_eq!(insert.calculation(0x20), Some(calc("$i")));
        assert_eq!(insert.text(0).as_deref(), Some("hello"));
    }

    #[test]
    fn sort_orders_and_find_requests_are_encoded() {
        let sort = step(
            r#"<Step id="39"><Options>33587328</Options><ParameterValues><Parameter type="Boolean"><Boolean type="With dialog" id="128" value="False"></Boolean></Parameter><Parameter type="Restore"><Restore value="True"></Restore></Parameter><Parameter type="SortSpecification"><SortSpecification value="True" blanksLast="False" maintain="True"><SortList membercount="2"><Sort type="Ascending"><PrimaryField><FieldReference id="1" name="Task"><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></PrimaryField></Sort><Sort type="Custom"><PrimaryField><FieldReference id="4" name="Status"><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></PrimaryField><ValueListReference id="5" name="Status"></ValueListReference></Sort></SortList></SortSpecification></Parameter></ParameterValues></Step>"#,
        );
        let bytes = crate::node_bytes(sort.data_leaf(StepData::SORT_ORDER).unwrap()).unwrap();
        // FileMaker's form: no value list is the empty key `00`.
        assert_eq!(
            &bytes[..28],
            [
                0, 0, 0, 2, 4, 3, 0xd0, 0, 2, 2, 1, 1, 0, 1, 2, 1, 0, 2, 1, 0, 0, 1, 0, 0, 0, 0, 0,
                0
            ]
        );
        let spec = SortSpec::decode(&bytes).unwrap();
        assert_eq!(spec.fields[1].value_list, Some(2));
        let find = step(
            r#"<Step id="28"><Options>33587200</Options><ParameterValues><Parameter type="FindRequest"><FindRequestSet membercount="2"><FindRequest membercount="1" action="find" index="1"><find criteria="x &amp; &quot;y&quot;"><FieldReference id="1" name="Task" repetition="2"><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></find></FindRequest><FindRequest membercount="1" action="omit" index="2"><find criteria="==$name"><FieldReference id="1" name="First Name" repetition="1"><TableOccurrenceReference id="1065092" name="Assignees"></TableOccurrenceReference></FieldReference></find></FindRequest></FindRequestSet></Parameter></ParameterValues></Step>"#,
        );
        let requests = specs::requests(find.data_leaf(StepData::REQUESTS).unwrap()).unwrap();
        assert_eq!(requests.len(), 2);
        assert!(!requests[0].omit && requests[1].omit);
        assert_eq!(requests[0].criteria[0].0.repetition, 2);
        assert_eq!(requests[0].criteria[0].1, "x & \"y\"");
        assert_eq!(requests[1].criteria[0].0.occurrence, Some(0xd00004));
    }

    #[test]
    fn options_are_the_flags_and_booleans_stand_in_without_them() {
        let commit = r#"<Step id="75"><Options>384</Options><ParameterValues><Parameter type="Boolean"><Boolean type="Skip data entry validation" id="256" value="True"></Boolean></Parameter><Parameter type="Boolean"><Boolean type="With dialog" id="128" value="False"></Boolean></Parameter></ParameterValues></Step>"#;
        assert_eq!(step(commit).flags, 0x180);
        let without = commit.replace("<Options>384</Options>", "");
        assert_eq!(step(&without).flags, 0x180);
        // Convert File's "Open File" is True with kOption clear.
        let convert = r#"<Step id="139"><ParameterValues><Parameter type="Boolean"><Boolean type="Open File" id="256" value="False"></Boolean></Parameter></ParameterValues></Step>"#;
        assert_eq!(step(convert).flags, flag::OPTION);
        // The signed form of a flags word with the top bit.
        let close = r#"<Step id="121"><Options>-2147483648</Options><ParameterValues><Parameter type="WindowReference"><WindowReference><Select kind="0" type="current"></Select></WindowReference></Parameter></ParameterValues></Step>"#;
        assert_eq!(step(close).flags, flag::LIMIT_TO_CURRENT_FILE);
    }

    #[test]
    fn custom_dialogs_read_buttons_and_inputs() {
        let dialog = step(
            r#"<Step id="87"><Options>16389</Options><ParameterValues><Parameter type="Title"><Calculation position="0"><Calculation><Text><![CDATA["T"]]></Text></Calculation></Calculation></Parameter><Parameter type="Button1" value="OK"><Boolean type="Commit" value="True"></Boolean></Parameter><Parameter type="Button2"><Calculation position="6"><Calculation><Text><![CDATA["Two"]]></Text></Calculation></Calculation><Boolean type="Commit" value="False"></Boolean></Parameter><Parameter type="Button3"><Boolean type="Commit" value="False"></Boolean></Parameter><Parameter type="Field1"><Parameter type="Target"><FieldReference id="1" name="Task"><repetition><Calculation position="32"><Calculation><Text><![CDATA[1]]></Text></Calculation></Calculation></repetition><TableOccurrenceReference id="1065090" name="Tasks"></TableOccurrenceReference></FieldReference></Parameter><Boolean type="Password" value="True"></Boolean><Parameter type="Label"><Calculation position="2"><Calculation><Text><![CDATA["Label"]]></Text></Calculation></Calculation></Parameter></Parameter></ParameterValues></Step>"#,
        );
        assert_eq!(dialog.text(0).as_deref(), Some("OK"));
        assert_eq!(dialog.calculation(5), None);
        assert_eq!(dialog.calculation(6), Some(calc("\"Two\"")));
        assert_eq!(
            (3..6).map(|n| dialog.number(n)).collect::<Vec<_>>(),
            [Some(1), Some(0), Some(0)]
        );
        assert_eq!(dialog.number(0), Some(1));
        assert_eq!(dialog.field_spec().unwrap().field().id(), Some(1));
        assert_eq!(dialog.calculation(2), Some(calc("\"Label\"")));
    }

    #[test]
    fn what_does_not_map_fails_with_the_steps_xml() {
        let speak = r#"<Step index="3" id="66" name="Speak" enable="True"><Options>262</Options><ParameterValues membercount="1"><Parameter type="Voice"><Voice id="1" value="1"></Voice></Parameter></ParameterValues></Step>"#;
        let document =
            format!("<fmxmlsnippet type=\"FMObjectList\">{GO_TO_OBJECT}{speak}</fmxmlsnippet>");
        let pasted = parse_steps(&document, &Tasks::new(), Origin::Unknown).unwrap();
        assert!(matches!(&pasted[0], Pasted::Step(s) if s.id() == 145));
        let Pasted::Failed { text, reason } = &pasted[1] else {
            panic!("{:?}", pasted[1]);
        };
        assert_eq!(text, speak);
        assert!(reason.contains("Voice"), "{reason}");
        // A calculation that does not compile, an unknown attribute.
        let bad_calc = r#"<Step id="103"><Options>16384</Options><ParameterValues><Parameter type="Calculation"><Calculation position="0"><Calculation><Text><![CDATA[1 +]]></Text></Calculation></Calculation></Parameter></ParameterValues></Step>"#;
        assert!(matches!(one(bad_calc), Pasted::Failed { .. }));
        let unknown = GO_TO_OBJECT.replace("datatype=\"1\"", "datatype=\"1\" extra=\"x\"");
        assert!(matches!(one(&unknown), Pasted::Failed { reason, .. } if reason.contains("extra")));
        // Not XML, or no step: the whole text, once.
        for text in [
            "<fmxmlsnippet><Step id=\"7\">",
            "<fmxmlsnippet></fmxmlsnippet>",
        ] {
            let pasted = parse_steps(text, &Tasks::new(), Origin::Unknown).unwrap();
            assert!(
                matches!(&pasted[..], [Pasted::Failed { text: t, .. }] if t == text),
                "{pasted:?}"
            );
        }
    }

    #[test]
    fn the_reader_decodes_entities_cdata_and_nesting() {
        let text = "<?xml version=\"1.0\"?>\n<!-- c --><a x='1 &amp; 2' y=\"&#60;&#x3e;\">t&quot;<b/><c><![CDATA[<raw> & ]]></c></a>";
        let nodes = Document::parse(text).unwrap();
        let a = nodes
            .iter()
            .find_map(|n| match n {
                Node::Element(e) => Some(e),
                Node::Text(_) => None,
            })
            .unwrap();
        assert_eq!(a.attr("x"), Some("1 & 2"));
        assert_eq!(a.attr("y"), Some("<>"));
        assert_eq!(a.text(), "t\"");
        assert_eq!(a.child("c").unwrap().text(), "<raw> & ");
        assert_eq!(&text[a.child("b").unwrap().span.clone()], "<b/>");
        assert_eq!(&text[a.span.clone()], &text[text.find("<a").unwrap()..]);
        for bad in [
            "<a>",
            "<a></b>",
            "<a x=1></a>",
            "<a>&nope;</a>",
            "</a>",
            "<a x='1' x='2'/>",
        ] {
            assert!(Document::parse(bad).is_err(), "{bad}");
        }
    }
}
