//! Checking a whole version of a tree: every page read and checked, and the
//! rules that span pages: keys in order and within their separators, no
//! empty page below the root, no index root with one child, every node's
//! parent present and a list. What
//! a background check (`docs/server-storage.md` section 6.12) and the tests
//! run.

use std::sync::Arc;

use super::key;
use super::read::{Pages, Result};
use super::{fault, Root};
use crate::page::{Fault, Hash, Page, PageKind, PageNo, PageStore, KIND_VALUE};

/// What a check found in a sound tree.
#[derive(Clone, Debug, Default)]
pub struct Report {
    /// Levels of pages, leaves included.
    pub height: usize,
    pub items: usize,
    pub leaves: usize,
    pub index_pages: usize,
    pub overflow_pages: usize,
    /// Items whose key and value spill to overflow pages.
    pub spilled_items: usize,
    /// Separators that spill to overflow pages.
    pub spilled_separators: usize,
    /// Every page the tree reaches, in depth-first order.
    pub pages: Vec<(PageNo, Hash)>,
}

struct Walk<'a, 's, S: ?Sized> {
    pages: &'a Pages<'s, S>,
    report: Report,
    last: Option<Vec<u8>>,
    /// The keys of the lists that are open at the current key: its
    /// ancestors, as far as seen.
    open: Vec<Vec<u8>>,
}

fn malformed(page: &Page, why: &'static str) -> super::Error {
    fault(page.number(), page.hash(), Fault::Malformed(why))
}

/// Reads every page of the version `root` names and checks it.
pub fn check<S: PageStore + ?Sized>(store: &S, root: Root) -> Result<Report> {
    let pages = Pages { store };
    let top = pages.root(root)?;
    let mut walk = Walk {
        pages: &pages,
        report: Report {
            height: usize::from(top.level()) + 1,
            ..Report::default()
        },
        last: None,
        open: Vec::new(),
    };
    walk.page(top, None, None, true)?;
    Ok(walk.report)
}

impl<'a, 's, S: PageStore + ?Sized> Walk<'a, 's, S> {
    fn page(
        &mut self,
        page: Arc<Page>,
        lo: Option<&[u8]>,
        hi: Option<&[u8]>,
        root: bool,
    ) -> Result<()> {
        self.report.pages.push((page.number(), page.hash()));
        match page.kind() {
            PageKind::Leaf => {
                self.report.leaves += 1;
                if page.count() == 0 && !root {
                    return Err(malformed(&page, "an empty leaf below the root"));
                }
                for i in 0..page.count() {
                    let key = self.pages.item_key(&page, i)?;
                    let node = self.pages.item_node(&page, i)?;
                    let item = page.item(i);
                    if item.refs.len() > 0 {
                        self.report.spilled_items += 1;
                        self.report.overflow_pages += item.refs.len();
                        self.report.pages.extend(item.refs.iter());
                    }
                    if lo.is_some_and(|lo| key.as_slice() < lo)
                        || hi.is_some_and(|hi| key.as_slice() >= hi)
                    {
                        return Err(malformed(&page, "a key outside its separators"));
                    }
                    if self.last.as_ref().is_some_and(|last| *last >= key) {
                        return Err(malformed(&page, "keys out of order"));
                    }
                    // The parent is the longest open list that begins the key.
                    while self.open.last().is_some_and(|open| !key.starts_with(open)) {
                        self.open.pop();
                    }
                    let depth = key::Key(key.clone()).depth();
                    let parent_depth = self.open.last().map_or(0, |k| key::Key(k.clone()).depth());
                    if parent_depth + 1 != depth {
                        return Err(malformed(&page, "a node without its parent"));
                    }
                    if node.kind() != KIND_VALUE {
                        self.open.push(key.clone());
                    }
                    self.report.items += 1;
                    self.last = Some(key);
                }
            }
            PageKind::Index => {
                self.report.index_pages += 1;
                if root && page.count() == 1 {
                    return Err(malformed(&page, "an index root with one child"));
                }
                let mut seps = Vec::with_capacity(page.count());
                for c in 1..page.count() {
                    let separator = page.child(c).separator;
                    if let Some(s) = separator.filter(|s| s.refs.len() > 0) {
                        self.report.spilled_separators += 1;
                        self.report.overflow_pages += s.refs.len();
                        self.report.pages.extend(s.refs.iter());
                    }
                    let sep = self.pages.separator(&page, c)?;
                    if lo.is_some_and(|lo| sep.as_slice() <= lo)
                        || hi.is_some_and(|hi| sep.as_slice() >= hi)
                    {
                        return Err(malformed(&page, "a separator outside its page's range"));
                    }
                    seps.push(sep);
                }
                for c in 0..page.count() {
                    let child_lo = if c == 0 {
                        lo
                    } else {
                        Some(seps[c - 1].as_slice())
                    };
                    let child_hi = seps.get(c).map(Vec::as_slice).or(hi);
                    let child = self.pages.child(&page, c)?;
                    self.page(child, child_lo, child_hi, false)?;
                }
            }
            PageKind::Overflow => return Err(malformed(&page, "an overflow page in the tree")),
        }
        Ok(())
    }
}
