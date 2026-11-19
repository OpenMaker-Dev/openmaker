//! Script triggers of a layout and of its objects (file-metadata.md 7):
//! the `KScriptTriggers` (`0x30`) cache of the layout `4/5/<layout>/30`, of
//! an object `4/5/<layout>/5/<object>/30`, and of an object of the layout's
//! table view `4/5/<layout>/0d/5/<object>/30`.
//!
//! ```text
//! 30/04                 u16 count of triggers
//! 30/05/<event>/01      the record (ScriptTrigger::Read), variable length:
//!   u16       event id (script-runtime.md 7.1); a stored 108 reads as 110
//!   key       the script, a serial id: [u8 n][u8 seglen][segment]
//!   key       the script's file reference; 00 = this file
//!   u32       modes: 1 Browse, 2 Find, 4 Preview
//!   u32       option, 0 in every record
//!   u16+bytes extra bytes, none in every record (fewer than 1000)
//! 30/05/<event>/31      the parameter calculation: 04 context, 05
//!                       formatted, 06 Polish (calc-encoding.md 1)
//! ```
//!
//! The common 16-byte record is `0065 020103 00 00000001 00000000 0000`
//! (OnLayoutEnter, script 3, this file, Browse); a script id of 128 or more
//! makes it 17 bytes.

use om_formula::Formula;

/// A stored calculation's streams: child 4 (context) and child 6 (Polish).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredFormula {
    pub context: Option<Vec<u8>>,
    pub polish: Vec<u8>,
}

/// One trigger record.
#[derive(Clone, Debug, PartialEq)]
pub struct ScriptTrigger {
    /// The event id (`TriggerEventID`).
    pub event: u16,
    /// The script's id (the serial decode of its key, as `Script::id`);
    /// `None` for an empty key.
    pub script: Option<u64>,
    /// The file reference the script is in; `None` for this file.
    pub file_reference: Option<u64>,
    /// The script lives in another file (`file_reference` is set).
    pub external: bool,
    /// Enabled modes: 1 Browse, 2 Find, 4 Preview.
    pub modes: u32,
    /// `mOption1`, 0 in every observed record.
    pub option: u32,
    /// `mExtraBytes`, empty in every observed record.
    pub extra: Vec<u8>,
    /// The script parameter calculation, when one is stored and decodes.
    pub parameter: Option<StoredFormula>,
}

/// Child keys of the cache and of an event's member.
const ENTRIES: u64 = 5;
const RECORD: u64 = 1;
const PARAMETER: u64 = 0x31;
/// The calculation children of a stored formula.
const CONTEXT: u64 = 4;
const POLISH: u64 = 6;

/// The old view-change event id, read as OnViewChange.
const OLD_VIEW_CHANGE: u16 = 108;
const VIEW_CHANGE: u16 = 110;
/// `ScriptTrigger::Read` refuses this many extra bytes or more.
const EXTRA_LIMIT: u16 = 1000;

/// Decode a `KScriptTriggers` cache. Records that do not decode are
/// skipped.
pub fn decode(node: &om_hbam::Node<'_>) -> Vec<ScriptTrigger> {
    let Some(entries) = node.get(ENTRIES) else {
        return Vec::new();
    };
    entries
        .children
        .iter()
        .filter_map(|entry| {
            let bytes = entry.get(RECORD)?.assembled()?;
            let mut trigger = decode_record(&bytes)?;
            trigger.parameter = entry.get(PARAMETER).and_then(formula);
            Some(trigger)
        })
        .collect()
}

/// A big-endian reader over the record.
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let out = self.bytes.get(self.at..self.at + n)?;
        self.at += n;
        Some(out)
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// A key `[u8 n][segments]` as the serial id of its first segment;
    /// `None` inside for an empty key (`00`, or the segment `00`).
    fn key(&mut self) -> Option<Option<u64>> {
        let n = usize::from(*self.take(1)?.first()?);
        let key = self.take(n)?;
        let Some((&len, rest)) = key.split_first() else {
            return Some(None);
        };
        let segment = rest.get(..usize::from(len))?;
        Some(om_hbam::serial::decode(segment).filter(|id| *id != 0))
    }
}

/// Decode one trigger record (`05/<event>/01`); `None` when it is
/// malformed. Bytes after the extra bytes are ignored.
pub fn decode_record(bytes: &[u8]) -> Option<ScriptTrigger> {
    let mut r = Reader { bytes, at: 0 };
    let event = match r.u16()? {
        OLD_VIEW_CHANGE => VIEW_CHANGE,
        event => event,
    };
    let script = r.key()?;
    let file_reference = r.key()?;
    let modes = r.u32()?;
    let option = r.u32()?;
    let count = r.u16()?;
    if count >= EXTRA_LIMIT {
        return None;
    }
    let extra = r.take(usize::from(count))?.to_vec();
    Some(ScriptTrigger {
        event,
        script,
        file_reference,
        external: file_reference.is_some(),
        modes,
        option,
        extra,
        parameter: None,
    })
}

/// A key of one serial segment; `00` for none.
fn put_key(out: &mut Vec<u8>, id: Option<u64>) {
    match id {
        Some(id) => {
            let segment = om_hbam::serial::encode(id);
            out.push(segment.len() as u8 + 1);
            out.push(segment.len() as u8);
            out.extend(segment);
        }
        None => out.push(0),
    }
}

/// The record [`decode_record`] reads.
pub fn encode_record(trigger: &ScriptTrigger) -> Vec<u8> {
    let mut out = trigger.event.to_be_bytes().to_vec();
    put_key(&mut out, trigger.script);
    put_key(&mut out, trigger.file_reference);
    out.extend(trigger.modes.to_be_bytes());
    out.extend(trigger.option.to_be_bytes());
    out.extend((trigger.extra.len() as u16).to_be_bytes());
    out.extend(&trigger.extra);
    out
}

/// The formula stored at `node`, when it has a Polish stream that decodes.
fn formula(node: &om_hbam::Node<'_>) -> Option<StoredFormula> {
    let polish = node.get(POLISH)?.assembled()?;
    let context = node.get(CONTEXT).and_then(|n| n.assembled());
    Formula::decode(context.as_deref(), None, &polish).ok()?;
    Some(StoredFormula {
        context: context.map(|c| c.into_owned()),
        polish: polish.into_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use om_hbam::{KeyTree, OwnedNode, SlotReader};

    #[test]
    fn decodes_the_template_on_layout_enter_record() {
        // test project, layout 2: OnLayoutEnter runs script 3 in Browse.
        let record = [
            0x00, 0x65, 0x02, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let trigger = decode_record(&record).unwrap();
        assert_eq!(trigger.event, 101);
        assert_eq!(trigger.script, Some(3));
        assert_eq!(trigger.modes, 1);
        assert_eq!(trigger.file_reference, None);
        assert!(!trigger.external);
        assert_eq!(encode_record(&trigger), record);
        assert!(decode_record(&record[..10]).is_none());
    }

    #[test]
    fn script_ids_from_128_lengthen_the_record() {
        // OnRecordLoad, script 141 (`80 0d`), Browse and Find: 17 bytes.
        let record = [
            0x00, 0x67, 0x03, 0x02, 0x80, 0x0d, 0x00, 0x00, 0x00, 0x00, 0x03, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00,
        ];
        let trigger = decode_record(&record).unwrap();
        assert_eq!(
            (trigger.event, trigger.script, trigger.modes),
            (103, Some(141), 3)
        );
        assert_eq!(encode_record(&trigger), record);
        // The old view-change id, and a script in another file.
        let mut bytes = vec![0, 108, 0x03, 0x02, 0x80, 0x0d, 0x02, 0x01, 0x02];
        bytes.extend(&record[7..]);
        let trigger = decode_record(&bytes).unwrap();
        assert_eq!(trigger.event, 110);
        assert_eq!(trigger.file_reference, Some(2));
        assert!(trigger.external);
        assert_eq!(trigger.modes, 3);
        // 1000 extra bytes are refused.
        let mut long = record[..15].to_vec();
        long.extend(1000u16.to_be_bytes());
        long.extend([0; 1000]);
        assert!(decode_record(&long).is_none());
    }

    #[test]
    fn the_parameter_is_the_formula_at_31() {
        let context = om_formula::Context {
            names: &om_formula::NoNames,
            occurrence: None,
            parameters: &[],
            support_self: false,
            decimal_comma: false,
        };
        let expr = om_formula::compile("7", &context).unwrap().expr;
        let polish = om_formula::polish::encode(&expr);
        let record = [
            0x00, 0x65, 0x02, 0x01, 0x03, 0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00,
        ];
        let event = OwnedNode::list(
            [0x00, 0x65],
            vec![
                OwnedNode::leaf([RECORD as u8], record),
                OwnedNode::list(
                    [PARAMETER as u8],
                    vec![OwnedNode::leaf([POLISH as u8], polish.clone())],
                ),
            ],
        );
        let cache = OwnedNode::list(
            [0x30],
            vec![
                OwnedNode::leaf([4], [0, 1]),
                OwnedNode::list([ENTRIES as u8], vec![event]),
            ],
        );
        let wire = OwnedNode::list(Vec::new(), vec![cache]).to_wire().unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let triggers = decode(tree.at(&[0x30]).unwrap());
        assert_eq!(triggers.len(), 1);
        assert_eq!(triggers[0].parameter.as_ref().unwrap().polish, polish);
    }
}
