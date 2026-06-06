//! A view is an ordered selection of data rows — the result of a search/filter.
//!
//! Views are immutable and cheap to swap, which is what keeps the UI thread-safe
//! (RNF-01): a background search produces a new `View`, and the UI atomically
//! replaces the active one. The identity view (`All`) carries no allocation, so
//! "show everything" over a 50 M-row file costs nothing.

/// An ordered set of data-row indices.
#[derive(Debug, Clone)]
pub enum View {
    /// All rows `0..count`, in natural order. No backing allocation.
    All(u64),
    /// An explicit, ordered subset of data-row indices.
    Filtered(Vec<u32>),
}

impl View {
    /// Number of rows in the view.
    #[inline]
    pub fn len(&self) -> u64 {
        match self {
            View::All(n) => *n,
            View::Filtered(ids) => ids.len() as u64,
        }
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Map a position in the view to the underlying data-row index.
    /// Returns `None` when `view_row` is out of range — never panics.
    #[inline]
    pub fn row_id(&self, view_row: u64) -> Option<u32> {
        match self {
            View::All(n) => (view_row < *n).then_some(view_row as u32),
            View::Filtered(ids) => ids.get(view_row as usize).copied(),
        }
    }

    /// Iterate the underlying data-row indices in view order.
    pub fn iter(&self) -> Box<dyn Iterator<Item = u32> + '_> {
        match self {
            View::All(n) => Box::new(0..(*n as u32)),
            View::Filtered(ids) => Box::new(ids.iter().copied()),
        }
    }
}
