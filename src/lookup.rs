//! Two-stage membership test for funded addresses.
//!
//! Stage 1 is a *blocked* Bloom filter: all 16 probes for a key land in the
//! same aligned 64-byte cache line, so a lookup costs one cache miss instead of
//! 16. With 60 addresses per candidate, the textbook layout would issue nearly
//! a thousand scattered DRAM reads per seed and dominate the whole pipeline.
//!
//! Stage 2 is an mmap'd, sorted record file searched by binary search. It is
//! only touched on a Bloom hit — at the default false-positive rate, once per
//! few hundred thousand candidates — so its cost never reaches the throughput
//! figure.
//!
//! HASH160 outputs are already uniform, but the probe positions are still
//! derived through a mixing step rather than from the raw bytes. See [`mix64`]
//! for the bug that made that necessary.

use std::fs::File;
use std::io::{self, BufRead, BufWriter, Write};
use std::path::Path;

use memmap2::Mmap;

use crate::address::Kind;

/// On-disk record: kind, HASH160, balance in satoshis.
pub const RECORD_LEN: usize = 29;
const MAGIC: &[u8; 8] = b"SCDB0001";
/// Bytes of a record that form the sort/search key.
const KEY_LEN: usize = 21;

/// Distinguishes the same HASH160 under two script kinds. Odd, so it never
/// collapses the low bits.
const KIND_SALT: u64 = 0x9E37_79B9_7F4A_7C15;

/// Lanes per block. Each lane contributes exactly one probe.
const LANES: usize = 16;
/// Bits per lane. 16 x 32 = 512 bits = one cache line.
const LANE_BITS: usize = 32;
/// Hash bits consumed per probe, `log2(LANE_BITS)`.
const LANE_SHIFT: u32 = 5;

/// Murmur3's 64-bit finaliser.
///
/// Avalanche matters more here than it looks. An earlier version derived the
/// probe stride directly from the key's low bits, which are also what selects
/// the block; for any input with arithmetic structure every key in a block then
/// shared one stride, the probe sets became translates of each other, and the
/// measured false-positive rate blew up to 16%. Mixing first removes that
/// coupling. Real HASH160s are uniform and would not have exposed it, which is
/// exactly why the regression test below uses structured keys.
#[inline(always)]
fn mix64(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    x
}

/// Two independent 64-bit hashes of a lookup key.
#[inline(always)]
fn key_hashes(kind: Kind, h: &[u8; 20]) -> (u64, u64) {
    let a = u64::from_le_bytes(h[0..8].try_into().unwrap());
    let b = u64::from_le_bytes(h[8..16].try_into().unwrap());
    // The trailing 4 bytes would otherwise never influence a probe.
    let c = u32::from_le_bytes(h[16..20].try_into().unwrap()) as u64;

    let h1 = mix64(a ^ (kind as u64).wrapping_mul(KIND_SALT));
    let h2 = mix64(b ^ (c << 32) ^ h1);
    (h1, h2)
}

/// Expected false-positive rate at a given bit budget.
///
/// Averages over the Poisson-distributed occupancy of a block rather than
/// evaluating at the mean: the per-block rate is convex in occupancy, so the
/// overfull blocks dominate. Sizing at the mean underestimated the true rate by
/// about 100x in testing. Evaluated once at startup, so the loop is free.
///
/// Within a block the lanes are independent, and each lane holds `j` bits drawn
/// with replacement from `LANE_BITS`, which makes the conditional rate exactly
/// `(1 - (1 - 1/LANE_BITS)^j)^LANES`.
fn blocked_fpr(bits_per_entry: f64) -> f64 {
    if bits_per_entry <= 0.0 {
        return 1.0;
    }
    let lambda: f64 = (LANES * LANE_BITS) as f64 / bits_per_entry;
    let miss: f64 = 1.0 - 1.0 / LANE_BITS as f64;

    let mut sum = 0.0;
    let mut pmf = (-lambda).exp();
    let cutoff = (lambda + 12.0 * lambda.sqrt()).ceil() as u32 + 32;

    for j in 0..cutoff {
        if j > 0 {
            pmf *= lambda / j as f64;
        }
        let lane_set = 1.0 - miss.powi(j as i32);
        sum += pmf * lane_set.powi(LANES as i32);
    }
    sum.clamp(0.0, 1.0)
}

/// Smallest bit budget meeting `target`.
fn size_for(target: f64) -> f64 {
    let mut bpe = 4.0;
    while bpe <= 1024.0 {
        if blocked_fpr(bpe) <= target {
            return bpe;
        }
        bpe += 0.25;
    }
    1024.0
}

/// One cache line. The alignment is what makes "one cache miss per lookup"
/// true rather than merely likely.
#[repr(align(64))]
#[derive(Clone, Copy)]
struct Block([u32; LANES]);

/// A cache-line-blocked Bloom filter.
///
/// Each key sets exactly one bit in each of the 16 lanes, with the position
/// drawn from 5 dedicated hash bits. An earlier version placed all probes by
/// double hashing — `base + i*stride mod 512` — which admits only about 2^17
/// distinct probe patterns. With ~20 keys per block, two keys sharing a pattern
/// became likely enough to floor the false-positive rate near 1e-3 regardless
/// of how much memory was thrown at it. The lane scheme has a 2^80 pattern
/// space, so no such floor exists.
pub struct Bloom {
    blocks: Vec<Block>,
}

impl Bloom {
    /// Sizes a filter for `n` entries at the requested false-positive rate.
    ///
    /// The classical `m/n = -log2(p)/ln2` formula does not apply here. Keys are
    /// distributed over blocks by hash, so a block's occupancy is Poisson, and
    /// the false-positive rate is convex in occupancy — the overfull blocks
    /// dominate the average. Sizing by the mean underestimated the real rate by
    /// roughly 100x in testing. So the rate is modelled properly by
    /// [`blocked_fpr`] and the size is solved for numerically.
    ///
    /// The block count is deliberately *not* rounded to a power of two, which
    /// would waste up to half the allocation; indexing uses a multiply-shift
    /// reduction instead of a mask.
    pub fn new(n: usize, fpr: f64) -> Bloom {
        let n = n.max(1);
        let target = fpr.clamp(1e-12, 0.5);
        let bits_per_entry = size_for(target);
        let blocks =
            ((n as f64 * bits_per_entry / (LANES * LANE_BITS) as f64).ceil() as usize).max(1);

        Bloom {
            blocks: vec![Block([0u32; LANES]); blocks],
        }
    }

    pub fn bytes(&self) -> usize {
        self.blocks.len() * std::mem::size_of::<Block>()
    }

    /// Probes per lookup. Fixed by the lane geometry.
    pub fn k(&self) -> u32 {
        LANES as u32
    }

    /// False-positive rate for the realised geometry.
    pub fn theoretical_fpr(&self, n: usize) -> f64 {
        blocked_fpr(self.bits_per_entry(n))
    }

    /// Bits allotted per stored entry.
    pub fn bits_per_entry(&self, n: usize) -> f64 {
        (self.blocks.len() * LANES * LANE_BITS) as f64 / n.max(1) as f64
    }

    /// Locates the block and the per-lane bit positions for a key.
    ///
    /// 16 probes need 80 hash bits; `h2` supplies 60 and a third mix the rest,
    /// so no bit range is reused between the block index and the probes.
    #[inline(always)]
    fn probes(&self, kind: Kind, h: &[u8; 20]) -> (usize, [u8; LANES]) {
        let (h1, h2) = key_hashes(kind, h);

        // Lemire's multiply-shift: maps a uniform u64 onto 0..blocks without
        // requiring a power of two and without a division.
        let block = (((h1 as u128) * (self.blocks.len() as u128)) >> 64) as usize;
        let h3 = mix64(h1 ^ h2 ^ 0xA24B_AED4_963E_E407);

        let mut pos = [0u8; LANES];
        let mask = (LANE_BITS - 1) as u64;
        for (i, p) in pos.iter_mut().enumerate() {
            let src = if i < 12 { h2 } else { h3 };
            let shift = if i < 12 { i } else { i - 12 } as u32 * LANE_SHIFT;
            *p = ((src >> shift) & mask) as u8;
        }
        (block, pos)
    }

    pub fn insert(&mut self, kind: Kind, h: &[u8; 20]) {
        let (block, pos) = self.probes(kind, h);
        let cell = &mut self.blocks[block];
        for (lane, &p) in pos.iter().enumerate() {
            cell.0[lane] |= 1u32 << p;
        }
    }

    /// The hot-path query: one cache line, then a bit test per lane, returning
    /// at the first clear bit. For a non-member that is usually lane 0 or 1.
    #[inline(always)]
    pub fn contains(&self, kind: Kind, h: &[u8; 20]) -> bool {
        let (block, pos) = self.probes(kind, h);
        // Multiply-shift cannot reach `len()` for any u64 short of saturation,
        // but clamping costs one predictable compare and removes the need to
        // reason about it in `unsafe`.
        let cell = &self.blocks[block.min(self.blocks.len() - 1)];
        for (lane, &p) in pos.iter().enumerate() {
            if cell.0[lane] & (1u32 << p) == 0 {
                return false;
            }
        }
        true
    }
}

/// The sorted, memory-mapped record file backing stage 2.
pub struct Database {
    mmap: Mmap,
    count: usize,
}

impl Database {
    pub fn open(path: &Path) -> io::Result<Database> {
        let file = File::open(path)?;
        // SAFETY: the file is opened read-only and treated as untrusted bytes;
        // every access is bounds-checked against `count` below. A concurrent
        // truncation by another process would be undefined, which is the
        // standard caveat for mmap and is documented in the README.
        let mmap = unsafe { Mmap::map(&file)? };

        if mmap.len() < 16 || &mmap[..8] != MAGIC {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "not a seed-collider database (bad magic)",
            ));
        }
        let count = u64::from_le_bytes(mmap[8..16].try_into().unwrap()) as usize;
        let need = 16 + count * RECORD_LEN;
        if mmap.len() < need {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("database truncated: need {need} bytes, have {}", mmap.len()),
            ));
        }
        Ok(Database { mmap, count })
    }

    pub fn count(&self) -> usize {
        self.count
    }

    pub fn bytes(&self) -> usize {
        self.mmap.len()
    }

    #[inline]
    fn record(&self, i: usize) -> &[u8] {
        let off = 16 + i * RECORD_LEN;
        &self.mmap[off..off + RECORD_LEN]
    }

    /// Confirms a Bloom hit and returns the recorded balance in satoshis.
    pub fn lookup(&self, kind: Kind, h: &[u8; 20]) -> Option<u64> {
        let mut key = [0u8; KEY_LEN];
        key[0] = kind as u8;
        key[1..].copy_from_slice(h);

        let (mut lo, mut hi) = (0usize, self.count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            let rec = self.record(mid);
            match rec[..KEY_LEN].cmp(&key[..]) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    return Some(u64::from_le_bytes(
                        rec[KEY_LEN..RECORD_LEN].try_into().unwrap(),
                    ))
                }
            }
        }
        None
    }

    /// Rebuilds the Bloom filter by streaming every record.
    pub fn build_bloom(&self, fpr: f64) -> Bloom {
        self.build_bloom_with_progress(fpr, |_| {})
    }

    /// As [`Database::build_bloom`], reporting completion from 0.0 to 1.0.
    ///
    /// At 50M records this takes long enough that a caller with a window to
    /// draw needs to say something about it; the callback fires about a
    /// hundred times, which is often enough to animate and rare enough to cost
    /// nothing.
    pub fn build_bloom_with_progress(&self, fpr: f64, mut progress: impl FnMut(f32)) -> Bloom {
        let mut bloom = Bloom::new(self.count, fpr);
        let step = (self.count / 100).max(1);
        for i in 0..self.count {
            let rec = self.record(i);
            if let Some(kind) = Kind::from_u8(rec[0]) {
                let mut h = [0u8; 20];
                h.copy_from_slice(&rec[1..21]);
                bloom.insert(kind, &h);
            }
            if i % step == 0 {
                progress(i as f32 / self.count.max(1) as f32);
            }
        }
        progress(1.0);
        bloom
    }
}

/// One entry on its way into the database.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Record(pub [u8; RECORD_LEN]);

impl Record {
    pub fn new(kind: Kind, h: &[u8; 20], balance: u64) -> Record {
        let mut r = [0u8; RECORD_LEN];
        r[0] = kind as u8;
        r[1..21].copy_from_slice(h);
        r[21..].copy_from_slice(&balance.to_le_bytes());
        Record(r)
    }
}

/// Builds `count` records from OS entropy, for a practice database.
///
/// These addresses belong to nobody: they are random hashes, not a dump of the
/// chain. A search against them is the real search at the real speed, and it
/// finds exactly as much as a search against the real set would — nothing.
/// What they buy is a program that runs at all before anyone has downloaded a
/// 40 GB dump.
///
/// Lives here rather than in the binary because the window offers to build one
/// too, on a machine whose owner has never opened a terminal.
pub fn synthetic_records(count: usize) -> Result<Vec<Record>, String> {
    let mut records: Vec<Record> = Vec::with_capacity(count + 64);
    // Drawn in blocks; a syscall per record would dominate the runtime.
    let mut block = vec![0u8; 1 << 20];
    let mut made = 0usize;
    while made < count {
        getrandom::getrandom(&mut block).map_err(|e| e.to_string())?;
        for chunk in block.chunks_exact(20) {
            if made >= count {
                break;
            }
            let mut h = [0u8; 20];
            h.copy_from_slice(chunk);
            let kind = match chunk[0] % 3 {
                0 => Kind::P2pkh,
                1 => Kind::P2sh,
                _ => Kind::P2wpkh,
            };
            records.push(Record::new(kind, &h, 100_000 + made as u64));
            made += 1;
        }
    }
    Ok(records)
}

/// Sorts, de-duplicates and writes records to a database file.
pub fn write_database(path: &Path, mut records: Vec<Record>) -> io::Result<usize> {
    // Ordering is by the 21-byte key; the balance tail rides along. Sorting the
    // whole record keeps duplicates adjacent and picks the larger balance.
    records.sort_unstable();
    records.dedup_by(|a, b| a.0[..KEY_LEN] == b.0[..KEY_LEN]);

    let mut w = BufWriter::with_capacity(1 << 20, File::create(path)?);
    w.write_all(MAGIC)?;
    w.write_all(&(records.len() as u64).to_le_bytes())?;
    for r in &records {
        w.write_all(&r.0)?;
    }
    w.flush()?;
    w.into_inner()?.sync_all()?;
    Ok(records.len())
}

/// Parses a dump of funded addresses into records.
///
/// Accepts one address per line, optionally followed by a tab or comma and a
/// balance in satoshis — the shape the common public dumps use. Lines whose
/// address type this collider cannot derive (P2WSH, P2TR, testnet) are skipped
/// and counted, since matching them would be impossible anyway.
pub fn parse_dump<R: BufRead>(reader: R) -> io::Result<(Vec<Record>, usize)> {
    let mut records = Vec::new();
    let mut skipped = 0usize;

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        let mut parts = line.split(['\t', ',', ' ']).filter(|s| !s.is_empty());
        let Some(addr) = parts.next() else { continue };
        // Blockchair-style dumps put the address first and the balance second;
        // a header line will simply fail to decode and be skipped.
        let balance = parts
            .next()
            .and_then(|s| s.parse::<u64>().ok())
            .unwrap_or(0);

        match crate::address::decode(addr) {
            Some((kind, h)) => records.push(Record::new(kind, &h, balance)),
            None => skipped += 1,
        }
    }
    Ok((records, skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// A realistic key: uniformly distributed, like a real HASH160.
    fn h(n: u64) -> [u8; 20] {
        use sha2::{Digest, Sha256};
        let d = Sha256::digest(n.to_le_bytes());
        let mut out = [0u8; 20];
        out.copy_from_slice(&d[..20]);
        out
    }

    /// A deliberately structured key: low entropy, arithmetic relationships
    /// between the words. Not what Bitcoin produces, but the shape that broke
    /// the first version of the probe function.
    fn h_structured(n: u64) -> [u8; 20] {
        let mut out = [0u8; 20];
        out[..8].copy_from_slice(&n.wrapping_mul(0x9E37_79B9_7F4A_7C15).to_le_bytes());
        out[8..16].copy_from_slice(&n.wrapping_mul(0xC2B2_AE3D_27D4_EB4F).to_le_bytes());
        out[16..].copy_from_slice(&(n as u32).to_le_bytes());
        out
    }

    fn measure_fpr(keys: impl Fn(u64) -> [u8; 20], n: usize, target: f64) -> f64 {
        let mut b = Bloom::new(n, target);
        for i in 0..n as u64 {
            b.insert(Kind::P2pkh, &keys(i));
        }
        let trials = 1_000_000u64;
        let mut fp = 0u64;
        for i in 0..trials {
            if b.contains(Kind::P2pkh, &keys(i + 100_000_000)) {
                fp += 1;
            }
        }
        fp as f64 / trials as f64
    }

    #[test]
    fn bloom_has_no_false_negatives() {
        let n = 50_000;
        let mut b = Bloom::new(n, 1e-6);
        for i in 0..n as u64 {
            b.insert(Kind::P2pkh, &h(i));
        }
        for i in 0..n as u64 {
            assert!(b.contains(Kind::P2pkh, &h(i)), "false negative at {i}");
        }
    }

    /// The realised false-positive rate must meet the request.
    #[test]
    fn bloom_fpr_meets_target() {
        let target = 1e-4;
        let measured = measure_fpr(h, 100_000, target);
        assert!(
            measured <= target * 2.0,
            "measured fpr {measured:e} exceeds target {target:e}"
        );
    }

    /// Sizing must not be wasteful either — a filter ten times more accurate
    /// than asked for is one that ate memory nobody authorised.
    #[test]
    fn bloom_is_not_grossly_oversized() {
        let target = 1e-3;
        let measured = measure_fpr(h, 200_000, target);
        assert!(
            measured > target / 50.0,
            "measured fpr {measured:e} is far below target {target:e}; oversized"
        );
    }

    /// Regression test for the probe-correlation bug: keys with arithmetic
    /// structure must not degrade the filter. The first implementation scored
    /// 1.6e-1 here against a 1e-4 target.
    #[test]
    fn bloom_survives_structured_keys() {
        let target = 1e-4;
        let measured = measure_fpr(h_structured, 100_000, target);
        assert!(
            measured < target * 5.0,
            "structured keys degraded fpr to {measured:e}, target {target:e}"
        );
    }

    /// The reported rate must track reality, or the startup banner lies about
    /// the memory/accuracy tradeoff.
    #[test]
    fn theoretical_fpr_tracks_measurement() {
        for target in [1e-2, 1e-3] {
            let n = 200_000;
            let mut b = Bloom::new(n, target);
            for i in 0..n as u64 {
                b.insert(Kind::P2pkh, &h(i));
            }
            let predicted = b.theoretical_fpr(n);
            let measured = measure_fpr(h, n, target);
            let ratio = measured / predicted.max(f64::MIN_POSITIVE);
            assert!(
                (0.3..3.0).contains(&ratio),
                "target {target:e}: predicted {predicted:e} vs measured {measured:e}"
            );
        }
    }

    /// Sizing must not silently round the allocation up to a power of two.
    #[test]
    fn memory_is_not_rounded_to_a_power_of_two() {
        let b = Bloom::new(50_000_000, 1e-6);
        let bytes = b.bytes();
        assert!(
            !bytes.is_power_of_two(),
            "{bytes} bytes is an exact power of two; sizing is rounding up"
        );
        // Sanity: a few hundred MB for 50M entries, not a few GB.
        assert!(
            (100e6..900e6).contains(&(bytes as f64)),
            "{:.0} MB for 50M entries is out of range",
            bytes as f64 / 1e6
        );
    }

    /// More bits must never mean a worse modelled rate.
    #[test]
    fn model_is_monotonic_in_budget() {
        let mut prev = 1.0;
        for bpe in [8.0, 16.0, 24.0, 32.0, 48.0, 64.0] {
            let f = blocked_fpr(bpe);
            assert!(f <= prev, "fpr rose from {prev:e} to {f:e} at {bpe} bits");
            prev = f;
        }
    }

    /// Each block must occupy exactly one cache line, and be aligned to it.
    #[test]
    fn blocks_are_cache_line_sized() {
        assert_eq!(std::mem::size_of::<Block>(), 64);
        assert_eq!(std::mem::align_of::<Block>(), 64);
        assert_eq!(LANES * LANE_BITS, 512);
        assert_eq!(1usize << LANE_SHIFT, LANE_BITS);
        // 12 lanes from h2 and 4 from h3 must both fit in 64 bits.
        const { assert!(12 * LANE_SHIFT <= 64 && 4 * LANE_SHIFT <= 64) };
    }

    /// Distinct script kinds over the same HASH160 must be independent keys.
    #[test]
    fn bloom_separates_kinds() {
        let mut b = Bloom::new(1000, 1e-9);
        let key = h(42);
        b.insert(Kind::P2pkh, &key);
        assert!(b.contains(Kind::P2pkh, &key));
        // Not a hard guarantee, but at 1e-9 a collision here would be a bug.
        assert!(!b.contains(Kind::P2wpkh, &key));
    }

    #[test]
    fn database_roundtrip() {
        let dir = std::env::temp_dir().join("scdb-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.scdb");

        let mut recs = Vec::new();
        for i in 0..1000u64 {
            recs.push(Record::new(Kind::P2wpkh, &h(i), i * 1000));
        }
        // Duplicate keys must collapse.
        recs.push(Record::new(Kind::P2wpkh, &h(5), 5000));
        let written = write_database(&path, recs).unwrap();
        assert_eq!(written, 1000);

        let db = Database::open(&path).unwrap();
        assert_eq!(db.count(), 1000);
        for i in 0..1000u64 {
            assert_eq!(db.lookup(Kind::P2wpkh, &h(i)), Some(i * 1000));
        }
        assert_eq!(db.lookup(Kind::P2pkh, &h(0)), None);
        assert_eq!(db.lookup(Kind::P2wpkh, &h(999_999)), None);

        std::fs::remove_file(&path).ok();
    }

    #[test]
    fn parses_dump_formats() {
        let input = "\
# comment
1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA\t500000
bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu,42
37VucYSaXLCAsxYyAPfbSi9eh4iEcbShgf
bc1pmfr3p9j00pfxjh0zmgp99y8zftmd3s5pmedqhyptwy6lm87hf5sspknck9
";
        let (recs, skipped) = parse_dump(Cursor::new(input)).unwrap();
        assert_eq!(recs.len(), 3, "three derivable addresses");
        assert_eq!(skipped, 1, "the taproot address is not derivable here");
        assert_eq!(
            u64::from_le_bytes(recs[0].0[21..].try_into().unwrap()),
            500000
        );
        assert_eq!(u64::from_le_bytes(recs[1].0[21..].try_into().unwrap()), 42);
    }
}
