//! Opportunistic block-level trigram Bloom filters (RF-04).
//!
//! Rows are partitioned into fixed-size blocks. For each block we record, in a
//! small Bloom bitset, the character trigrams present anywhere in that block. A
//! substring search can then skip an entire block whenever *any* trigram of the
//! needle is absent from the block's filter — no row in the block can contain
//! the needle.
//!
//! This is exact: a Bloom hit may be a false positive (the block is scanned
//! anyway), but a Bloom miss is always a true negative, so no match is ever
//! lost. The win is large for selective needles (rare IOCs) over big files,
//! which is the hot path for triage. The index is built lazily on the first
//! substring search and reused afterwards.

use rayon::prelude::*;

use crate::dataset::Dataset;

/// Rows per block. Smaller blocks → fewer trigrams each → lower false-positive
/// rate, at the cost of more (cheap) block checks.
const BLOCK_SIZE: usize = 4096;
/// Bits per block's filter (power of two). 32 Ki bits = 4 KiB/block.
const BITS: usize = 32 * 1024;
/// Number of hash probes per trigram.
const PROBES: u64 = 2;

const WORDS_PER_BLOCK: usize = BITS / 64;
const BIT_MASK: usize = BITS - 1;

/// Per-block trigram Bloom filters over a dataset's rows.
pub struct BlockBloom {
    /// Concatenated bitsets: block `b` occupies `[b*WPB .. (b+1)*WPB]`.
    words: Vec<u64>,
    num_blocks: usize,
}

impl BlockBloom {
    /// Build the filters for every data row of `dataset` (parallel by block).
    pub fn build(dataset: &Dataset) -> Self {
        let n = dataset.row_count();
        let num_blocks = n.div_ceil(BLOCK_SIZE);
        let mut words = vec![0u64; num_blocks.max(1) * WORDS_PER_BLOCK];

        words
            .par_chunks_mut(WORDS_PER_BLOCK)
            .enumerate()
            .for_each(|(block, bits)| {
                let start = block * BLOCK_SIZE;
                let end = (start + BLOCK_SIZE).min(n);
                for row in start..end {
                    if let Some(line) = dataset.line_bytes(row) {
                        insert_trigrams(line, bits);
                    }
                }
            });

        BlockBloom { words, num_blocks }
    }

    pub fn block_size(&self) -> usize {
        BLOCK_SIZE
    }

    pub fn num_blocks(&self) -> usize {
        self.num_blocks
    }

    /// Could block `block` contain `needle`? `needle` must be ASCII-lowercased.
    /// Always returns `true` for needles shorter than a trigram (no signal).
    pub fn block_might_contain(&self, block: usize, needle: &[u8]) -> bool {
        if needle.len() < 3 || block >= self.num_blocks {
            return true;
        }
        let bits = &self.words[block * WORDS_PER_BLOCK..(block + 1) * WORDS_PER_BLOCK];
        for window in needle.windows(3) {
            let (h1, h2) = trigram_hashes(window[0], window[1], window[2]);
            let mut present = true;
            for k in 0..PROBES {
                let idx = (h1.wrapping_add(k.wrapping_mul(h2)) as usize) & BIT_MASK;
                if bits[idx / 64] & (1u64 << (idx % 64)) == 0 {
                    present = false;
                    break;
                }
            }
            if !present {
                return false; // a needle trigram is absent → block cannot match
            }
        }
        true
    }
}

/// Run a single-substring search accelerated by `bloom`. `col` scopes the match
/// to one column (`None` = any column). Returns matching data rows in ascending
/// order — identical to a brute-force scan.
pub fn search_substring(
    dataset: &Dataset,
    bloom: &BlockBloom,
    col: Option<usize>,
    needle: &[u8],
) -> Vec<u32> {
    let n = dataset.row_count();
    let block_size = bloom.block_size();

    (0..bloom.num_blocks())
        .into_par_iter()
        .flat_map_iter(|block| {
            let mut local = Vec::new();
            if bloom.block_might_contain(block, needle) {
                let start = block * block_size;
                let end = (start + block_size).min(n);
                for row in start..end {
                    if dataset.matches_substring(row, col, needle) {
                        local.push(row as u32);
                    }
                }
            }
            local.into_iter()
        })
        .collect()
}

fn insert_trigrams(line: &[u8], bits: &mut [u64]) {
    if line.len() < 3 {
        return;
    }
    for window in line.windows(3) {
        let (h1, h2) = trigram_hashes(
            window[0].to_ascii_lowercase(),
            window[1].to_ascii_lowercase(),
            window[2].to_ascii_lowercase(),
        );
        for k in 0..PROBES {
            let idx = (h1.wrapping_add(k.wrapping_mul(h2)) as usize) & BIT_MASK;
            bits[idx / 64] |= 1u64 << (idx % 64);
        }
    }
}

/// Two independent FNV-1a hashes of a trigram, for double hashing.
fn trigram_hashes(a: u8, b: u8, c: u8) -> (u64, u64) {
    const OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;
    let mut h1 = OFFSET;
    for &byte in &[a, b, c] {
        h1 ^= byte as u64;
        h1 = h1.wrapping_mul(PRIME);
    }
    // A second hash with a different mixing order keeps the probes independent.
    let mut h2: u64 = 0x8422_2325_cbf2_9ce4;
    for &byte in &[c, a, b] {
        h2 = h2.wrapping_mul(PRIME);
        h2 ^= byte as u64;
    }
    (h1, h2 | 1) // ensure h2 is odd so probes don't collapse
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::encoding::ascii_icontains;
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
    fn bloom_matches_bruteforce() {
        // Build a dataset with a rare needle in a few known rows.
        let mut data = String::from("n,msg\n");
        for i in 0..20_000u32 {
            let msg = if i % 5000 == 0 {
                "rare_token_xyz"
            } else {
                "ordinary line"
            };
            data.push_str(&format!("{i},{msg}\n"));
        }
        let (ds, _t) = dataset(data.as_bytes());
        let bloom = BlockBloom::build(&ds);

        let needle = b"rare_token_xyz";
        let via_bloom = search_substring(&ds, &bloom, None, needle);

        // Brute-force reference.
        let brute: Vec<u32> = (0..ds.row_count() as u32)
            .filter(|&r| ascii_icontains(ds.line_bytes(r as usize).unwrap(), needle))
            .collect();

        assert_eq!(via_bloom, brute);
        assert_eq!(via_bloom.len(), 4); // rows 0, 5000, 10000, 15000
    }

    #[test]
    fn no_false_negatives_on_common_token() {
        let (ds, _t) = dataset(b"n,msg\n1,alpha\n2,beta\n3,alpha\n");
        let bloom = BlockBloom::build(&ds);
        assert_eq!(search_substring(&ds, &bloom, None, b"alpha"), vec![0, 2]);
        assert_eq!(search_substring(&ds, &bloom, None, b"beta"), vec![1]);
        assert!(search_substring(&ds, &bloom, None, b"missing").is_empty());
    }

    #[test]
    fn scoped_bloom_matches_only_its_column() {
        // "web01" appears in the host column (0) and inside a message (col 1).
        let (ds, _t) = dataset(b"host,msg\nweb01,ok\nweb02,web01 referenced\nweb01,done\n");
        let bloom = BlockBloom::build(&ds);
        // Global: all three lines contain "web01".
        assert_eq!(search_substring(&ds, &bloom, None, b"web01"), vec![0, 1, 2]);
        // Scoped to host (col 0): only rows 0 and 2.
        assert_eq!(search_substring(&ds, &bloom, Some(0), b"web01"), vec![0, 2]);
    }
}
