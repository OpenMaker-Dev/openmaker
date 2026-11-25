//! A report for people: how a file's paths fall into the groups, the paths
//! no rule covers, and the catalog members that are no design object, so
//! that gaps in the rules show when a FileMaker version adds keys.
//!
//! Paths are counted at their terminal nodes: leaves, and lists with no
//! children. A path no rule covers is reported by its **shape**: the part
//! the rules recognise, written as their patterns (`T` a table, `O` an
//! occurrence, `*` an id), then the first segment no rule names in hex, and
//! `/…` when the path goes deeper. An empty list where only deeper paths
//! have rules (an empty catalog) is structure, not a gap.

use std::collections::BTreeMap;
use std::fmt;

use super::rules::{hex, Group, Rule, Seg, CATALOGS};
use super::{instances, member_of, rules, secrets, step, Path};
use crate::tree::OwnedNode;

/// What a report found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Report {
    /// Terminal paths per group; [`Group::None`] counts those a rule puts
    /// there (the file as a whole), not the unclassified ones.
    pub groups: BTreeMap<Group, usize>,
    /// Empty lists where only deeper paths have rules.
    pub structure: usize,
    /// Terminal paths no rule covers, by shape.
    pub unclassified: BTreeMap<String, usize>,
    /// Terminal paths each rule covered, by its index in [`rules()`].
    pub hits: BTreeMap<usize, usize>,
    /// The catalog members that are no design object.
    pub members: Vec<Member>,
}

/// A catalog member that is no design object.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Member {
    /// The catalog's name, or `unknown catalog` and its path.
    pub catalog: String,
    pub path: Path,
    pub is: MemberIs,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum MemberIs {
    /// A member of an Access catalog (an account, a file access entry).
    Access,
    /// A member of a Data catalog (a table library's container value).
    Data,
    /// A member with no keys: a deleted one (scripts.md).
    Emptied,
    /// A member of a catalog no rule names.
    Unknown,
}

impl Report {
    /// Terminal paths no rule covers.
    pub fn unclassified_count(&self) -> usize {
        self.unclassified.values().sum()
    }

    fn add(&mut self, rule: &'static Rule, count: usize) {
        *self.groups.entry(rule.group).or_default() += count;
        // The rule's place in the table, from its address.
        let base = rules().as_ptr() as usize;
        let index = (rule as *const Rule as usize - base) / std::mem::size_of::<Rule>();
        *self.hits.entry(index).or_default() += count;
    }
}

/// The number of terminal nodes in `node`'s subtree, `node` included.
fn terminals(node: &OwnedNode) -> usize {
    let mut count = 0;
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.children.is_empty() {
            count += 1;
        }
        stack.extend(&n.children);
    }
    count
}

/// The shape of a path no rule covers below `known` segments, written by
/// `cands` (rules that matched those): see the module doc.
fn shape(path: &[&[u8]], known: usize, cands: &[&'static Rule], deeper: bool) -> String {
    let mut parts: Vec<String> = (0..known)
        .map(|i| match cands.first() {
            Some(rule) => match rule.pattern[i] {
                Seg::Key(_) => hex(path[i]),
                other => other.text(),
            },
            None => hex(path[i]),
        })
        .collect();
    parts.push(hex(path[known]));
    let mut text = format!("[{}]", parts.join(", "));
    if deeper {
        text.insert_str(text.len() - 1, ", …");
    }
    text
}

/// The Secret paths of the member a walk is in: the member's depth and
/// the paths, relative to it.
struct Secrets {
    depth: usize,
    paths: Vec<Path>,
}

impl Secrets {
    fn at(path: &[&[u8]], member: &OwnedNode) -> Option<Self> {
        let (kind, depth) = member_of(path).filter(|&(_, depth)| depth == path.len())?;
        let paths = secrets::of(kind, member)
            .into_iter()
            .map(|s| s.path)
            .collect();
        Some(Secrets { depth, paths })
    }

    fn is(&self, path: &[&[u8]]) -> bool {
        let below = &path[self.depth..];
        self.paths
            .iter()
            .any(|s| s.len() == below.len() && s.iter().zip(below).all(|(a, b)| a == b))
    }

    fn below(&self, path: &[&[u8]]) -> bool {
        let below = &path[self.depth..];
        self.paths
            .iter()
            .any(|s| s.len() > below.len() && s.iter().zip(below).all(|(a, b)| a == b))
    }
}

/// Count `node`'s children, at `path`, into `report`.
pub(super) fn count_under(node: &OwnedNode, path: &[&[u8]], report: &mut Report) {
    let (cands, best) = super::candidates(path);
    let secrets = Secrets::at(path, node);
    let mut at: Vec<&[u8]> = path.to_vec();
    walk(node, &mut at, &cands, best, secrets.as_ref(), report);
}

fn walk<'t>(
    node: &'t OwnedNode,
    path: &mut Vec<&'t [u8]>,
    cands: &[&'static Rule],
    best: Option<&'static Rule>,
    secrets: Option<&Secrets>,
    report: &mut Report,
) {
    let depth = path.len();
    for child in &node.children {
        let (deeper, rule) = step(cands, depth, &child.segment, best);
        path.push(&child.segment);
        let entered = match secrets {
            None => Secrets::at(path, child),
            Some(_) => None,
        };
        let secrets = secrets.or(entered.as_ref());
        if secrets.is_some_and(|s| s.is(path)) {
            *report.groups.entry(Group::Secret).or_default() += terminals(child);
        } else if (deeper.is_empty() && !secrets.is_some_and(|s| s.below(path)))
            || child.children.is_empty()
        {
            match rule {
                Some(rule) => report.add(rule, terminals(child)),
                None if !deeper.is_empty() && child.value.is_none() => report.structure += 1,
                None => {
                    let text = shape(path, depth, cands, !child.children.is_empty());
                    *report.unclassified.entry(text).or_default() += terminals(child);
                }
            }
        } else {
            walk(child, path, &deeper, rule, secrets, report);
        }
        path.pop();
    }
}

/// The report for a file's whole tree.
pub fn report(root: &OwnedNode) -> Report {
    let mut report = Report::default();
    count_under(root, &[], &mut report);
    members(root, &mut report.members);
    report
}

/// The members of every catalog that are no object, and of every node that
/// looks like a catalog no rule names: a top-level node, or one under the
/// schema `[3]` or security `[17]` catalogs, with members `05` (file-format.md
/// 3.4's convention) and no rule covering it.
fn members(root: &OwnedNode, out: &mut Vec<Member>) {
    for catalog in CATALOGS {
        for (_, path, node) in instances(root, catalog) {
            let Some(list) = node.child(&[0x05]) else {
                continue;
            };
            for member in &list.children {
                let is = if member.children.is_empty() {
                    MemberIs::Emptied
                } else {
                    match catalog.group {
                        Group::Access => MemberIs::Access,
                        Group::Data => MemberIs::Data,
                        _ => continue,
                    }
                };
                let mut at = path.clone();
                at.extend([vec![0x05], member.segment.clone()]);
                out.push(Member {
                    catalog: catalog.name.into(),
                    path: at,
                    is,
                });
            }
        }
    }
    let mut unknown = |path: Vec<Vec<u8>>, node: &OwnedNode| {
        // Covered: a rule covers the node, or goes below it (a catalog the
        // table lists).
        let covered = rules()
            .iter()
            .any(|r| r.pattern.iter().zip(&path).all(|(p, s)| p.matches(s)));
        if covered {
            return;
        }
        let Some(list) = node.child(&[0x05]) else {
            return;
        };
        for member in &list.children {
            let mut at = path.clone();
            at.extend([vec![0x05], member.segment.clone()]);
            out.push(Member {
                catalog: format!("unknown catalog [{}]", hex_path(&path)),
                path: at,
                is: MemberIs::Unknown,
            });
        }
    };
    for top in &root.children {
        if matches!(top.segment.as_slice(), [0x03] | [0x17]) {
            for second in &top.children {
                unknown(vec![top.segment.clone(), second.segment.clone()], second);
            }
        } else if !Seg::Table.matches(&top.segment) && !Seg::Occurrence.matches(&top.segment) {
            unknown(vec![top.segment.clone()], top);
        }
    }
}

fn hex_path(path: &[Vec<u8>]) -> String {
    path.iter().map(|s| hex(s)).collect::<Vec<_>>().join(", ")
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "terminal paths by group:")?;
        for (group, count) in &self.groups {
            let label = match group {
                Group::None => "none (the file as a whole)",
                other => other.name(),
            };
            writeln!(f, "  {label:<28} {count:>9}")?;
        }
        writeln!(
            f,
            "  {:<28} {:>9}",
            "structure (empty lists)", self.structure
        )?;
        writeln!(
            f,
            "  {:<28} {:>9}",
            "in no group, no rule",
            self.unclassified_count()
        )?;
        if !self.unclassified.is_empty() {
            writeln!(f, "paths no rule covers, by shape:")?;
            for (shape, count) in &self.unclassified {
                writeln!(f, "  {count:>9}  {shape}")?;
            }
        }
        let mut by: BTreeMap<(&str, MemberIs), usize> = BTreeMap::new();
        for member in &self.members {
            *by.entry((&member.catalog, member.is)).or_default() += 1;
        }
        if !by.is_empty() {
            writeln!(f, "catalog members that are no design object:")?;
            for ((catalog, is), count) in by {
                writeln!(f, "  {count:>9}  {catalog}: {is:?}")?;
            }
        }
        Ok(())
    }
}
