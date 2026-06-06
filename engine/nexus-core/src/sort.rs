//! Stable multi-column sort (RF-05).
//!
//! Sorting reorders a permutation of row ids — strings are never moved in RAM.
//! Each key cell is parsed once into a totally-ordered key, then a stable
//! parallel sort orders the ids. Numeric columns sort numerically; everything
//! else sorts case-insensitively. Numbers sort before text.

use rayon::prelude::*;
use smallvec::SmallVec;
use std::cmp::Ordering;

use crate::dataset::Dataset;
use crate::view::View;

/// One level of a multi-column sort.
#[derive(Debug, Clone, Copy)]
pub struct SortKey {
    pub col: usize,
    pub ascending: bool,
}

/// A precomputed, totally-ordered key for one cell.
///
/// The derived `Ord` gives exactly the ordering we want: the `Num` variant is
/// declared before `Text`, so numbers sort before text; within `Num` the
/// order-preserving `u64` encoding sorts numerically; within `Text`, bytes sort
/// lexicographically.
#[derive(PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum CellKey {
    Num(u64),
    Text(String),
}

impl CellKey {
    pub(crate) fn parse(value: &str) -> Self {
        let trimmed = value.trim();
        if !trimmed.is_empty() {
            if let Ok(f) = trimmed.parse::<f64>() {
                if !f.is_nan() {
                    return CellKey::Num(f64_sortable(f));
                }
            }
        }
        CellKey::Text(trimmed.to_ascii_lowercase())
    }
}

/// Map an `f64` to a `u64` whose unsigned ordering matches IEEE-754 ordering
/// (the standard total-order bit trick). Inputs are never NaN here.
fn f64_sortable(f: f64) -> u64 {
    let bits = f.to_bits() as i64;
    let mask = ((bits >> 63) as u64) | 0x8000_0000_0000_0000;
    (bits as u64) ^ mask
}

/// Sort the rows of `view` by `keys` (stable), returning a new view. An empty
/// key list returns the view unchanged.
pub fn sort(dataset: &Dataset, view: &View, keys: &[SortKey]) -> View {
    if keys.is_empty() {
        return view.clone();
    }

    let ids: Vec<u32> = view.iter().collect();

    // Parse each row's key cells once, in parallel.
    let computed: Vec<SmallVec<[CellKey; 4]>> = ids
        .par_iter()
        .map(|&id| {
            keys.iter()
                .map(|k| CellKey::parse(&dataset.cell(id as usize, k.col)))
                .collect()
        })
        .collect();

    // Stable parallel sort of an index permutation: equal keys keep the view's
    // existing order, which is what makes successive sorts compose predictably.
    let mut order: Vec<usize> = (0..ids.len()).collect();
    order.par_sort_by(|&a, &b| {
        for (level, key) in keys.iter().enumerate() {
            let ord = computed[a][level].cmp(&computed[b][level]);
            let ord = if key.ascending { ord } else { ord.reverse() };
            if ord != Ordering::Equal {
                return ord;
            }
        }
        Ordering::Equal
    });

    View::Filtered(order.into_iter().map(|i| ids[i]).collect())
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

    fn key(col: usize, ascending: bool) -> SortKey {
        SortKey { col, ascending }
    }

    #[test]
    fn numeric_not_lexicographic() {
        let (ds, _t) = dataset(b"n\n10\n2\n1\n100\n");
        let v = sort(&ds, &ds.view_all(), &[key(0, true)]);
        let got: Vec<String> = v.iter().map(|r| ds.cell(r as usize, 0)).collect();
        assert_eq!(got, vec!["1", "2", "10", "100"]);
    }

    #[test]
    fn descending() {
        let (ds, _t) = dataset(b"n\n1\n3\n2\n");
        let v = sort(&ds, &ds.view_all(), &[key(0, false)]);
        let got: Vec<String> = v.iter().map(|r| ds.cell(r as usize, 0)).collect();
        assert_eq!(got, vec!["3", "2", "1"]);
    }

    #[test]
    fn multi_key_with_stable_tiebreak() {
        // Sort by group asc, then by value desc.
        let (ds, _t) = dataset(b"group,value\nb,1\na,5\nb,9\na,5\na,2\n");
        let v = sort(&ds, &ds.view_all(), &[key(0, true), key(1, false)]);
        let rows: Vec<(String, String)> = v
            .iter()
            .map(|r| (ds.cell(r as usize, 0), ds.cell(r as usize, 1)))
            .collect();
        assert_eq!(
            rows,
            vec![
                ("a".into(), "5".into()),
                ("a".into(), "5".into()),
                ("a".into(), "2".into()),
                ("b".into(), "9".into()),
                ("b".into(), "1".into()),
            ]
        );
    }

    #[test]
    fn text_case_insensitive() {
        let (ds, _t) = dataset(b"name\nBravo\nalpha\nCharlie\n");
        let v = sort(&ds, &ds.view_all(), &[key(0, true)]);
        let got: Vec<String> = v.iter().map(|r| ds.cell(r as usize, 0)).collect();
        assert_eq!(got, vec!["alpha", "Bravo", "Charlie"]);
    }

    #[test]
    fn empty_keys_is_identity() {
        let (ds, _t) = dataset(b"n\n3\n1\n2\n");
        let v = sort(&ds, &ds.view_all(), &[]);
        assert_eq!(v.len(), 3);
    }
}
