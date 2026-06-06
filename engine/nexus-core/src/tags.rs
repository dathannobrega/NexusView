//! Row tagging (Timeline Explorer-style "Tag" column).
//!
//! A tag is a per-row boolean keyed by the **absolute data-row index**, so it is
//! independent of the current filter, sort, or grouping — tagged rows stay
//! tagged no matter how the view changes. Storage is a lock-free atomic bitset
//! (1 bit/row, ~625 KB for 5 M rows), allocated lazily on the first tag so an
//! untagged session costs nothing.

use std::sync::atomic::{AtomicU64, Ordering};

/// A compact atomic bitset over `len` rows.
pub struct TagStore {
    words: Vec<AtomicU64>,
    len: usize,
}

impl TagStore {
    pub fn new(len: usize) -> Self {
        let word_count = len.div_ceil(64).max(1);
        let mut words = Vec::with_capacity(word_count);
        words.resize_with(word_count, || AtomicU64::new(0));
        TagStore { words, len }
    }

    /// Tag or untag `row`. Out-of-range rows are ignored.
    pub fn set(&self, row: usize, tagged: bool) {
        if row >= self.len {
            return;
        }
        let bit = 1u64 << (row % 64);
        if tagged {
            self.words[row / 64].fetch_or(bit, Ordering::Relaxed);
        } else {
            self.words[row / 64].fetch_and(!bit, Ordering::Relaxed);
        }
    }

    /// Is `row` tagged?
    pub fn get(&self, row: usize) -> bool {
        if row >= self.len {
            return false;
        }
        self.words[row / 64].load(Ordering::Relaxed) & (1u64 << (row % 64)) != 0
    }

    /// Total number of tagged rows.
    pub fn count(&self) -> usize {
        self.words
            .iter()
            .map(|w| w.load(Ordering::Relaxed).count_ones() as usize)
            .sum()
    }

    /// Untag everything.
    pub fn clear(&self) {
        for word in &self.words {
            word.store(0, Ordering::Relaxed);
        }
    }

    /// All tagged row indices, ascending.
    pub fn tagged_rows(&self) -> Vec<u32> {
        let mut rows = Vec::new();
        for (word_index, word) in self.words.iter().enumerate() {
            let mut bits = word.load(Ordering::Relaxed);
            while bits != 0 {
                let row = word_index * 64 + bits.trailing_zeros() as usize;
                if row < self.len {
                    rows.push(row as u32);
                }
                bits &= bits - 1; // clear lowest set bit
            }
        }
        rows
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_get_count_clear() {
        let store = TagStore::new(200);
        assert_eq!(store.count(), 0);
        store.set(5, true);
        store.set(130, true);
        store.set(5, true); // idempotent
        assert!(store.get(5));
        assert!(store.get(130));
        assert!(!store.get(6));
        assert_eq!(store.count(), 2);
        assert_eq!(store.tagged_rows(), vec![5, 130]);

        store.set(5, false);
        assert!(!store.get(5));
        assert_eq!(store.count(), 1);

        store.clear();
        assert_eq!(store.count(), 0);
    }

    #[test]
    fn out_of_range_is_safe() {
        let store = TagStore::new(10);
        store.set(999, true); // ignored, no panic
        assert!(!store.get(999));
        assert_eq!(store.count(), 0);
    }
}
