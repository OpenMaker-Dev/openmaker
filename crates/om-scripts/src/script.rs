//! The script catalog `[17]` and one script definition `[17, 5, S]`
//! (scripts.md, script-steps.md 1 and 6).

use crate::flow;
use crate::key::StoredKey;
use crate::params::ParamValue;
use crate::step::{ScriptStep, StepData};
use crate::value::{bytes_node, node_text, text_node};
use crate::{Result, ScriptError};
use om_hbam::{serial, KeyTree, Node, OwnedNode};
use std::collections::BTreeSet;
use std::ops::Range;

/// Key of the script name (`KName`).
const NAME: u8 = 0x10;
/// Key of the step list.
const STEP_LIST: u8 = 0x04;
/// Key of the step data subtrees.
const STEP_DATA: u8 = 0x05;
/// Key of the private member flags (`KPrivateMemberFlags`, script-steps.md
/// 1); bit 0 is "Run script with full access privileges" (script-runtime.md
/// 9.1).
const PRIVATE_FLAGS: u8 = 0xf1;
const RUN_WITH_FULL_ACCESS: u64 = 0x1;
/// Key of the member flags (`KMemberFlags`): bit 0 folder, bit 1 folder
/// end, bit 3 left out of the Scripts menu (FileMaker's XML `hidden`,
/// script-steps.md, "Implementation notes (step text)").
const MEMBER_FLAGS: u8 = 0xf0;
const HIDDEN: u64 = 0x8;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ScriptCatalog {
    pub scripts: Vec<Script>,
    pub writable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ScriptKind {
    #[default]
    Script,
    /// A folder (member flag `0x01`).
    Folder,
    /// The end of a folder, named `--` (flag `0x02`); the Scripts menu
    /// and `ScriptNames` leave it out.
    FolderEnd,
    Separator,
}

/// A script definition. Decoding keeps every stored node, so
/// [`Script::encode`] rebuilds `[17, 5, S]` exactly; edits through the
/// methods below keep the cached block indexes right.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Script {
    /// The id FileMaker shows (`ScriptIDs`, Perform Script references, the
    /// script key of Perform Script on Server): the serial decode of the
    /// definition's key segment (file-metadata.md 1; `80 0d` is 141).
    pub id: u64,
    pub name: String,
    pub kind: ScriptKind,
    pub steps: Vec<ScriptStep>,
    /// Children of the definition other than the step list (`04`) and the
    /// step data (`05`): the name node (`10`, rewritten when `name`
    /// changes), `13`, UUIDs, flags and mod counts, verbatim.
    pub metadata: Vec<OwnedNode>,
    /// Step data subtrees under `05` that no step's data key names, verbatim.
    pub unreferenced_data: Vec<OwnedNode>,
    /// Whether the definition stores a step data node (`05`) even when it
    /// holds nothing.
    pub step_data_node: bool,
}

impl ScriptCatalog {
    pub fn decode(tree: &KeyTree<'_>) -> Result<Self> {
        let mut scripts = Vec::new();
        if let Some(definitions) = tree.at(&[17, 5]) {
            for node in &definitions.children {
                // A deleted script's member is emptied (FileMaker Pro's op13
                // with node state `00`, scripts.md): no member.
                let emptied = node.children.is_empty() && node.value.is_none();
                if node.id().filter(|id| *id != 0).is_some() && !emptied {
                    scripts.push(Script::decode(node)?);
                }
            }
        }
        // The Scripts pane and menu show the catalog's display order
        // `[17, 1, 03, n]` (file-metadata.md 1.1); members it does not list
        // follow in id order.
        if let Some(order) = display_order(tree) {
            scripts.sort_by_key(|s| {
                (
                    order
                        .iter()
                        .position(|id| *id == s.id)
                        .unwrap_or(usize::MAX),
                    s.id,
                )
            });
        }
        Ok(Self {
            scripts,
            ..Self::default()
        })
    }

    pub fn script(&self, id: u64) -> Option<&Script> {
        self.scripts
            .iter()
            .find(|s| s.id == id && s.kind == ScriptKind::Script)
    }
}

/// The script ids of the catalog's display order: each entry `[17, 1, 03,
/// n]` is one key segment with its length byte, the serial key of a
/// definition.
fn display_order(tree: &KeyTree<'_>) -> Option<Vec<u64>> {
    let order = tree.at(&[17, 1, 3])?;
    let mut entries: Vec<(u64, u64)> = order
        .children
        .iter()
        .filter_map(|n| {
            let value = n.assembled()?;
            let (&len, segment) = value.split_first()?;
            Some((n.id()?, serial::decode(segment.get(..usize::from(len))?)?))
        })
        .collect();
    entries.sort_by_key(|(position, _)| *position);
    Some(entries.into_iter().map(|(_, id)| id).collect())
}

impl Script {
    /// Decode a definition node `[17, 5, S]`.
    pub fn decode(node: &Node<'_>) -> Result<Self> {
        let id = serial::decode(node.segment.0).ok_or_else(|| {
            ScriptError(format!("Script key {} is not a serial key.", node.segment))
        })?;
        let name = node
            .get(u64::from(NAME))
            .and_then(Node::text)
            .transpose()
            .map_err(|e| ScriptError(format!("Script {id} name: {e}")))?
            .unwrap_or_else(|| default_name(id));
        let list = node.get(u64::from(STEP_LIST)).and_then(Node::assembled);
        // Folders keep an empty step list, so the flags decide first.
        let flags = node.get(0xf0).and_then(Node::uint_value).unwrap_or(0);
        let kind = if flags & 1 != 0 {
            ScriptKind::Folder
        } else if flags & 2 != 0 {
            ScriptKind::FolderEnd
        } else if list.is_some() {
            ScriptKind::Script
        } else {
            ScriptKind::Separator
        };
        let data = node.get(u64::from(STEP_DATA));
        let mut steps = Vec::new();
        if let Some(bytes) = list {
            if bytes.len() % 28 != 0 {
                return Err(ScriptError(format!(
                    "Script {name} has an unsupported step-list size ({} bytes).",
                    bytes.len()
                )));
            }
            for chunk in bytes.chunks_exact(28) {
                let mut step = ScriptStep::from_descriptor(chunk.try_into().expect("28 bytes"));
                step.data = step
                    .data_key
                    .segment()
                    .and_then(|segment| data?.child(segment))
                    .map(|n| StepData::decode(&n.to_owned_node()));
                steps.push(step);
            }
        }
        let referenced: BTreeSet<&[u8]> =
            steps.iter().filter_map(|s| s.data_key.segment()).collect();
        let unreferenced_data = data
            .map(|d| {
                d.children
                    .iter()
                    .filter(|c| !referenced.contains(c.segment.0))
                    .map(Node::to_owned_node)
                    .collect()
            })
            .unwrap_or_default();
        // A folder's empty step list is kept as stored: only a script's is
        // rebuilt from its steps.
        let metadata = node
            .children
            .iter()
            .filter(|c| match c.segment.0 {
                [STEP_LIST] => kind != ScriptKind::Script,
                [STEP_DATA] => false,
                _ => true,
            })
            .map(Node::to_owned_node)
            .collect();
        Ok(Script {
            id,
            name,
            kind,
            steps,
            metadata,
            unreferenced_data,
            step_data_node: data.is_some(),
        })
    }

    /// The private member flags `f1`, 0 when not stored.
    fn private_flags(&self) -> u64 {
        self.metadata
            .iter()
            .find(|n| n.segment == [PRIVATE_FLAGS])
            .and_then(|n| n.value.as_deref())
            .and_then(om_hbam::slot::be_uint)
            .unwrap_or(0)
    }

    /// "Run script with full access privileges": bit 0 of the private
    /// member flags (script-runtime.md 9.1; no sample file sets it, so the
    /// key holding those flags, `f1`, is inferred from its name).
    pub fn runs_with_full_access(&self) -> bool {
        self.private_flags() & RUN_WITH_FULL_ACCESS != 0
    }

    /// Turn "Run script with full access privileges" on or off, keeping the
    /// other private flags.
    pub fn set_runs_with_full_access(&mut self, on: bool) {
        let flags = if on {
            self.private_flags() | RUN_WITH_FULL_ACCESS
        } else {
            self.private_flags() & !RUN_WITH_FULL_ACCESS
        };
        let value = om_hbam::slot::uint_segment(flags);
        match self
            .metadata
            .iter_mut()
            .find(|n| n.segment == [PRIVATE_FLAGS])
        {
            Some(node) => node.value = Some(value),
            None if flags != 0 => self
                .metadata
                .push(OwnedNode::leaf(vec![PRIVATE_FLAGS], value)),
            None => {}
        }
    }

    /// The member flags `f0`, 0 when not stored.
    fn member_flags(&self) -> u64 {
        self.metadata
            .iter()
            .find(|n| n.segment == [MEMBER_FLAGS])
            .and_then(|n| n.value.as_deref())
            .and_then(om_hbam::slot::be_uint)
            .unwrap_or(0)
    }

    /// "Include in menu": the Scripts menu lists the member (bit 3 of the
    /// member flags clear).
    pub fn in_scripts_menu(&self) -> bool {
        self.member_flags() & HIDDEN == 0
    }

    /// Include the member in the Scripts menu or leave it out, keeping the
    /// other member flags.
    pub fn set_in_scripts_menu(&mut self, on: bool) {
        let flags = if on {
            self.member_flags() & !HIDDEN
        } else {
            self.member_flags() | HIDDEN
        };
        let value = om_hbam::slot::uint_segment(flags);
        match self
            .metadata
            .iter_mut()
            .find(|n| n.segment == [MEMBER_FLAGS])
        {
            Some(node) => node.value = Some(value),
            None if flags != 0 => self
                .metadata
                .push(OwnedNode::leaf(vec![MEMBER_FLAGS], value)),
            None => {}
        }
    }

    /// The step list (`04`): the 28-byte descriptors in order.
    pub fn encoded_steps(&self) -> Vec<u8> {
        self.steps.iter().flat_map(|s| s.descriptor()).collect()
    }

    /// The step data node (`05`): each step's subtree and the unreferenced
    /// ones, in key order; `None` when the definition has none.
    pub fn encoded_step_data(&self) -> Option<OwnedNode> {
        let mut seen = BTreeSet::new();
        let mut children: Vec<OwnedNode> = self
            .steps
            .iter()
            .filter_map(ScriptStep::data_node)
            .chain(self.unreferenced_data.iter().cloned())
            .filter(|n| seen.insert(n.segment.clone()))
            .collect();
        if children.is_empty() && !self.step_data_node {
            return None;
        }
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        Some(OwnedNode::list([STEP_DATA], children))
    }

    /// The definition node `[17, 5, S]`: step list (chunked above 1000
    /// bytes), step data, and the metadata with the name node rewritten when
    /// the name changed.
    pub fn encode(&self) -> OwnedNode {
        let mut children = Vec::new();
        if self.kind == ScriptKind::Script {
            children.push(bytes_node(&[STEP_LIST], &self.encoded_steps()));
        }
        children.extend(self.encoded_step_data());
        let mut has_name = false;
        for node in &self.metadata {
            if node.segment == [NAME] {
                has_name = true;
                if node_text(node).as_deref() != Some(self.name.as_str()) {
                    children.push(text_node(&[NAME], &self.name));
                    continue;
                }
            }
            children.push(node.clone());
        }
        if !has_name && self.name != default_name(self.id) {
            children.push(text_node(&[NAME], &self.name));
        }
        children.sort_by(|a, b| a.segment.cmp(&b.segment));
        OwnedNode::list(serial::encode(self.id), children)
    }

    /// The stored definition `stored` with this script's step list (`04`)
    /// and step data (`05`), and every other child as stored: what a step
    /// edit saves (OpenMaker Server's `ReplaceScriptSteps`, authoring.md),
    /// leaving the name, options, UUIDs and keys OpenMaker does not know as
    /// the host has them.
    pub fn with_steps_in(&self, stored: &OwnedNode) -> OwnedNode {
        let steps = |c: &&OwnedNode| matches!(c.segment.as_slice(), [STEP_LIST] | [STEP_DATA]);
        let encoded = self.encode();
        let mut node = stored.clone();
        node.children.retain(|c| !steps(&c));
        for child in encoded.children.iter().filter(steps) {
            node.set_child(child.clone());
        }
        node
    }

    /// Check that the enabled steps nest (If/Else If/Else/End If, Loop/Exit
    /// Loop If/End Loop, Open/Commit Transaction).
    pub fn check_structure(&self) -> Result<()> {
        flow::analyze(&self.steps).map(|_| ())
    }

    /// Recompute the index every enabled block step caches
    /// (script-runtime.md 3.1). Refuses an unbalanced script.
    pub fn update_partner_indexes(&mut self) -> Result<()> {
        flow::update(&mut self.steps)
    }

    /// Apply an edit to a copy of the steps; keep it only when the result
    /// nests, with its cached indexes recomputed.
    fn edit<T>(&mut self, f: impl FnOnce(&mut Vec<ScriptStep>) -> Result<T>) -> Result<T> {
        let mut steps = self.steps.clone();
        let out = f(&mut steps)?;
        flow::update(&mut steps)?;
        self.steps = steps;
        Ok(out)
    }

    /// The next step data key: one past the highest serial key in `05`.
    fn next_data_key(&self) -> u64 {
        self.steps
            .iter()
            .filter_map(|s| s.data_key.segment())
            .chain(self.unreferenced_data.iter().map(|n| n.segment.as_slice()))
            .filter_map(serial::decode)
            .max()
            .map_or(1, |k| k + 1)
    }

    /// Insert steps before `at`. Each gets a fresh data key (the next serial
    /// key of `05`, as `ScriptStep::MakeDataKey` allocates); Go to Record and
    /// Go to Portal Row get their "Exit after last" index. Refused, leaving
    /// the script unchanged, when the result does not nest.
    pub fn insert_steps(
        &mut self,
        at: usize,
        steps: impl IntoIterator<Item = ScriptStep>,
    ) -> Result<()> {
        let mut inserted = Vec::new();
        for (key, mut step) in (self.next_data_key()..).zip(steps) {
            step.data_key = StoredKey::from_segment(&serial::encode(key))
                .ok_or_else(|| ScriptError("The script has no step data keys left.".into()))?;
            if matches!(step.id(), flow::GO_TO_RECORD | flow::GO_TO_PORTAL_ROW)
                && step.number(0).is_none()
            {
                step.set_number(0, Some(-1));
            }
            inserted.push(step);
        }
        self.edit(|steps| {
            if at > steps.len() {
                return Err(ScriptError(format!("Cannot insert at step {}.", at + 1)));
            }
            steps.splice(at..at, inserted);
            Ok(())
        })
    }

    /// Replace the whole step list: each entry one of the script's own
    /// steps, kept as stored (data key, data and item UUIDs), or a new step
    /// that gets a fresh data key above every key the script held, as
    /// [`Script::insert_steps`] gives one. Steps not kept are deleted with
    /// their data. Refused, leaving the script unchanged, when a step is
    /// kept twice or out of range, or the result does not nest.
    pub fn replace_steps(&mut self, steps: impl IntoIterator<Item = StepSource>) -> Result<()> {
        let mut next = self.next_data_key();
        let mut kept = BTreeSet::new();
        let mut list = Vec::new();
        for source in steps {
            match source {
                StepSource::Kept(i) => {
                    let step = self
                        .steps
                        .get(i)
                        .ok_or_else(|| ScriptError(format!("The script has no step {}.", i + 1)))?;
                    if !kept.insert(i) {
                        return Err(ScriptError(format!("Step {} is kept twice.", i + 1)));
                    }
                    list.push(step.clone());
                }
                StepSource::New(mut step) => {
                    step.data_key =
                        StoredKey::from_segment(&serial::encode(next)).ok_or_else(|| {
                            ScriptError("The script has no step data keys left.".into())
                        })?;
                    next += 1;
                    if matches!(step.id(), flow::GO_TO_RECORD | flow::GO_TO_PORTAL_ROW)
                        && step.number(0).is_none()
                    {
                        step.set_number(0, Some(-1));
                    }
                    list.push(step);
                }
            }
        }
        flow::update(&mut list)?;
        self.steps = list;
        Ok(())
    }

    pub fn insert_step(&mut self, at: usize, step: ScriptStep) -> Result<()> {
        self.insert_steps(at, [step])
    }

    /// Delete a range of steps and their data.
    pub fn delete_steps(&mut self, range: Range<usize>) -> Result<Vec<ScriptStep>> {
        self.edit(|steps| {
            if range.start > range.end || range.end > steps.len() {
                return Err(ScriptError("The steps to delete are out of range.".into()));
            }
            Ok(steps.drain(range).collect())
        })
    }

    pub fn delete_step(&mut self, index: usize) -> Result<ScriptStep> {
        Ok(self.delete_steps(index..index + 1)?.remove(0))
    }

    /// Move a range of steps so that its first step ends up at `to` (an
    /// index into the list after the move).
    pub fn move_steps(&mut self, range: Range<usize>, to: usize) -> Result<()> {
        self.edit(|steps| {
            if range.start > range.end || range.end > steps.len() || to + range.len() > steps.len()
            {
                return Err(ScriptError("The steps to move are out of range.".into()));
            }
            let moved: Vec<ScriptStep> = steps.drain(range).collect();
            steps.splice(to..to, moved);
            Ok(())
        })
    }

    pub fn move_step(&mut self, from: usize, to: usize) -> Result<()> {
        self.move_steps(from..from + 1, to)
    }

    /// Enable or disable a range of steps. A disabled step leaves the block
    /// structure; its cached index is left as it was, as FileMaker does.
    pub fn set_steps_enabled(&mut self, range: Range<usize>, enabled: bool) -> Result<()> {
        self.edit(|steps| {
            let steps = steps
                .get_mut(range)
                .ok_or_else(|| ScriptError("The steps to enable are out of range.".into()))?;
            steps.iter_mut().for_each(|s| s.set_enabled(enabled));
            Ok(())
        })
    }

    pub fn set_step_enabled(&mut self, index: usize, enabled: bool) -> Result<()> {
        self.set_steps_enabled(index..index + 1, enabled)
    }

    /// Set a step's parameter by schema name (see
    /// [`ScriptStep::set_parameter`]); cached indexes follow (an "Exit after
    /// last" change moves Go to Record's End Loop index).
    pub fn set_parameter(&mut self, index: usize, name: &str, value: ParamValue) -> Result<()> {
        self.edit_step(index, |step| step.set_parameter(name, value))
    }

    /// Any other change to one step (its id, flags, data), refused when the
    /// result does not nest.
    pub fn edit_step<T>(
        &mut self,
        index: usize,
        f: impl FnOnce(&mut ScriptStep) -> Result<T>,
    ) -> Result<T> {
        self.edit(|steps| {
            let step = steps
                .get_mut(index)
                .ok_or_else(|| ScriptError(format!("There is no step {}.", index + 1)))?;
            f(step)
        })
    }
}

/// A step of a new step list ([`Script::replace_steps`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StepSource {
    /// The script's own step at this index, as stored.
    Kept(usize),
    /// A new step.
    New(ScriptStep),
}

fn default_name(id: u64) -> String {
    format!("Script {id}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::flow::{ELSE, ELSE_IF, END_IF, END_LOOP, EXIT_LOOP_IF, IF, LOOP};
    use crate::step::flag;

    #[test]
    fn folders_keep_their_empty_step_list_and_folder_ends_are_told_apart() {
        use om_hbam::{KeyTree, OwnedNode, SlotReader};
        let flags = |v: u64| OwnedNode::leaf([0xf0], v.to_be_bytes().to_vec());
        // As the FMS sample stores a folder: an empty step list (`4`) and
        // flags 0x0d; a folder end: flags 0x02, no step list.
        let folder = OwnedNode::list(
            [7],
            vec![OwnedNode::leaf([STEP_LIST], Vec::new()), flags(0x0d)],
        );
        let end = OwnedNode::list([8], vec![flags(0x02)]);
        let wire = OwnedNode::list(Vec::new(), vec![folder.clone(), end.clone()])
            .to_wire()
            .unwrap();
        let tree = KeyTree::from_slots(SlotReader::new(&wire)).unwrap();
        let decoded: Vec<Script> = tree
            .root
            .children
            .iter()
            .map(|n| Script::decode(n).unwrap())
            .collect();
        assert_eq!(decoded[0].kind, ScriptKind::Folder);
        assert_eq!(decoded[1].kind, ScriptKind::FolderEnd);
        assert_eq!(decoded[0].encode(), folder);
        assert_eq!(decoded[1].encode(), end);
    }

    fn script(ids: &[u16]) -> Script {
        let mut script = Script {
            id: 1,
            name: "Test".into(),
            ..Script::default()
        };
        script
            .insert_steps(0, ids.iter().map(|id| ScriptStep::new(*id)))
            .unwrap();
        script
    }

    fn values(script: &Script) -> Vec<u16> {
        script.steps.iter().map(|s| s.value).collect()
    }

    #[test]
    fn inserted_blocks_cache_their_partners() {
        // 0 If, 1 Else If, 2 Else, 3 End If, 4 Loop, 5 If, 6 Exit Loop If,
        // 7 End If, 8 End Loop
        let s = script(&[
            IF,
            ELSE_IF,
            ELSE,
            END_IF,
            LOOP,
            IF,
            EXIT_LOOP_IF,
            END_IF,
            END_LOOP,
        ]);
        assert_eq!(values(&s), [1, 2, 3, 0, 8, 7, 8, 0, 4]);
        // Else If also stores its End If index as numeric data.
        assert_eq!(s.steps[1].number(0), Some(3));
        // Fresh serial data keys, FileMaker's padding.
        assert_eq!(s.steps[0].data_key.0, [2, 1, 1, 0, 0xbe, 0xbe]);
        assert_eq!(s.steps[8].key(), Some(9));
    }

    #[test]
    fn full_access_is_bit_0_of_the_private_flags() {
        let mut s = script(&[7]);
        assert!(!s.runs_with_full_access());
        s.set_runs_with_full_access(true);
        let node = s.encode();
        assert_eq!(
            node.child(&[PRIVATE_FLAGS]).unwrap().value.as_deref(),
            Some(&[1][..])
        );
        let wire = OwnedNode::list(Vec::new(), vec![node]).to_wire().unwrap();
        let tree = KeyTree::from_slots(om_hbam::SlotReader::new(&wire)).unwrap();
        let decoded = Script::decode(tree.root.children.first().unwrap()).unwrap();
        assert!(decoded.runs_with_full_access());
        // Turning it off keeps a stored flags node, now 0.
        s.set_runs_with_full_access(false);
        assert!(!s.runs_with_full_access());
        assert_eq!(
            s.encode().child(&[PRIVATE_FLAGS]).unwrap().value.as_deref(),
            Some(&[0][..])
        );
    }

    #[test]
    fn edits_that_unbalance_the_script_are_refused() {
        let mut s = script(&[IF, 7, END_IF]);
        let before = s.clone();
        assert!(s.delete_step(2).unwrap_err().0.contains("If"));
        assert!(s.insert_step(0, ScriptStep::new(END_LOOP)).is_err());
        assert!(s.insert_step(1, ScriptStep::new(ELSE)).is_ok());
        assert!(s
            .insert_step(2, ScriptStep::new(ELSE_IF))
            .unwrap_err()
            .0
            .contains("follows an Else"));
        assert!(s.move_step(0, 3).is_err());
        assert!(s.set_step_enabled(0, false).is_err());
        assert!(s
            .edit_step(1, |st| {
                st.opcode = LOOP as i16;
                Ok(())
            })
            .is_err());
        assert!(s
            .insert_step(0, ScriptStep::new(EXIT_LOOP_IF))
            .unwrap_err()
            .0
            .contains("outside a Loop"));
        // Interleaved blocks.
        let mut t: Vec<ScriptStep> = [LOOP, IF, END_LOOP, END_IF].map(ScriptStep::new).into();
        assert!(flow::update(&mut t)
            .unwrap_err()
            .0
            .contains("closes the If"));
        s.delete_step(1).unwrap();
        assert_eq!(s, before);
    }

    #[test]
    fn deleting_moving_and_disabling_blocks_keep_indexes_right() {
        let mut s = script(&[7, IF, 7, ELSE, 7, END_IF, LOOP, END_LOOP]);
        assert_eq!(values(&s), [0, 3, 0, 5, 0, 0, 7, 6]);
        s.delete_steps(0..1).unwrap();
        assert_eq!(values(&s), [2, 0, 4, 0, 0, 6, 5]);
        // Move the Loop block in front of the If block.
        s.move_steps(5..7, 0).unwrap();
        assert_eq!(values(&s), [1, 0, 4, 0, 6, 0, 0]);
        // Disabling the whole If block takes it out of the structure; its
        // steps keep their stale indexes.
        s.set_steps_enabled(2..7, false).unwrap();
        assert_eq!(values(&s), [1, 0, 4, 0, 6, 0, 0]);
        s.insert_step(7, ScriptStep::new(LOOP)).unwrap_err();
        s.insert_steps(2, [ScriptStep::new(LOOP), ScriptStep::new(END_LOOP)])
            .unwrap();
        assert_eq!(values(&s)[..4], [1, 0, 3, 2]);
        // Re-enabling recomputes.
        s.set_steps_enabled(4..9, true).unwrap();
        assert_eq!(values(&s), [1, 0, 3, 2, 6, 0, 8, 0, 0]);
    }

    #[test]
    fn replaced_step_lists_keep_kept_steps_and_key_new_ones_afresh() {
        let mut s = script(&[IF, 7, END_IF, 99]);
        s.steps[1].set_unique_ids([7; 16], None);
        let kept = s.steps[1].clone();
        // Keys 1-4 are held; the deleted step 4's key is not reused.
        s.replace_steps([
            StepSource::Kept(0),
            StepSource::New(ScriptStep::new(LOOP)),
            StepSource::Kept(1),
            StepSource::New(ScriptStep::new(END_LOOP)),
            StepSource::Kept(2),
        ])
        .unwrap();
        assert_eq!(values(&s), [4, 3, 0, 1, 0]);
        assert_eq!(s.steps[2], kept);
        assert_eq!(s.steps[1].key(), Some(5));
        assert_eq!(s.steps[3].key(), Some(6));
        let before = s.clone();
        assert!(s.replace_steps([StepSource::Kept(0)]).is_err());
        assert!(s
            .replace_steps([StepSource::Kept(1), StepSource::Kept(1)])
            .unwrap_err()
            .0
            .contains("twice"));
        assert!(s.replace_steps([StepSource::Kept(9)]).is_err());
        assert_eq!(s, before);
    }

    #[test]
    fn go_to_record_caches_the_enclosing_end_loop() {
        // Inside a loop the End Loop index is stored with or without "Exit
        // after last"; outside, -1.
        let mut s = script(&[LOOP, 16, END_LOOP, 16]);
        assert_eq!(s.steps[1].number(0), Some(2));
        assert_eq!(s.steps[3].number(0), Some(-1));
        s.set_parameter(1, "Exit after last", ParamValue::Bool(true))
            .unwrap();
        assert!(s.steps[1].has_flag(flag::EXIT));
        assert_eq!(s.steps[1].number(0), Some(2));
        s.move_step(1, 3).unwrap();
        assert_eq!(s.steps[3].number(0), Some(-1));
        s.insert_steps(
            0,
            [
                ScriptStep::new(LOOP),
                ScriptStep::new(99),
                ScriptStep::new(END_LOOP),
            ],
        )
        .unwrap();
        assert_eq!(s.steps[1].number(0), Some(2));
    }

    #[test]
    fn hidden_scripts_set_bit_3_of_the_member_flags() {
        // FileMaker's XML for the Contacts template: folder 9 and scripts 8
        // are hidden="True", the folder end 2 is not.
        let mut s = script(&[7]);
        assert!(s.in_scripts_menu());
        s.set_in_scripts_menu(false);
        assert_eq!(
            s.encode().child(&[MEMBER_FLAGS]).unwrap().value.as_deref(),
            Some(&[8][..])
        );
        s.metadata
            .iter_mut()
            .find(|n| n.segment == [MEMBER_FLAGS])
            .unwrap()
            .value = Some(vec![9]);
        assert!(!s.in_scripts_menu());
        s.set_in_scripts_menu(true);
        assert_eq!(s.member_flags(), 1);
    }

    #[test]
    fn the_catalog_lists_members_in_display_order() {
        let def = |id: u64| OwnedNode::list(serial::encode(id), vec![OwnedNode::leaf([4], [])]);
        let entry = |n: u64, id: u64| {
            let segment = serial::encode(id);
            let mut value = vec![segment.len() as u8];
            value.extend(segment);
            OwnedNode::leaf(serial::encode(n), value)
        };
        let catalog = OwnedNode::list(
            [17],
            vec![
                OwnedNode::list(
                    [1],
                    vec![OwnedNode::list(
                        [3],
                        vec![entry(1, 3), entry(2, 141), entry(3, 2)],
                    )],
                ),
                OwnedNode::list([5], vec![def(2), def(3), def(5), def(141)]),
            ],
        );
        let wire = OwnedNode::list(Vec::new(), vec![catalog])
            .to_wire()
            .unwrap();
        let tree = KeyTree::from_slots(om_hbam::SlotReader::new(&wire)).unwrap();
        let ids: Vec<u64> = ScriptCatalog::decode(&tree)
            .unwrap()
            .scripts
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, [3, 141, 2, 5]);
    }

    #[test]
    fn transactions_cache_the_commit_index_twice() {
        let s = script(&[205, 7, 206]);
        assert_eq!(values(&s), [2, 0, 2]);
    }

    #[test]
    fn ids_are_the_serial_decode_of_the_key() {
        // Script key `80 0d` is script 141 (file-metadata.md 1).
        let mut s = script(&[7]);
        s.id = 141;
        let node = s.encode();
        assert_eq!(node.segment, [0x80, 0x0d]);
        let wire = OwnedNode::list(Vec::new(), vec![node]).to_wire().unwrap();
        let tree = KeyTree::from_slots(om_hbam::SlotReader::new(&wire)).unwrap();
        let catalog = ScriptCatalog {
            scripts: vec![Script::decode(tree.root.children.first().unwrap()).unwrap()],
            ..ScriptCatalog::default()
        };
        assert_eq!(catalog.scripts[0].id, 141);
        assert!(catalog.script(141).is_some());
        assert!(catalog.script(0x800d).is_none());
    }

    #[test]
    fn definitions_reencode_and_renames_rewrite_the_name() {
        let mut s = script(&[IF, END_IF]);
        s.steps[0].set_unique_ids([7; 16], None);
        s.metadata.push(text_node(&[NAME], "Test"));
        s.unreferenced_data.push(OwnedNode::list(
            [0x40],
            vec![OwnedNode::leaf([0xd8], [1; 16])],
        ));
        let node = s.encode();
        let segments: Vec<&[u8]> = node.children.iter().map(|c| c.segment.as_slice()).collect();
        assert_eq!(segments, [&[4][..], &[5], &[0x10]]);
        assert_eq!(node.child(&[5]).unwrap().children.len(), 2);
        s.name = "Renamed".into();
        assert_eq!(
            node_text(s.encode().child(&[0x10]).unwrap()).as_deref(),
            Some("Renamed")
        );
    }
}
