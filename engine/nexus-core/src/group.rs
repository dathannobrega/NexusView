//! Multi-level grouping (RF-03).
//!
//! Rows are grouped by N columns into a tree: each level partitions by a column
//! value, and the deepest level holds the rows themselves. The tree only
//! materializes *group* nodes (one per distinct value combination) plus a single
//! `u32` per row — it never allocates a node per row, so it scales to millions.
//!
//! The tree is consumed by an `NSOutlineView`: a group node's children are
//! sub-groups (more levels) or the rows in that group.

use rayon::prelude::*;
use smallvec::SmallVec;
use std::cmp::Ordering;

use crate::dataset::Dataset;
use crate::sort::CellKey;
use crate::view::View;

/// A node addresses either a group (by node index) or a single data row.
/// Encoded as `i64` for the FFI: `>= 0` is a group node id; `< 0` is the data
/// row `(-item - 1)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    Group(usize),
    Row(u32),
}

impl Item {
    pub fn encode(self) -> i64 {
        match self {
            Item::Group(n) => n as i64,
            Item::Row(r) => -(r as i64) - 1,
        }
    }

    pub fn decode(item: i64) -> Item {
        if item >= 0 {
            Item::Group(item as usize)
        } else {
            Item::Row((-item - 1) as u32)
        }
    }
}

enum Children {
    /// Sub-group node ids.
    Groups(Vec<u32>),
    /// A contiguous range into `GroupTree::row_ids` (deepest level).
    Rows { start: usize, len: usize },
}

struct GroupNode {
    label: String,
    count: u64,
    children: Children,
}

/// A built grouping tree.
pub struct GroupTree {
    nodes: Vec<GroupNode>,
    /// All grouped rows, partitioned so each leaf group owns a contiguous range.
    row_ids: Vec<u32>,
    /// Top-level group node ids.
    roots: Vec<u32>,
}

/// Build a grouping tree for `view` over `cols` (in nesting order).
pub fn build(dataset: &Dataset, view: &View, cols: &[usize]) -> GroupTree {
    let ids: Vec<u32> = view.iter().collect();
    if cols.is_empty() || ids.is_empty() {
        return GroupTree {
            nodes: Vec::new(),
            row_ids: ids,
            roots: Vec::new(),
        };
    }

    // Precompute ordering keys for each row (one parse per group cell).
    let keys: Vec<SmallVec<[CellKey; 4]>> = ids
        .par_iter()
        .map(|&id| {
            cols.iter()
                .map(|&c| CellKey::parse(&dataset.cell(id as usize, c)))
                .collect()
        })
        .collect();

    // Order rows so identical key tuples are contiguous (and groups appear in
    // natural, numeric-aware order).
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.par_sort_by(|&a, &b| {
        for (ka, kb) in keys[a].iter().zip(keys[b].iter()) {
            let ord = ka.cmp(kb);
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });

    let sorted_ids: Vec<u32> = order.iter().map(|&i| ids[i]).collect();
    let sorted_keys: Vec<&SmallVec<[CellKey; 4]>> = order.iter().map(|&i| &keys[i]).collect();

    let mut nodes: Vec<GroupNode> = Vec::new();
    let roots = build_level(
        dataset,
        cols,
        &sorted_ids,
        &sorted_keys,
        0,
        0,
        sorted_ids.len(),
        &mut nodes,
    );

    GroupTree {
        nodes,
        row_ids: sorted_ids,
        roots,
    }
}

/// Recursively build group nodes for `[start, end)` at `depth`, returning the
/// node ids created at this level.
#[allow(clippy::too_many_arguments)]
fn build_level(
    dataset: &Dataset,
    cols: &[usize],
    sorted_ids: &[u32],
    sorted_keys: &[&SmallVec<[CellKey; 4]>],
    depth: usize,
    start: usize,
    end: usize,
    nodes: &mut Vec<GroupNode>,
) -> Vec<u32> {
    let mut result = Vec::new();
    let mut i = start;
    while i < end {
        // Extend the run while this level's key stays constant.
        let mut j = i + 1;
        while j < end && sorted_keys[j][depth] == sorted_keys[i][depth] {
            j += 1;
        }

        let label = dataset.cell(sorted_ids[i] as usize, cols[depth]);
        let count = (j - i) as u64;
        let children = if depth + 1 < cols.len() {
            Children::Groups(build_level(
                dataset,
                cols,
                sorted_ids,
                sorted_keys,
                depth + 1,
                i,
                j,
                nodes,
            ))
        } else {
            Children::Rows {
                start: i,
                len: j - i,
            }
        };

        let id = nodes.len() as u32;
        nodes.push(GroupNode {
            label,
            count,
            children,
        });
        result.push(id);
        i = j;
    }
    result
}

impl GroupTree {
    /// Number of top-level groups.
    pub fn root_count(&self) -> usize {
        self.roots.len()
    }

    /// The i-th top-level group as an [`Item`], or `None` if out of range.
    pub fn root_child(&self, index: usize) -> Option<Item> {
        self.roots.get(index).map(|&n| Item::Group(n as usize))
    }

    /// Number of children of `item` (sub-groups or rows). Rows have none.
    pub fn child_count(&self, item: Item) -> usize {
        match item {
            Item::Group(n) => match self.nodes.get(n) {
                Some(node) => match &node.children {
                    Children::Groups(g) => g.len(),
                    Children::Rows { len, .. } => *len,
                },
                None => 0,
            },
            Item::Row(_) => 0,
        }
    }

    /// The i-th child of `item`, or `None` if out of range / item is a row.
    pub fn child(&self, item: Item, index: usize) -> Option<Item> {
        let Item::Group(n) = item else { return None };
        let node = self.nodes.get(n)?;
        match &node.children {
            Children::Groups(g) => g.get(index).map(|&c| Item::Group(c as usize)),
            Children::Rows { start, len } => {
                if index < *len {
                    self.row_ids.get(start + index).map(|&r| Item::Row(r))
                } else {
                    None
                }
            }
        }
    }

    /// True when `item` is an expandable group node.
    pub fn is_group(&self, item: Item) -> bool {
        matches!(item, Item::Group(n) if n < self.nodes.len())
    }

    /// Group label (the column value). Empty for row items.
    pub fn label(&self, item: Item) -> &str {
        match item {
            Item::Group(n) => self.nodes.get(n).map_or("", |x| x.label.as_str()),
            Item::Row(_) => "",
        }
    }

    /// Number of rows under `item` (group aggregate, or 1 for a row).
    pub fn count(&self, item: Item) -> u64 {
        match item {
            Item::Group(n) => self.nodes.get(n).map_or(0, |x| x.count),
            Item::Row(_) => 1,
        }
    }

    /// The data row id for a row item, or `None` for a group.
    pub fn row(&self, item: Item) -> Option<u32> {
        match item {
            Item::Row(r) => Some(r),
            Item::Group(_) => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ParserSchema;
    use std::io::Write;

    fn dataset(bytes: &[u8]) -> (Dataset, tempfile::NamedTempFile) {
        let mut tf = tempfile::NamedTempFile::new().unwrap();
        tf.write_all(bytes).unwrap();
        tf.flush().unwrap();
        (
            Dataset::open(tf.path(), Some(ParserSchema::csv())).unwrap(),
            tf,
        )
    }

    #[test]
    fn single_level_grouping() {
        let (ds, _t) = dataset(b"sev,msg\nINFO,a\nERROR,b\nINFO,c\nERROR,d\nINFO,e\n");
        let tree = build(&ds, &ds.view_all(), &[0]);
        assert_eq!(tree.root_count(), 2); // ERROR, INFO (sorted)

        let g0 = tree.root_child(0).unwrap();
        assert_eq!(tree.label(g0), "ERROR");
        assert_eq!(tree.count(g0), 2);
        assert_eq!(tree.child_count(g0), 2);

        let g1 = tree.root_child(1).unwrap();
        assert_eq!(tree.label(g1), "INFO");
        assert_eq!(tree.count(g1), 3);

        // First child of ERROR group is a row item.
        let row = tree.child(g0, 0).unwrap();
        assert!(!tree.is_group(row));
        assert!(tree.row(row).is_some());
    }

    #[test]
    fn two_level_grouping() {
        let (ds, _t) =
            dataset(b"host,sev,msg\nweb01,INFO,a\nweb01,ERROR,b\nweb01,INFO,c\nweb02,ERROR,d\n");
        let tree = build(&ds, &ds.view_all(), &[0, 1]);
        assert_eq!(tree.root_count(), 2); // web01, web02

        let web01 = tree.root_child(0).unwrap();
        assert_eq!(tree.label(web01), "web01");
        assert_eq!(tree.count(web01), 3);
        // web01 has two severity sub-groups: ERROR(1), INFO(2)
        assert_eq!(tree.child_count(web01), 2);
        let err = tree.child(web01, 0).unwrap();
        assert!(tree.is_group(err));
        assert_eq!(tree.label(err), "ERROR");
        assert_eq!(tree.count(err), 1);
        let info = tree.child(web01, 1).unwrap();
        assert_eq!(tree.label(info), "INFO");
        assert_eq!(tree.count(info), 2);
        assert_eq!(tree.child_count(info), 2); // two row leaves
    }

    #[test]
    fn item_encoding_roundtrip() {
        assert_eq!(Item::decode(Item::Group(42).encode()), Item::Group(42));
        assert_eq!(Item::decode(Item::Row(0).encode()), Item::Row(0));
        assert_eq!(Item::decode(Item::Row(99).encode()), Item::Row(99));
    }
}
