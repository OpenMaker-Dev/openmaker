//! One script step: the 28-byte descriptor and its data subtree
//! (script-steps.md 2-4).

use crate::key::{FieldSpec, StoredKey};
use crate::schema::{Location, SlotKind};
use crate::value::{bytes_node, node_bytes, node_text, renamed, text_node};
use om_hbam::OwnedNode;
use std::collections::BTreeMap;

/// `ScriptStep::StepFlags` bits (script-steps.md 2.2).
pub mod flag {
    /// A field or variable target is in `mReserved`.
    pub const FIELD_SPEC: u32 = 0x1;
    /// `mStepValue` holds a list choice.
    pub const MENU_SELECTION: u32 = 0x2;
    pub const TEXT_DATA: u32 = 0x4;
    /// `mReserved` holds a layout or menu set key.
    pub const MEMBER_KEY: u32 = 0x8;
    pub const FILE_REFERENCE: u32 = 0x10;
    pub const FILE_PATH: u32 = 0x20;
    /// `mReserved` holds a script key.
    pub const SCRIPT_KEY: u32 = 0x40;
    /// "With dialog: Off".
    pub const NO_INTERACT: u32 = 0x80;
    pub const OPTION: u32 = 0x100;
    pub const OPTION2: u32 = 0x200;
    /// `kUNUSED1`, a third option bit of 26.x steps.
    pub const OPTION3: u32 = 0x400;
    pub const STORE_REFERENCE: u32 = 0x800;
    pub const SELECT_ALL: u32 = 0x1000;
    /// "Exit after last".
    pub const EXIT: u32 = 0x2000;
    /// The step has calculation slots.
    pub const CALCULATION: u32 = 0x4000;
    /// A spec subtree is stored (find, sort, import, export, print).
    pub const EXTENDED_DATA: u32 = 0x8000;
    pub const SET_ON_OFF: u32 = 0x1_0000;
    pub const TURN_ON: u32 = 0x2_0000;
    pub const LINK_AVAIL: u32 = 0x4_0000;
    pub const LOCK: u32 = 0x8_0000;
    pub const BREAKPOINT: u32 = 0x10_0000;
    pub const NOT_MAC_DEPRECATED: u32 = 0x20_0000;
    pub const NOT_WIN_DEPRECATED: u32 = 0x40_0000;
    pub const DISABLE_STEP_COLLAPSED: u32 = 0x80_0000;
    pub const PAUSE_SCRIPT: u32 = 0x100_0000;
    /// "Restore"; "Collapsed" for block steps.
    pub const RESTORE: u32 = 0x200_0000;
    pub const GO_TO_FIELD: u32 = 0x400_0000;
    pub const FIELD_SPEC_BY_CALC: u32 = 0x800_0000;
    pub const VERIFY_SSL_CERTIFICATES: u32 = 0x1000_0000;
    pub const OPEN_AUTOMATICALLY: u32 = 0x2000_0000;
    pub const CREATE_EMAIL: u32 = 0x4000_0000;
    pub const LIMIT_TO_CURRENT_FILE: u32 = 0x8000_0000;
    /// Bits any step may carry: a debugger breakpoint and the collapsed
    /// display of a disabled step.
    pub const EDITOR: u32 = BREAKPOINT | DISABLE_STEP_COLLAPSED;
}

/// A stored calculation (calc-encoding.md 1): the streams as stored, for
/// `om-formula` to decode. Each is the reassembled value of its child.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Calculation {
    /// `04`: the context table occurrence key; `01 00` (none) for steps.
    pub context: Option<Vec<u8>>,
    /// `05`: the formatted stream, absent when the text was stripped.
    pub formatted: Option<Vec<u8>>,
    /// `06`: the Polish stream, the one FileMaker evaluates.
    pub polish: Option<Vec<u8>>,
}

impl Calculation {
    /// The context of every script step calculation: occurrence 0.
    pub const NO_CONTEXT: [u8; 2] = [1, 0];

    /// A step calculation from its streams.
    pub fn new(formatted: Option<Vec<u8>>, polish: Vec<u8>) -> Self {
        Self {
            context: Some(Self::NO_CONTEXT.to_vec()),
            formatted,
            polish: Some(polish),
        }
    }

    /// Decode a `KFormula` node. Children other than `04`-`06` are left to
    /// the stored node (and reported by [`crate::ScriptStep::unclassified`]).
    pub fn decode(node: &OwnedNode) -> Option<Self> {
        if node.value.is_some() {
            return None;
        }
        let stream = |key: u8| {
            node.child(&[key])
                .and_then(node_bytes)
                .map(|b| b.into_owned())
        };
        Some(Self {
            context: stream(0x04),
            formatted: stream(0x05),
            polish: stream(0x06),
        })
    }

    /// The `KFormula` node, written as FileMaker writes values (chunked
    /// above 1000 bytes).
    pub fn encode(&self, segment: &[u8]) -> OwnedNode {
        let children = [
            (0x04, &self.context),
            (0x05, &self.formatted),
            (0x06, &self.polish),
        ]
        .into_iter()
        .filter_map(|(key, stream)| stream.as_ref().map(|s| bytes_node(&[key], s)))
        .collect();
        OwnedNode::list(segment, children)
    }

    /// The stream to evaluate: Polish, or the formatted stream when that is
    /// all that is stored.
    pub fn stream(&self) -> Option<&[u8]> {
        self.polish.as_deref().or(self.formatted.as_deref())
    }
}

/// One slot node `[00 n]` of a step's data (script-steps.md 4.2). Children
/// stay as stored; the accessors decode them.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Slot {
    /// The slot node's own value: a list choice (u32) when it is a leaf.
    value: Option<Vec<u8>>,
    push: bool,
    /// One-byte children: `01` text, `03` numeric data, `05` calculation,
    /// `10` field spec, `11` table occurrences, `82` path list, or unknown.
    children: BTreeMap<u8, OwnedNode>,
    /// Children with longer segments (never seen), verbatim.
    other: Vec<OwnedNode>,
}

impl Slot {
    pub(crate) fn decode(node: &OwnedNode) -> Self {
        let mut slot = Slot {
            value: node.value.clone(),
            push: node.push,
            ..Slot::default()
        };
        for child in &node.children {
            match child.segment.as_slice() {
                [key] => {
                    slot.children.insert(*key, child.clone());
                }
                _ => slot.other.push(child.clone()),
            }
        }
        slot
    }

    pub(crate) fn encode(&self, slot: u16) -> OwnedNode {
        let mut children: Vec<OwnedNode> = self.children.values().cloned().collect();
        children.extend(self.other.iter().cloned());
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        OwnedNode {
            segment: slot.to_be_bytes().to_vec(),
            value: self.value.clone(),
            push: self.push || !children.is_empty(),
            children,
        }
    }

    fn is_empty(&self) -> bool {
        self.value.is_none() && self.children.is_empty() && self.other.is_empty()
    }

    /// The slot node's own value.
    pub fn value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// The child stored under a one-byte key.
    pub fn child(&self, key: u8) -> Option<&OwnedNode> {
        self.children.get(&key)
    }

    /// The one-byte child keys, in key order.
    pub fn child_keys(&self) -> impl Iterator<Item = u8> + '_ {
        self.children.keys().copied()
    }

    /// Children with segments longer than one byte.
    pub fn other(&self) -> &[OwnedNode] {
        &self.other
    }
}

/// The step's data subtree `[17, 5, S, 5, K]`: slot nodes, step-level keys
/// (`06` find requests, `0a` sort order, `20` file references, `82` paths,
/// `83` blob, `84`-`88` import, export and print specs, `d8`/`d9` UUIDs, ...)
/// and anything else, all kept as stored so the subtree re-encodes exactly.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StepData {
    slots: BTreeMap<u16, Slot>,
    keys: BTreeMap<u8, OwnedNode>,
    other: Vec<OwnedNode>,
    value: Option<Vec<u8>>,
    push: bool,
}

impl StepData {
    pub const REQUESTS: u8 = 0x06;
    pub const SORT_ORDER: u8 = 0x0a;
    pub const FILE_REFERENCES: u8 = 0x20;
    pub const PATH_STRING: u8 = 0x82;
    pub const BLOB: u8 = 0x83;
    pub const IMPORT_ORDER: u8 = 0x84;
    pub const EXPORT_ORDER: u8 = 0x85;
    pub const PROFILE: u8 = 0x86;
    pub const PAGE_FORMAT: u8 = 0x87;
    pub const PRINT_SETTINGS: u8 = 0x88;
    pub const FILE_FILTER_SPEC: u8 = 0x8c;
    pub const OWNER_ID: u8 = 0xd7;
    pub const UNIQUE_ID: u8 = 0xd8;
    pub const SOURCE_UUID: u8 = 0xd9;
    pub const TAGS: u8 = 0xda;

    /// Step-level keys every step may carry (catalog item bookkeeping).
    pub const ITEM_KEYS: [u8; 4] = [
        Self::OWNER_ID,
        Self::UNIQUE_ID,
        Self::SOURCE_UUID,
        Self::TAGS,
    ];

    pub fn decode(node: &OwnedNode) -> Self {
        let mut data = StepData {
            value: node.value.clone(),
            push: node.push,
            ..StepData::default()
        };
        for child in &node.children {
            match child.segment.as_slice() {
                [hi, lo] => {
                    data.slots
                        .insert(u16::from_be_bytes([*hi, *lo]), Slot::decode(child));
                }
                [key] => {
                    data.keys.insert(*key, child.clone());
                }
                _ => data.other.push(child.clone()),
            }
        }
        data
    }

    /// The subtree under the step's data key segment.
    pub fn encode(&self, segment: &[u8]) -> OwnedNode {
        let mut children: Vec<OwnedNode> = self
            .slots
            .iter()
            .map(|(n, slot)| slot.encode(*n))
            .chain(self.keys.values().cloned())
            .chain(self.other.iter().cloned())
            .collect();
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        OwnedNode {
            segment: segment.to_vec(),
            value: self.value.clone(),
            push: self.push || !children.is_empty(),
            children,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
            && self.keys.is_empty()
            && self.other.is_empty()
            && self.value.is_none()
    }

    pub fn slot(&self, n: u16) -> Option<&Slot> {
        self.slots.get(&n)
    }

    /// Slot numbers and slots, in key order.
    pub fn slots(&self) -> impl Iterator<Item = (u16, &Slot)> {
        self.slots.iter().map(|(n, s)| (*n, s))
    }

    /// A step-level key's node.
    pub fn key(&self, key: u8) -> Option<&OwnedNode> {
        self.keys.get(&key)
    }

    /// The step-level keys present, in key order.
    pub fn keys(&self) -> impl Iterator<Item = u8> + '_ {
        self.keys.keys().copied()
    }

    /// Children that are neither slots nor one-byte keys.
    pub fn other(&self) -> &[OwnedNode] {
        &self.other
    }

    /// A value on the step node itself (never seen; kept).
    pub fn node_value(&self) -> Option<&[u8]> {
        self.value.as_deref()
    }

    /// Replace or remove slot `n` with a stored slot node (the step text's
    /// `Stored slot` option).
    pub(crate) fn set_slot_node(&mut self, n: u16, node: Option<&OwnedNode>) {
        match node {
            Some(node) => {
                self.slots.insert(n, Slot::decode(node));
            }
            None => {
                self.slots.remove(&n);
            }
        }
    }

    /// Replace or remove a step-level key's node as stored.
    pub(crate) fn set_key_node(&mut self, key: u8, node: Option<OwnedNode>) {
        match node {
            Some(node) => {
                self.keys.insert(key, node);
            }
            None => {
                self.keys.remove(&key);
            }
        }
    }

    /// The flags the present data implies (script-steps.md 6).
    fn implied_flags(&self) -> u32 {
        let slots = self.slots.iter().flat_map(|(n, slot)| {
            slot.children.keys().filter_map(move |key| {
                SlotKind::from_child(*key).map(|kind| Location::Slot { slot: *n, kind })
            })
        });
        let keys = self
            .keys
            .keys()
            .filter(|k| !Self::ITEM_KEYS.contains(k))
            .map(|k| Location::Key(*k));
        slots.chain(keys).fold(0, |acc, l| acc | l.flags())
    }
}

/// A script step: the 28-byte descriptor `Draco::ScriptStep` (big-endian on
/// disk) and its data subtree.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptStep {
    /// `mDataKey` (bytes 0-5): names the step's data subtree. Filler kept.
    pub data_key: StoredKey,
    /// `mReserved` (bytes 6-19): an inline reference, valid only while its
    /// flag (`FIELD_SPEC`, `MEMBER_KEY`, `SCRIPT_KEY`, ...) is set; stale
    /// bytes are kept.
    pub reserved: [u8; 14],
    /// `mStepID` (bytes 20-21): the step id; negative when disabled.
    pub opcode: i16,
    /// `mStepValue` (bytes 22-23): a list choice or a cached step index.
    pub value: u16,
    /// `mStepFlags` (bytes 24-27): [`flag`] bits.
    pub flags: u32,
    /// The data subtree; `None` when the file has none for this step.
    pub data: Option<StepData>,
}

/// The variable name of target `n` is text in slot `0x20 + n` (corpus).
pub const VARIABLE_NAME_SLOT: u16 = 0x20;

impl ScriptStep {
    /// An enabled step with no parameters and no data key yet.
    pub fn new(id: u16) -> Self {
        Self {
            opcode: id as i16,
            ..Self::default()
        }
    }

    /// A layout object's single step (`LayoutObject.proto` `ScriptStep`:
    /// field 2 is `mStepValue`, field 3 `mStepFlags`, field 4 `mReserved`).
    /// Carries the object's separately stored parameters into the same
    /// slot decoder used for script-catalog steps. `None` when the id or
    /// value does not fit the descriptor.
    pub fn from_layout(step: &om_layout::ScriptStep) -> Option<Self> {
        let mut reserved = [0; 14];
        let len = step.reserved.len().min(14);
        reserved[..len].copy_from_slice(&step.reserved[..len]);
        Some(Self {
            data_key: StoredKey::EMPTY,
            reserved,
            opcode: step.id.try_into().ok()?,
            value: step.value.try_into().ok()?,
            flags: step.flags as u32,
            data: step.data.as_ref().map(StepData::decode),
        })
    }

    pub fn from_descriptor(bytes: &[u8; 28]) -> Self {
        Self {
            data_key: StoredKey::from_bytes(&bytes[..6]).expect("six bytes"),
            reserved: bytes[6..20].try_into().expect("fourteen bytes"),
            opcode: i16::from_be_bytes([bytes[20], bytes[21]]),
            value: u16::from_be_bytes([bytes[22], bytes[23]]),
            flags: u32::from_be_bytes(bytes[24..].try_into().expect("four bytes")),
            data: None,
        }
    }

    /// The 28 descriptor bytes, as `ScriptStep::Write` streams them.
    pub fn descriptor(&self) -> [u8; 28] {
        let mut bytes = [0; 28];
        bytes[..6].copy_from_slice(&self.data_key.0);
        bytes[6..20].copy_from_slice(&self.reserved);
        bytes[20..22].copy_from_slice(&self.opcode.to_be_bytes());
        bytes[22..24].copy_from_slice(&self.value.to_be_bytes());
        bytes[24..].copy_from_slice(&self.flags.to_be_bytes());
        bytes
    }

    /// The step id (catalogs/script-steps.csv), enabled or not.
    pub fn id(&self) -> u16 {
        self.opcode.unsigned_abs()
    }

    pub fn is_enabled(&self) -> bool {
        self.opcode >= 0
    }

    /// Enable or disable the step (`SetStepEnabled` negates the id). Use
    /// [`crate::Script::set_steps_enabled`] to keep cached indexes right.
    pub fn set_enabled(&mut self, enabled: bool) {
        let id = self.opcode.saturating_abs();
        self.opcode = if enabled { id } else { -id };
    }

    pub fn name(&self) -> String {
        let id = self.id();
        crate::step_name(id)
            .map(str::to_owned)
            .unwrap_or_else(|| format!("Unsupported step ({id})"))
    }

    /// The data key as a numeric id (`None` for an empty key).
    pub fn key(&self) -> Option<u64> {
        self.data_key.id()
    }

    /// Whether all of `bits` are set.
    pub fn has_flag(&self, bits: u32) -> bool {
        self.flags & bits == bits
    }

    pub fn set_flag(&mut self, bits: u32, on: bool) {
        if on {
            self.flags |= bits;
        } else {
            self.flags &= !bits;
        }
    }

    /// The inline field (or variable) target, when `FIELD_SPEC` is set.
    pub fn field_spec(&self) -> Option<FieldSpec> {
        self.has_flag(flag::FIELD_SPEC)
            .then_some(FieldSpec(self.reserved))
    }

    /// The inline layout or menu set key, when `MEMBER_KEY` is set.
    pub fn member_key(&self) -> Option<StoredKey> {
        self.has_flag(flag::MEMBER_KEY).then(|| self.reserved_key())
    }

    /// The inline script key, when `SCRIPT_KEY` is set.
    pub fn script_key(&self) -> Option<StoredKey> {
        self.has_flag(flag::SCRIPT_KEY).then(|| self.reserved_key())
    }

    fn reserved_key(&self) -> StoredKey {
        StoredKey::from_bytes(&self.reserved[..6]).expect("six bytes")
    }

    /// The inline reference read as a local catalog id, whatever the flags
    /// say. Only `1 + length` bytes of the key are read (script-steps.md 2.1).
    pub fn target_id(&self) -> Option<u64> {
        self.reserved_key().id()
    }

    /// The script the step names (its inline script key) as a script id,
    /// the serial decode [`crate::Script::id`] uses.
    pub fn script_id(&self) -> Option<u64> {
        self.script_key()?.serial_id()
    }

    /// The inline reference read as a script id, whatever the flags say (a
    /// layout button's Perform Script).
    pub fn target_script_id(&self) -> Option<u64> {
        self.reserved_key().serial_id()
    }

    fn slot_child(&self, slot: u16, key: u8) -> Option<&OwnedNode> {
        self.data.as_ref()?.slot(slot)?.child(key)
    }

    /// The calculation in a slot.
    pub fn calculation(&self, slot: u16) -> Option<Calculation> {
        Calculation::decode(self.slot_child(slot, 0x05)?)
    }

    /// The text in a slot.
    pub fn text(&self, slot: u16) -> Option<String> {
        node_text(self.slot_child(slot, 0x01)?)
    }

    /// The numeric data in a slot, signed by its stored width (`ffffffff` = -1).
    pub fn number(&self, slot: u16) -> Option<i64> {
        let bytes = node_bytes(self.slot_child(slot, 0x03)?)?;
        Some(match *bytes {
            [b] => i64::from(b),
            [a, b] => i64::from(i16::from_be_bytes([a, b])),
            [a, b, c, d] => i64::from(i32::from_be_bytes([a, b, c, d])),
            [a, b, c, d, e, f, g, h] => i64::from_be_bytes([a, b, c, d, e, f, g, h]),
            _ => return None,
        })
    }

    /// The list choice stored as a slot node's own value.
    pub fn list_value(&self, slot: u16) -> Option<u32> {
        let bytes = self.data.as_ref()?.slot(slot)?.value()?;
        om_hbam::slot::be_uint(bytes).and_then(|v| u32::try_from(v).ok())
    }

    /// The field spec of slot `n`: `mReserved` for slot 0 (with
    /// `FIELD_SPEC`), `[00 n, 10]` otherwise.
    pub fn slot_field_spec(&self, slot: u16) -> Option<FieldSpec> {
        if slot == 0 {
            return self.field_spec();
        }
        FieldSpec::from_bytes(&node_bytes(self.slot_child(slot, 0x10)?)?)
    }

    /// The variable name of target `slot`, when that target is a variable.
    pub fn variable_name(&self, slot: u16) -> Option<String> {
        self.slot_field_spec(slot).filter(FieldSpec::is_variable)?;
        self.text(VARIABLE_NAME_SLOT + slot)
    }

    /// The node of a step-level key (`06`, `0a`, `20`, ... `88`).
    pub fn data_leaf(&self, key: u8) -> Option<&OwnedNode> {
        self.data.as_ref()?.key(key)
    }

    /// The step's `KUniqueID` (`d8`).
    pub fn unique_id(&self) -> Option<[u8; 16]> {
        node_bytes(self.data_leaf(StepData::UNIQUE_ID)?)?
            .as_ref()
            .try_into()
            .ok()
    }

    /// The id a calculation stores as a key "with its position"
    /// (`SetScriptKeyWithStorePosition`, script-steps.md 12.2): the
    /// formula is a quoted text literal of the key segment in hex, `"08"`
    /// for script 8, `"01"` for data source 1. Either case of the digits
    /// a-f is read. `None` when the slot holds no such literal.
    pub fn positioned_id(&self, slot: u16) -> Option<u64> {
        let polish = self.calculation(slot)?.polish?;
        let om_formula::Expr::Text(text) = om_formula::polish::decode(&polish).ok()? else {
            return None;
        };
        let hex = text.value.as_bytes();
        // A key segment is at most 5 bytes (`HBKey<5>`).
        if hex.is_empty()
            || hex.len() % 2 != 0
            || hex.len() > 10
            || !hex.iter().all(u8::is_ascii_hexdigit)
        {
            return None;
        }
        let digit = |d: u8| (d as char).to_digit(16).expect("checked") as u8;
        let segment: Vec<u8> = hex
            .chunks(2)
            .map(|pair| digit(pair[0]) << 4 | digit(pair[1]))
            .collect();
        om_hbam::serial::decode(&segment)
    }

    /// A calculation parameter as the editor shows it when it is a text
    /// literal or `Get ( ScriptParameter )`; `None` for other formulas.
    pub fn argument_text(&self, slot: u16) -> Option<String> {
        /// `Get ( ScriptParameter )` (catalogs/get-selectors.csv).
        const SCRIPT_PARAMETER: u8 = 62;
        match om_formula::polish::decode(self.calculation(slot)?.polish.as_deref()?).ok()? {
            om_formula::Expr::Text(text) => Some(text.value),
            om_formula::Expr::Get(SCRIPT_PARAMETER) => Some("Get ( ScriptParameter )".into()),
            _ => None,
        }
    }

    pub(crate) fn data_mut(&mut self) -> &mut StepData {
        self.data.get_or_insert_with(|| StepData {
            push: true,
            ..StepData::default()
        })
    }

    fn slot_mut(&mut self, slot: u16) -> &mut Slot {
        self.data_mut().slots.entry(slot).or_insert_with(|| Slot {
            push: true,
            ..Slot::default()
        })
    }

    /// Drop empty slots and an empty subtree after a removal.
    fn prune(&mut self, slot: Option<u16>) {
        if let Some(data) = &mut self.data {
            if let Some(n) = slot {
                if data.slots.get(&n).is_some_and(Slot::is_empty) {
                    data.slots.remove(&n);
                }
            }
            if data.is_empty() {
                self.data = None;
            }
        }
    }

    /// Clear `bits` unless the remaining data still implies them.
    fn clear_unless_implied(&mut self, bits: u32) {
        let implied = self.data.as_ref().map_or(0, StepData::implied_flags);
        self.flags &= !(bits & !implied);
    }

    /// Set or clear the child `key` of slot `slot` (`[00 slot, key]`), such
    /// as Go to Related Record's occurrence at `[00 0, 11]`.
    pub fn set_slot_child(&mut self, slot: u16, key: u8, node: Option<OwnedNode>) {
        match node {
            Some(node) => {
                self.slot_mut(slot)
                    .children
                    .insert(key, renamed(&node, &[key]));
            }
            None => {
                if let Some(data) = &mut self.data {
                    if let Some(s) = data.slots.get_mut(&slot) {
                        s.children.remove(&key);
                    }
                }
                self.prune(Some(slot));
            }
        }
    }

    /// Set or clear the field (or variable) spec of slot `n`; slot 0 is
    /// `mReserved` with `FIELD_SPEC`, whose stale bytes stay when cleared.
    pub fn set_field_spec(&mut self, slot: u16, spec: Option<FieldSpec>) {
        if slot == 0 {
            if let Some(spec) = spec {
                self.reserved = spec.0;
            }
            self.set_flag(flag::FIELD_SPEC, spec.is_some());
        } else {
            self.set_slot_child(slot, 0x10, spec.map(|s| OwnedNode::leaf([0x10], s.0)));
        }
    }

    /// Target a variable in slot `n`: a variable field spec and its name.
    pub fn set_variable_target(&mut self, slot: u16, name: &str) {
        let repetition = self
            .slot_field_spec(slot)
            .filter(FieldSpec::is_variable)
            .map_or(1, |s| s.repetition());
        self.set_field_spec(slot, Some(FieldSpec::variable(repetition)));
        self.set_text(VARIABLE_NAME_SLOT + slot, Some(name));
    }

    fn set_reserved_key(&mut self, bit: u32, key: Option<StoredKey>) {
        if let Some(key) = key {
            self.reserved[..6].copy_from_slice(&key.0);
        }
        self.set_flag(bit, key.is_some());
    }

    /// Set or clear the inline layout or menu set key (`MEMBER_KEY`).
    pub fn set_member_key(&mut self, key: Option<StoredKey>) {
        self.set_reserved_key(flag::MEMBER_KEY, key);
    }

    /// Set or clear the inline script key (`SCRIPT_KEY`).
    pub fn set_script_key(&mut self, key: Option<StoredKey>) {
        self.set_reserved_key(flag::SCRIPT_KEY, key);
    }

    /// Set or clear a slot's calculation, maintaining `CALCULATION`.
    pub fn set_calculation(&mut self, slot: u16, calculation: Option<&Calculation>) {
        let node = calculation.map(|c| c.encode(&[0x05]));
        let set = node.is_some();
        self.set_slot_child(slot, 0x05, node);
        if set {
            self.flags |= flag::CALCULATION;
        } else {
            self.clear_unless_implied(flag::CALCULATION);
        }
    }

    /// Set or clear a slot's text, maintaining `TEXT_DATA`; empty text
    /// clears it, as `SetTextData` does.
    pub fn set_text(&mut self, slot: u16, text: Option<&str>) {
        let node = text
            .filter(|t| !t.is_empty())
            .map(|t| text_node(&[0x01], t));
        let set = node.is_some();
        self.set_slot_child(slot, 0x01, node);
        if set {
            self.flags |= flag::TEXT_DATA;
        } else {
            self.clear_unless_implied(flag::TEXT_DATA);
        }
    }

    /// Set or clear a slot's numeric data as a 4-byte big-endian integer.
    pub fn set_number(&mut self, slot: u16, value: Option<i32>) {
        let node = value.map(|v| OwnedNode::leaf([0x03], v.to_be_bytes()));
        self.set_slot_child(slot, 0x03, node);
    }

    /// Set or clear a Boolean stored as one-byte numeric data.
    pub fn set_number_bool(&mut self, slot: u16, value: Option<bool>) {
        let node = value.map(|v| OwnedNode::leaf([0x03], [u8::from(v)]));
        self.set_slot_child(slot, 0x03, node);
    }

    /// Set or clear a slot's numeric data as a 2-byte big-endian integer,
    /// the width FileMaker stores for some option slots (script-steps.md
    /// 11.7).
    pub fn set_number_u16(&mut self, slot: u16, value: Option<u16>) {
        let node = value.map(|v| OwnedNode::leaf([0x03], v.to_be_bytes()));
        self.set_slot_child(slot, 0x03, node);
    }

    /// Set or clear the list choice stored as the slot node's value.
    pub fn set_list_value(&mut self, slot: u16, value: Option<u32>) {
        match value {
            Some(v) => {
                let s = self.slot_mut(slot);
                s.value = Some(v.to_be_bytes().to_vec());
                s.push = !s.children.is_empty();
            }
            None => {
                if let Some(s) = self.data.as_mut().and_then(|d| d.slots.get_mut(&slot)) {
                    s.value = None;
                }
                self.prune(Some(slot));
            }
        }
    }

    /// Set or clear a step-level key's subtree (find requests, sort order,
    /// file references, paths, specs), maintaining the flag that marks it.
    pub fn set_data_leaf(&mut self, key: u8, node: Option<OwnedNode>) {
        let bits = Location::Key(key).flags();
        match node {
            Some(node) => {
                self.data_mut().keys.insert(key, renamed(&node, &[key]));
                if !StepData::ITEM_KEYS.contains(&key) {
                    self.flags |= bits;
                }
            }
            None => {
                if let Some(data) = &mut self.data {
                    data.keys.remove(&key);
                }
                self.prune(None);
                if !StepData::ITEM_KEYS.contains(&key) {
                    self.clear_unless_implied(bits);
                }
            }
        }
    }

    /// Store the step's `KUniqueID` (`d8`) and `KSourceUUID` (`d9`).
    pub fn set_unique_ids(&mut self, unique: [u8; 16], source: Option<[u8; 16]>) {
        self.set_data_leaf(StepData::UNIQUE_ID, Some(OwnedNode::leaf([0xd8], unique)));
        if let Some(source) = source {
            self.set_data_leaf(StepData::SOURCE_UUID, Some(OwnedNode::leaf([0xd9], source)));
        }
    }

    /// The step's data subtree as stored under `[17, 5, S, 5]`, keyed by
    /// its data key segment.
    pub fn data_node(&self) -> Option<OwnedNode> {
        Some(self.data.as_ref()?.encode(self.data_key.segment()?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_descriptors_keep_parameters_for_every_step_kind() {
        for id in [6, 141, 147, 87] {
            let raw = OwnedNode::list(
                [17],
                vec![
                    OwnedNode::list(
                        [0, 5],
                        vec![Calculation::new(None, vec![0x13, 1, b'2' ^ 0x5a, 0]).encode(&[5])],
                    ),
                    OwnedNode::leaf([0, 6], 2u32.to_be_bytes()),
                    OwnedNode::leaf([0x8c], [9, 8, 7]),
                ],
            );
            let native = om_layout::ScriptStep {
                id,
                value: 4,
                flags: 2,
                data: Some(raw.clone()),
                ..Default::default()
            };
            let step = ScriptStep::from_layout(&native).unwrap();
            assert_eq!(
                step.calculation(5).unwrap().stream(),
                Some(&[0x13, 1, b'2' ^ 0x5a, 0][..])
            );
            assert_eq!(step.list_value(6), Some(2));
            assert_eq!(step.data.unwrap().encode(&[17]), raw);
        }
        let descriptor_only = om_layout::ScriptStep {
            id: 7,
            ..Default::default()
        };
        assert!(ScriptStep::from_layout(&descriptor_only)
            .unwrap()
            .data
            .is_none());
    }

    #[test]
    fn descriptor_names_match_the_dwarf_layout() {
        // Bytes 22-23 are mStepValue, 24-27 mStepFlags (script-steps.md 2).
        let mut bytes = [0u8; 28];
        bytes[..6].copy_from_slice(&[2, 1, 6, 0, 0x6f, 0]);
        bytes[6..10].copy_from_slice(&[2, 1, 3, 0x00]);
        bytes[20..].copy_from_slice(&[0, 6, 0, 5, 0, 0, 0, 8]);
        let step = ScriptStep::from_descriptor(&bytes);
        assert_eq!(
            (step.id(), step.value, step.flags),
            (6, 5, flag::MEMBER_KEY)
        );
        assert_eq!(step.key(), Some(6));
        assert_eq!(step.member_key().and_then(|k| k.id()), Some(3));
        assert_eq!(step.target_id(), Some(3));
        assert_eq!(step.descriptor(), bytes);
    }

    #[test]
    fn setters_maintain_their_flags() {
        let mut step = ScriptStep::new(141);
        step.set_text(0, Some("$x"));
        let calc = Calculation::new(None, vec![0x13, 1, b'1' ^ 0x5a, 0]);
        step.set_calculation(1, Some(&calc));
        assert!(step.has_flag(flag::TEXT_DATA | flag::CALCULATION));
        assert_eq!(step.text(0).as_deref(), Some("$x"));
        assert_eq!(step.calculation(1), Some(calc));
        step.set_calculation(1, None);
        assert!(!step.has_flag(flag::CALCULATION));
        assert!(step.has_flag(flag::TEXT_DATA));
        step.set_text(0, Some(""));
        assert_eq!(step.flags, 0);
        assert_eq!(step.data, None);

        step.set_variable_target(0, "$target");
        assert!(step.field_spec().unwrap().is_variable());
        assert_eq!(step.variable_name(0).as_deref(), Some("$target"));
        step.set_data_leaf(StepData::SORT_ORDER, Some(OwnedNode::leaf([0], [1, 2])));
        assert!(step.has_flag(flag::EXTENDED_DATA));
        step.set_data_leaf(StepData::SORT_ORDER, None);
        assert!(!step.has_flag(flag::EXTENDED_DATA));
    }

    #[test]
    fn data_subtrees_reencode_as_stored() {
        // A slot that is an empty push, a list leaf, an unknown slot child
        // and an unknown step-level key all survive.
        let node = OwnedNode::list(
            [7],
            vec![
                OwnedNode::list([0, 0], vec![]),
                OwnedNode::list(
                    [0, 1],
                    vec![OwnedNode::leaf([0x03], [0]), OwnedNode::leaf([0x77], [1])],
                ),
                OwnedNode::leaf([0, 6], [0, 0, 0, 2]),
                OwnedNode::leaf([0x8c], [9]),
                OwnedNode::leaf([0xd8], [0; 16]),
            ],
        );
        let data = StepData::decode(&node);
        assert_eq!(data.encode(&[7]), node);
        let step = ScriptStep {
            data: Some(data),
            data_key: StoredKey::from_segment(&[7]).unwrap(),
            ..ScriptStep::new(6)
        };
        assert_eq!(step.list_value(6), Some(2));
        assert_eq!(step.number(1), Some(0));
        assert_eq!(step.data_node(), Some(node));
    }
}
