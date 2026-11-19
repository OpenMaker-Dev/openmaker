//! The sort orders and find requests stored inside steps
//! (script-steps.md 11.1, 11.2): decoded for running them. The step codec
//! keeps the nodes themselves byte for byte.

use om_hbam::{OwnedNode, RidList};

use crate::{node_bytes, node_text, text_node};

/// A field a spec names: the table occurrence and field as their key
/// segments read big-endian (the numbering step field specs use), and the
/// repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpecField {
    /// `None` when the spec names no occurrence (an empty key).
    pub occurrence: Option<u64>,
    pub field: u64,
    pub repetition: u16,
}

/// A stored sort order (`0a`, `DBSortSpec`, 11.1.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortSpec {
    /// Flags bit 0 clear: "Keep records in sorted order".
    pub keep_sorted: bool,
    /// Flags bit 1: empty values last in both directions.
    pub blanks_last: bool,
    pub fields: Vec<SortEntry>,
}

/// One sort field, in priority order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SortEntry {
    pub field: SpecField,
    /// "Reorder based on summary field".
    pub summary: Option<SpecField>,
    /// "Custom order based on value list": the value list's key.
    pub value_list: Option<u64>,
    /// Order bit 0.
    pub descending: bool,
    /// A `Locale::Type` overriding the field's language; 0 none.
    pub locale: u32,
}

/// A big-endian reader over a stored stream.
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

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    /// An `HBKey`: `[u8 n]` and its segments; the first segment's value,
    /// `None` for no key (`00` or the segment `00`, 11.0).
    fn key(&mut self) -> Option<Option<u64>> {
        let n = usize::from(self.u8()?);
        let key = self.take(n)?;
        let Some((&len, rest)) = key.split_first() else {
            return Some(None);
        };
        let segment = rest.get(..usize::from(len))?;
        Some(om_hbam::slot::be_uint(segment).filter(|v| *v != 0))
    }

    /// A field spec: table key, field key, u16 repetition.
    fn field(&mut self) -> Option<Option<SpecField>> {
        let occurrence = self.key()?;
        let field = self.key()?;
        let repetition = self.u16()?;
        Some(field.map(|field| SpecField {
            occurrence,
            field,
            repetition,
        }))
    }
}

impl SortSpec {
    /// Decode the value of `0a`. `None` when it does not have the layout.
    pub fn decode(bytes: &[u8]) -> Option<SortSpec> {
        let (spec, used) = SortSpec::read(bytes)?;
        (used == bytes.len()).then_some(spec)
    }

    /// Decode a sort spec at the start of `bytes`, as relationship and value
    /// list definitions embed it (file-metadata.md 4.1, 6.2); returns it and
    /// the bytes it took.
    pub fn read(bytes: &[u8]) -> Option<(SortSpec, usize)> {
        let mut r = Reader { bytes, at: 0 };
        let flags = r.u16()?;
        let count = r.u16()?;
        let mut fields = Vec::with_capacity(usize::from(count));
        for _ in 0..count {
            let field = r.field()??;
            let summary = r.field()?;
            let value_list = r.key()?;
            let order = r.u8()?;
            let locale = r.u32()?;
            fields.push(SortEntry {
                field,
                summary,
                value_list,
                descending: order & 1 != 0,
                locale,
            });
        }
        Some((
            SortSpec {
                keep_sorted: flags & 1 == 0,
                blanks_last: flags & 2 != 0,
                fields,
            },
            r.at,
        ))
    }

    /// Decode a step's `0a` node.
    pub fn from_node(node: &OwnedNode) -> Option<SortSpec> {
        SortSpec::decode(&node_bytes(node)?)
    }

    /// The stream as FileMaker writes it (11.1.1): an absent summary field
    /// as keys holding the segment `00`, repetition 1; an absent value list
    /// as the empty key `00`.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::new();
        let flags = u16::from(!self.keep_sorted) | (u16::from(self.blanks_last) << 1);
        out.extend(flags.to_be_bytes());
        out.extend((self.fields.len() as u16).to_be_bytes());
        for entry in &self.fields {
            put_field(&mut out, Some(entry.field));
            put_field(&mut out, entry.summary);
            // `02 01 00` here reads as value list -1 and the entry shows as
            // a custom order (FileMaker's XML: `ValueListReference id="-1"`).
            match entry.value_list {
                Some(list) => put_key(&mut out, Some(list)),
                None => out.push(0),
            }
            out.push(u8::from(entry.descending));
            out.extend(entry.locale.to_be_bytes());
        }
        out
    }
}

/// An `HBKey` of one segment; `None` as the segment `00`.
fn put_key(out: &mut Vec<u8>, value: Option<u64>) {
    let segment = om_hbam::slot::uint_segment(value.unwrap_or(0));
    out.push(segment.len() as u8 + 1);
    out.push(segment.len() as u8);
    out.extend(segment);
}

fn put_field(out: &mut Vec<u8>, field: Option<SpecField>) {
    put_key(out, field.and_then(|f| f.occurrence));
    put_key(out, field.map(|f| f.field));
    out.extend(field.map_or(1, |f| f.repetition).to_be_bytes());
}

/// One stored find request (11.2.1).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredRequest {
    pub omit: bool,
    /// Criteria as typed, operators and variables included.
    pub criteria: Vec<(SpecField, String)>,
}

/// The request rows of a step's `06` node, in request order (the order
/// list `05`, else id order).
pub fn requests(node: &OwnedNode) -> Option<Vec<StoredRequest>> {
    let rows = node.child(&[0x06])?;
    let order: Vec<u64> = match node.child(&[0x05]).map(RidList::from_owned) {
        Some(Ok(list)) => list.iter().collect(),
        _ => rows
            .children
            .iter()
            .filter(|row| row.segment.as_slice() != [0x00])
            .filter_map(|row| om_hbam::serial::decode(&row.segment))
            .collect(),
    };
    let mut out = Vec::with_capacity(order.len());
    for id in order {
        let row = rows.child(&om_hbam::serial::encode(id))?;
        let mut request = StoredRequest {
            omit: false,
            criteria: Vec::new(),
        };
        for cell in &row.children {
            match cell.segment.as_slice() {
                [0x00] => request.omit = true,
                [0xfc] => {}
                key => {
                    if let (Some(field), Some(text)) = (criterion_key(key), node_text(cell)) {
                        request.criteria.push((field, text));
                    }
                }
            }
        }
        out.push(request);
    }
    Some(out)
}

/// A `06` node for `requests` as FileMaker writes a new request set
/// (11.2.6): ids 1..n in order, the highest id, omit markers, criteria
/// under their packed keys.
pub fn encode_requests(requests: &[StoredRequest]) -> OwnedNode {
    let mut order = RidList::new();
    let mut rows = vec![OwnedNode::leaf([0x00], {
        let id = om_hbam::serial::encode(requests.len() as u64);
        let mut value = vec![id.len() as u8];
        value.extend(id);
        value
    })];
    for (i, request) in requests.iter().enumerate() {
        let id = i as u64 + 1;
        order.insert(id);
        let mut cells = Vec::new();
        if request.omit {
            cells.push(OwnedNode::leaf([0x00], Vec::new()));
        }
        let mut criteria: Vec<(Vec<u8>, &str)> = request
            .criteria
            .iter()
            .map(|(field, text)| (packed_key(field), text.as_str()))
            .collect();
        criteria.sort();
        cells.extend(
            criteria
                .into_iter()
                .map(|(key, text)| text_node(&key, text)),
        );
        rows.push(OwnedNode::list(om_hbam::serial::encode(id), cells));
    }
    OwnedNode::list(
        vec![0x06],
        vec![order.to_node(vec![0x05]), OwnedNode::list(vec![0x06], rows)],
    )
}

/// `DBFieldSpec::GetFieldSpecKey` for a field of an occurrence.
fn packed_key(field: &SpecField) -> Vec<u8> {
    let occurrence = om_hbam::slot::uint_segment(field.occurrence.unwrap_or(0));
    let mut segment = om_hbam::slot::uint_segment(field.field);
    if field.repetition != 1 {
        segment.extend(om_hbam::serial::encode(u64::from(field.repetition)));
    }
    let mut key = vec![0x00, occurrence.len() as u8];
    key.extend(occurrence);
    key.push(segment.len() as u8);
    key.extend(segment);
    key
}

/// A packed criterion key (`DBFieldSpec::GetFieldSpecKey`): `00`, the
/// occurrence's segment, then one segment holding the field id and, when
/// it is not 1, the repetition, both as serial keys.
fn criterion_key(key: &[u8]) -> Option<SpecField> {
    let rest = key.strip_prefix(&[0x00])?;
    let (&len, rest) = rest.split_first()?;
    let occurrence = om_hbam::slot::be_uint(rest.get(..usize::from(len))?)?;
    let rest = &rest[usize::from(len)..];
    let (&len, rest) = rest.split_first()?;
    let segment = rest.get(..usize::from(len))?;
    let field_len = usize::from(segment.first()?.leading_ones() as u8 + 1);
    let field = om_hbam::slot::be_uint(segment.get(..field_len)?)?;
    let repetition = match &segment[field_len..] {
        [] => 1,
        rep => u16::try_from(om_hbam::serial::decode(rep)?).ok()?,
    };
    Some(SpecField {
        occurrence: Some(occurrence),
        field,
        repetition,
    })
}

/// `DBCalcScan::ExpandVariables` (11.2.3): each `$name` or `$$name` becomes
/// the variable's text; a variable `lookup` does not know stays as written;
/// replaced text is not scanned again.
pub fn expand_variables(text: &str, mut lookup: impl FnMut(&str) -> Option<String>) -> String {
    let chars: Vec<char> = text.chars().collect();
    let mut out = String::with_capacity(text.len());
    let mut i = 0;
    while i < chars.len() {
        if chars[i] == '$' {
            let start = i;
            let mut j = i + 1;
            if chars.get(j) == Some(&'$') {
                j += 1;
            }
            let name_start = j;
            while j < chars.len()
                && (chars[j].is_alphanumeric() || chars[j] == '_' || chars[j] == '.')
            {
                j += 1;
            }
            if j > name_start {
                let name: String = chars[start..j].iter().collect();
                match lookup(&name) {
                    Some(value) => out.push_str(&value),
                    None => out.push_str(&name),
                }
                i = j;
                continue;
            }
        }
        out.push(chars[i]);
        i += 1;
    }
    out
}

/// A copy of a `06` node with the variables of every criterion expanded
/// (what restoring the requests into the window does, 11.2.3).
pub fn expand_request_node(
    node: &OwnedNode,
    mut lookup: impl FnMut(&str) -> Option<String>,
) -> OwnedNode {
    let mut node = node.clone();
    if let Some(rows) = node.child_mut(&[0x06]) {
        for row in &mut rows.children {
            if row.segment.as_slice() == [0x00] {
                continue;
            }
            for cell in &mut row.children {
                if cell.segment.first() != Some(&0x00) || cell.segment.len() < 2 {
                    continue;
                }
                let Some(text) = node_text(cell) else {
                    continue;
                };
                if !text.contains('$') {
                    continue;
                }
                let expanded = expand_variables(&text, &mut lookup);
                if expanded != text {
                    *cell = text_node(&cell.segment.clone(), &expanded);
                }
            }
        }
    }
    node
}

/// A step's external data-source reference (`20`, script-steps.md 11.8).
/// Its serial key has an explicit byte count; truncated/trailing data is invalid.
pub fn data_source(node: &OwnedNode) -> Option<u64> {
    let bytes = node.assembled()?;
    let (&len, key) = bytes.split_first()?;
    (key.len() == usize::from(len))
        .then(|| om_hbam::serial::decode(key))
        .flatten()
        .filter(|id| *id != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The Contacts template's "Sort by Name": Initial ascending, no
    /// summary field (keys `02 01 00`), no value list (key `00`).
    const CONTACTS_SORT_BY_NAME: [u8; 28] = [
        0x00, 0x00, 0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x02, 0x02, 0x01, 0x08, 0x00, 0x01, 0x02,
        0x01, 0x00, 0x02, 0x01, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    /// The Tasks template's "Sort by Status": Status ascending, custom
    /// order by value list 2.
    const TASKS_SORT_BY_STATUS: [u8; 30] = [
        0x00, 0x00, 0x00, 0x01, 0x04, 0x03, 0xd0, 0x00, 0x02, 0x02, 0x01, 0x04, 0x00, 0x01, 0x02,
        0x01, 0x00, 0x02, 0x01, 0x00, 0x00, 0x01, 0x02, 0x01, 0x02, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    #[test]
    fn sort_specs_encode_as_filemaker_stores_them() {
        // An absent value list is the empty key `00`, not `02 01 00`.
        for bytes in [&CONTACTS_SORT_BY_NAME[..], &TASKS_SORT_BY_STATUS[..]] {
            assert_eq!(SortSpec::decode(bytes).unwrap().encode(), bytes);
        }
        let status = SortSpec::decode(&TASKS_SORT_BY_STATUS).unwrap();
        assert_eq!(status.fields[0].value_list, Some(2));
        let name = SortSpec::decode(&CONTACTS_SORT_BY_NAME).unwrap();
        assert_eq!(name.fields[0].value_list, None);
    }

    #[test]
    fn sort_specs_decode_as_stored() {
        // Contacts template "Sort by Name" as stored (11.1.1).
        let bytes = CONTACTS_SORT_BY_NAME;
        let spec = SortSpec::decode(&bytes).unwrap();
        assert!(spec.keep_sorted && !spec.blanks_last);
        assert_eq!(
            spec.fields,
            [SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00002),
                    field: 8,
                    repetition: 1
                },
                summary: None,
                value_list: None,
                descending: false,
                locale: 0,
            }]
        );
        // query-program.md 10.2's form: empty keys as `00`, descending,
        // blanks last, not maintained.
        let short = [
            0x00, 0x03, 0x00, 0x01, 0x03, 0x02, 0x80, 0x02, 0x02, 0x01, 0x02, 0x00, 0x01, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x19,
        ];
        let spec = SortSpec::decode(&short).unwrap();
        assert!(!spec.keep_sorted && spec.blanks_last);
        assert!(spec.fields[0].descending);
        assert_eq!(spec.fields[0].locale, 25);
        assert_eq!(spec.fields[0].field.occurrence, Some(0x8002));
        assert!(SortSpec::decode(&bytes[..20]).is_none());
        // Embedded in a longer stream, as in a relationship definition.
        let mut longer = bytes.to_vec();
        longer.extend([0, 0, 0, 0xa2]);
        assert!(SortSpec::decode(&longer).is_none());
        assert_eq!(
            SortSpec::read(&longer),
            Some((SortSpec::decode(&bytes).unwrap(), bytes.len()))
        );
    }

    fn request_set(rows: Vec<OwnedNode>, order: &[u64]) -> OwnedNode {
        let mut list = RidList::new();
        for id in order {
            list.insert(*id);
        }
        let mut children = vec![OwnedNode::leaf([0x00], vec![1, order.len() as u8])];
        children.extend(rows);
        OwnedNode::list(
            vec![0x06],
            vec![
                list.to_node(vec![0x05]),
                OwnedNode::list(vec![0x06], children),
            ],
        )
    }

    #[test]
    fn requests_decode_with_omit_markers_and_repetitions() {
        let key = |field: u8, rep: &[u8]| {
            let mut k = vec![0x00, 0x03, 0xd0, 0x00, 0x02, 1 + rep.len() as u8, field];
            k.extend_from_slice(rep);
            k
        };
        let first = OwnedNode::list(vec![0x01], vec![text_node(&key(4, &[]), "In Progress")]);
        let second = OwnedNode::list(
            vec![0x02],
            vec![
                OwnedNode::leaf([0x00], Vec::new()),
                text_node(&key(1, &[0x80, 0xac]), "b"),
            ],
        );
        let node = request_set(vec![first, second], &[1, 2]);
        let decoded = requests(&node).unwrap();
        assert_eq!(decoded.len(), 2);
        assert!(!decoded[0].omit && decoded[1].omit);
        assert_eq!(decoded[0].criteria[0].0.field, 4);
        assert_eq!(decoded[0].criteria[0].1, "In Progress");
        assert_eq!(decoded[1].criteria[0].0.repetition, 300);

        let expanded = expand_request_node(&node, |name| (name == "$s").then(|| "Done".into()));
        assert_eq!(requests(&expanded).unwrap(), decoded);
        let with_var = request_set(
            vec![OwnedNode::list(
                vec![0x01],
                vec![text_node(&key(4, &[]), "==$s $nosuch")],
            )],
            &[1],
        );
        let expanded = expand_request_node(&with_var, |name| (name == "$s").then(|| "Done".into()));
        assert_eq!(
            requests(&expanded).unwrap()[0].criteria[0].1,
            "==Done $nosuch"
        );
    }

    #[test]
    fn written_specs_read_back() {
        let spec = SortSpec {
            keep_sorted: true,
            blanks_last: true,
            fields: vec![SortEntry {
                field: SpecField {
                    occurrence: Some(0xd00002),
                    field: 0x8145,
                    repetition: 1,
                },
                summary: None,
                value_list: None,
                descending: true,
                locale: 0,
            }],
        };
        assert_eq!(SortSpec::decode(&spec.encode()), Some(spec));
        let requests = vec![
            StoredRequest {
                omit: false,
                criteria: vec![(
                    SpecField {
                        occurrence: Some(0xd00002),
                        field: 4,
                        repetition: 1,
                    },
                    "==Ada".into(),
                )],
            },
            StoredRequest {
                omit: true,
                criteria: vec![(
                    SpecField {
                        occurrence: Some(0xd00002),
                        field: 1,
                        repetition: 300,
                    },
                    "b".into(),
                )],
            },
        ];
        assert_eq!(super::requests(&encode_requests(&requests)), Some(requests));
    }

    #[test]
    fn variables_expand_once_and_unknown_ones_stay() {
        let lookup = |name: &str| match name {
            "$a" => Some("$b".to_owned()),
            "$$g" => Some("Low".to_owned()),
            _ => None,
        };
        assert_eq!(
            expand_variables("$a and $$g, $x, $", lookup),
            "$b and Low, $x, $"
        );
    }
}

#[cfg(test)]
mod file_reference_tests {
    use super::*;
    #[test]
    fn a_data_source_reference_has_one_complete_serial_key() {
        for id in [1, 129, 16385] {
            let key = om_hbam::serial::encode(id);
            let mut bytes = vec![key.len() as u8];
            bytes.extend(&key);
            assert_eq!(
                data_source(&OwnedNode::leaf([32], bytes.as_slice())),
                Some(id)
            );
            bytes.push(0);
            assert_eq!(data_source(&OwnedNode::leaf([32], bytes)), None);
        }
        assert_eq!(data_source(&OwnedNode::leaf([32], [1, 0])), None);
    }
}
