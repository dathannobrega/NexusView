//! Read-only memory mapping (RF-02, RNF-02).
//!
//! The file is never loaded into the heap. The OS maps it and pages data in on
//! demand, so opening a 50 GB evidence file costs almost nothing up front and
//! the resident footprint stays bounded by the working set actually touched.

use crate::error::{NexusError, Result};
use memmap2::{Advice, Mmap};
use std::fs::File;
use std::path::Path;

/// An immutable memory map over a file plus the keep-alive file handle.
pub struct MappedFile {
    // Kept alive for the lifetime of the mapping; the map borrows the fd.
    _file: File,
    mmap: Mmap,
}

impl MappedFile {
    /// Map `path` read-only.
    ///
    /// # Errors
    /// Returns [`NexusError::EmptyFile`] for zero-length files (nothing to map)
    /// and [`NexusError::Mmap`] if the kernel refuses the mapping.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let file = File::open(path)?;
        let len = file.metadata()?.len();
        if len == 0 {
            return Err(NexusError::EmptyFile);
        }

        // SAFETY: The mapping is read-only and we treat its bytes as immutable.
        // The one external hazard is another process truncating the file while
        // mapped, which can raise SIGBUS. Triage targets are static evidence
        // files, so this is an accepted, documented constraint.
        let mmap = unsafe { Mmap::map(&file) }.map_err(|e| NexusError::Mmap(e.to_string()))?;

        Ok(Self { _file: file, mmap })
    }

    /// The full file contents as an immutable byte slice (zero-copy).
    #[inline]
    pub fn bytes(&self) -> &[u8] {
        &self.mmap
    }

    /// Hint the kernel that the next pass is a sequential scan (used while the
    /// line index is built). Best-effort; failure is ignored.
    pub fn advise_sequential(&self) {
        let _ = self.mmap.advise(Advice::Sequential);
    }
}
