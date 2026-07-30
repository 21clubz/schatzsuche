//! BIP-39 mnemonic generation and seed stretching.
//!
//! This module owns the acknowledged bottleneck: PBKDF2-HMAC-SHA512 with 2048
//! iterations. One seed costs 4096 SHA-512 compressions and nothing else here
//! comes close, so the implementation avoids the generic `pbkdf2` crate and
//! keeps the HMAC key schedule precomputed across iterations.
//!
//! Only the English wordlist is supported. Every English BIP-39 word is ASCII,
//! so the NFKD normalisation the spec mandates is the identity function on the
//! mnemonic. It is *not* the identity on an arbitrary passphrase; see
//! [`Pbkdf2Ctx::seed`] for how that case is handled.

use sha2::{Digest, Sha256, Sha512};

/// Number of PBKDF2 iterations fixed by BIP-39.
pub const PBKDF2_ROUNDS: u32 = 2048;

/// SHA-512 block size in bytes; also the HMAC pad width.
const BLOCK: usize = 128;

/// Supported mnemonic lengths — the five BIP-39 defines.
///
/// Every one of them carries a checksum of at most eight bits, which is why a
/// single SHA-256 byte covers the whole family in [`entropy_to_mnemonic`].
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum WordCount {
    W12,
    W15,
    W18,
    W21,
    W24,
}

/// The lengths, shortest first. Used by the interfaces to offer the choice
/// without hard-coding a list that could drift from the enum.
pub const ALL_WORD_COUNTS: [WordCount; 5] = [
    WordCount::W12,
    WordCount::W15,
    WordCount::W18,
    WordCount::W21,
    WordCount::W24,
];

impl WordCount {
    /// Entropy bytes backing this mnemonic length.
    pub const fn entropy_bytes(self) -> usize {
        match self {
            WordCount::W12 => 16,
            WordCount::W15 => 20,
            WordCount::W18 => 24,
            WordCount::W21 => 28,
            WordCount::W24 => 32,
        }
    }

    pub const fn words(self) -> usize {
        match self {
            WordCount::W12 => 12,
            WordCount::W15 => 15,
            WordCount::W18 => 18,
            WordCount::W21 => 21,
            WordCount::W24 => 24,
        }
    }

    /// Entropy bits — the exponent of the keyspace this length searches.
    pub const fn entropy_bits(self) -> u32 {
        (self.entropy_bytes() * 8) as u32
    }

    /// Parses a word count written by a human, in `config.toml` or on the
    /// command line. `None` for anything BIP-39 does not define.
    pub const fn from_words(n: u8) -> Option<WordCount> {
        match n {
            12 => Some(WordCount::W12),
            15 => Some(WordCount::W15),
            18 => Some(WordCount::W18),
            21 => Some(WordCount::W21),
            24 => Some(WordCount::W24),
            _ => None,
        }
    }

    /// Checksum bits appended to the entropy before splitting into 11-bit groups.
    pub const fn checksum_bits(self) -> u32 {
        (self.entropy_bytes() as u32 * 8) / 32
    }
}

/// Writes the BIP-39 mnemonic for `entropy` into `out`.
///
/// `out` is cleared and reused, so a worker can keep one `String` alive for its
/// whole life and never allocate in the hot loop.
pub fn entropy_to_mnemonic(entropy: &[u8], wc: WordCount, out: &mut String) {
    debug_assert_eq!(entropy.len(), wc.entropy_bytes());
    out.clear();

    let wordlist = bip39::Language::English.word_list();
    let checksum = Sha256::digest(entropy)[0];
    let cs_bits = wc.checksum_bits();

    // Treat entropy||checksum as a bit string and peel off 11 bits at a time.
    // The entropy is at most 32 bytes, so a 256-bit rolling window would need
    // big-int work; instead index bits directly, which is branch-light and
    // stays well inside L1.
    let total_bits = entropy.len() * 8 + cs_bits as usize;
    debug_assert_eq!(total_bits, wc.words() * 11);

    for w in 0..wc.words() {
        let mut idx: usize = 0;
        for b in 0..11 {
            let bit_pos = w * 11 + b;
            let bit = if bit_pos < entropy.len() * 8 {
                (entropy[bit_pos / 8] >> (7 - (bit_pos % 8))) & 1
            } else {
                let cs_pos = bit_pos - entropy.len() * 8;
                (checksum >> (7 - cs_pos)) & 1
            };
            idx = (idx << 1) | bit as usize;
        }
        if w > 0 {
            out.push(' ');
        }
        out.push_str(wordlist[idx]);
    }
}

/// The English BIP-39 word list, 2048 entries indexed 0..2047.
pub fn wordlist() -> &'static [&'static str] {
    bip39::Language::English.word_list()
}

/// The list index of a word, or `None` if it is not on the list.
///
/// BIP-39 words are unique in their first four letters, so a four-letter
/// prefix is accepted too: someone recovering a seed from handwriting often
/// has the stub of a word rather than the whole of it.
pub fn word_index(word: &str) -> Option<u16> {
    let word = word.trim().to_ascii_lowercase();
    let list = wordlist();
    if let Some(i) = list.iter().position(|w| *w == word) {
        return Some(i as u16);
    }
    if word.len() >= 4 {
        let stub = &word[..4];
        if let Some(i) = list.iter().position(|w| w.starts_with(stub)) {
            return Some(i as u16);
        }
    }
    None
}

/// The word [`word_index`] would actually use for this input.
///
/// The recovery screen shows this back to the reader. The lookup above is
/// deliberately forgiving — it accepts a four-letter stub, and therefore also
/// accepts "abandonn" and silently reads it as "abandon". Forgiving is right;
/// silent is not, when the thing being read is somebody's seed. Handing the
/// resolved word back is what lets the screen say which word it understood.
pub fn resolve_word(input: &str) -> Option<&'static str> {
    word_index(input).map(|i| wordlist()[i as usize])
}

/// Words beginning with `prefix`, at most `max` of them, for the suggestions
/// the recovery screen offers while a word is being typed.
///
/// An empty prefix returns nothing rather than the first `max` words: a blank
/// field means "I do not know this one", and answering it with "abandon,
/// ability, able" would be noise at best and a nudge at worst.
pub fn words_starting_with(prefix: &str, max: usize) -> Vec<&'static str> {
    let prefix = prefix.trim().to_ascii_lowercase();
    if prefix.is_empty() {
        return Vec::new();
    }
    wordlist()
        .iter()
        .filter(|w| w.starts_with(&prefix))
        .take(max)
        .copied()
        .collect()
}

/// Turns word indices back into entropy, but only if the BIP-39 checksum
/// holds. `None` means this exact sequence of words is not a valid mnemonic.
///
/// This is the filter the recovery search leans on: most candidates fail here,
/// at the cost of one SHA-256, and never reach the expensive derivation.
pub fn indices_to_entropy(indices: &[u16], wc: WordCount) -> Option<Vec<u8>> {
    if indices.len() != wc.words() {
        return None;
    }
    let n_entropy = wc.entropy_bytes();
    let total_bits = wc.words() * 11;
    let mut bits = vec![0u8; total_bits];
    for (w, &idx) in indices.iter().enumerate() {
        if idx >= 2048 {
            return None;
        }
        for b in 0..11 {
            bits[w * 11 + b] = ((idx >> (10 - b)) & 1) as u8;
        }
    }

    let mut entropy = vec![0u8; n_entropy];
    for (i, chunk) in bits[..n_entropy * 8].chunks(8).enumerate() {
        let mut byte = 0u8;
        for &bit in chunk {
            byte = (byte << 1) | bit;
        }
        entropy[i] = byte;
    }

    let checksum = Sha256::digest(&entropy)[0];
    for b in 0..wc.checksum_bits() as usize {
        let expected = (checksum >> (7 - b)) & 1;
        if bits[n_entropy * 8 + b] != expected {
            return None;
        }
    }
    Some(entropy)
}

/// A primed HMAC-SHA512 key schedule plus scratch space for PBKDF2.
///
/// Reused across candidates. Priming costs two SHA-512 compressions per seed,
/// which is 0.05% of the 4096 the stretch itself needs.
pub struct Pbkdf2Ctx {
    salt: Vec<u8>,
}

impl Default for Pbkdf2Ctx {
    fn default() -> Self {
        Self::new()
    }
}

impl Pbkdf2Ctx {
    pub fn new() -> Self {
        Self {
            salt: Vec::with_capacity(64),
        }
    }

    /// Derives the 64-byte BIP-39 seed from `mnemonic` and `passphrase`.
    ///
    /// The passphrase must already be NFKD-normalised by the caller. The
    /// collider always runs with an empty passphrase, for which normalisation
    /// is a no-op, so this costs nothing in the hot loop.
    pub fn seed(&mut self, mnemonic: &str, passphrase: &str, out: &mut [u8; 64]) {
        let (ipad, opad) = hmac_init(mnemonic.as_bytes());

        self.salt.clear();
        self.salt.extend_from_slice(b"mnemonic");
        self.salt.extend_from_slice(passphrase.as_bytes());
        // PBKDF2 block index; dkLen == hLen == 64, so there is exactly one.
        self.salt.extend_from_slice(&1u32.to_be_bytes());

        let mut u = hmac_finish(&ipad, &opad, &self.salt);
        *out = u;

        for _ in 1..PBKDF2_ROUNDS {
            u = hmac_finish(&ipad, &opad, &u);
            for (o, x) in out.iter_mut().zip(u.iter()) {
                *o ^= *x;
            }
        }
    }
}

/// Builds the two primed SHA-512 states for an HMAC key.
#[inline]
fn hmac_init(key: &[u8]) -> (Sha512, Sha512) {
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let d = Sha512::digest(key);
        k[..64].copy_from_slice(&d);
    } else {
        k[..key.len()].copy_from_slice(key);
    }

    let mut ipad = [0u8; BLOCK];
    let mut opad = [0u8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] = k[i] ^ 0x36;
        opad[i] = k[i] ^ 0x5c;
    }

    let mut i_state = Sha512::new();
    i_state.update(ipad);
    let mut o_state = Sha512::new();
    o_state.update(opad);
    (i_state, o_state)
}

/// One HMAC-SHA512 over `msg` using primed pad states. Two compressions.
#[inline]
fn hmac_finish(ipad: &Sha512, opad: &Sha512, msg: &[u8]) -> [u8; 64] {
    let mut inner = ipad.clone();
    inner.update(msg);
    let id = inner.finalize();

    let mut outer = opad.clone();
    outer.update(id);
    outer.finalize().into()
}

/// Standalone HMAC-SHA512, used by BIP-32 where the key changes every call.
#[inline]
pub fn hmac_sha512(key: &[u8], msg: &[u8]) -> [u8; 64] {
    let (i, o) = hmac_init(key);
    hmac_finish(&i, &o, msg)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// What the recovery screen shows back to the reader, including the cases
    /// where the forgiving lookup reads something other than what was typed.
    #[test]
    fn a_typed_word_reports_what_it_was_understood_as() {
        assert_eq!(resolve_word("abandon"), Some("abandon"));
        assert_eq!(resolve_word("  ABANDON  "), Some("abandon"));
        // A four-letter stub, which is the case the lookup exists for.
        assert_eq!(resolve_word("aban"), Some("abandon"));
        // And the case that used to pass silently: a typo one letter too long.
        assert_eq!(
            resolve_word("abandonn"),
            Some("abandon"),
            "still resolved — but now the screen can say so"
        );
        assert_eq!(resolve_word("zzzz"), None);
        assert_eq!(resolve_word(""), None);
        assert_eq!(resolve_word("ab"), None, "too short to be a stub");
    }

    #[test]
    fn suggestions_narrow_as_the_word_is_typed() {
        let a = words_starting_with("aba", 10);
        assert!(a.contains(&"abandon"), "got {a:?}");
        assert!(a.iter().all(|w| w.starts_with("aba")));

        // Narrowing to one.
        assert_eq!(words_starting_with("abando", 10), vec!["abandon"]);

        // Case and spacing are the reader's business, not the list's.
        assert_eq!(words_starting_with("  ABAN ", 10), vec!["abandon"]);

        // A blank field is a question, not a prefix.
        assert!(words_starting_with("", 10).is_empty());
        assert!(words_starting_with("   ", 10).is_empty());

        // Nothing matches nothing.
        assert!(words_starting_with("qqqq", 10).is_empty());

        // The cap holds.
        assert_eq!(words_starting_with("a", 3).len(), 3);

        // Every BIP-39 word can be reached by typing it out, and offers itself
        // first. Not *only* itself: the list contains words that are prefixes
        // of longer ones — "act" of "action", "add" of "address" — so someone
        // who has typed "act" may still be on their way to "action" and must
        // keep seeing it.
        for w in wordlist() {
            let found = words_starting_with(w, 8);
            assert_eq!(
                found.first(),
                Some(w),
                "a whole word must offer itself first: {w} gave {found:?}"
            );
        }
        assert_eq!(
            words_starting_with("act", 8),
            vec!["act", "action", "actor", "actress", "actual"]
        );
    }

    /// Trezor's canonical BIP-39 vector: all-zero entropy, passphrase "TREZOR".
    #[test]
    fn bip39_trezor_vector_12() {
        let entropy = [0u8; 16];
        let mut m = String::new();
        entropy_to_mnemonic(&entropy, WordCount::W12, &mut m);
        assert_eq!(
            m,
            "abandon abandon abandon abandon abandon abandon \
             abandon abandon abandon abandon abandon about"
        );

        let mut seed = [0u8; 64];
        Pbkdf2Ctx::new().seed(&m, "TREZOR", &mut seed);
        assert_eq!(
            hex(&seed),
            "c55257c360c07c72029aebc1b53c05ed0362ada38ead3e3e9efa3708e5349553\
             1f09a6987599d18264c1e1c92f2cf141630c7a3c4ab7c81b2f001698e7463b04"
                .replace(' ', "")
        );
    }

    #[test]
    fn bip39_trezor_vector_24() {
        let entropy = [0xffu8; 32];
        let mut m = String::new();
        entropy_to_mnemonic(&entropy, WordCount::W24, &mut m);
        assert_eq!(
            m,
            "zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo \
             zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo zoo vote"
        );
    }

    /// Cross-check a spread of random entropy against the reference crate.
    /// Guards the bit-slicing and the checksum against off-by-one errors that
    /// a single fixed vector would not catch.
    #[test]
    fn matches_reference_crate() {
        // Every length BIP-39 defines, not just the two ends of the range: the
        // checksum is five bits at fifteen words and seven at twenty-one, and
        // an off-by-one there would produce a mnemonic that looks perfectly
        // plausible and is wrong.
        for n in 0u32..64 {
            for wc in ALL_WORD_COUNTS {
                let mut entropy = vec![0u8; wc.entropy_bytes()];
                for (i, b) in entropy.iter_mut().enumerate() {
                    *b = (n.wrapping_mul(31).wrapping_add(i as u32 * 7)) as u8;
                }

                let mut ours = String::new();
                entropy_to_mnemonic(&entropy, wc, &mut ours);
                assert_eq!(
                    ours.split_whitespace().count(),
                    wc.words(),
                    "wrong word count for {wc:?}"
                );

                let theirs = bip39::Mnemonic::from_entropy(&entropy).unwrap().to_string();
                assert_eq!(ours, theirs, "mnemonic mismatch for {wc:?} {entropy:?}");

                let mut our_seed = [0u8; 64];
                Pbkdf2Ctx::new().seed(&ours, "", &mut our_seed);
                let their_seed = bip39::Mnemonic::parse(&theirs).unwrap().to_seed("");
                assert_eq!(our_seed, their_seed, "seed mismatch for {ours}");
            }
        }
    }

    /// The lengths and their keyspaces have to agree with BIP-39 arithmetic:
    /// eleven bits per word, of which the last few are the checksum.
    #[test]
    fn every_length_adds_up() {
        for wc in ALL_WORD_COUNTS {
            let entropy_bits = wc.entropy_bits() as usize;
            let checksum = entropy_bits / 32;
            assert_eq!(
                entropy_bits + checksum,
                wc.words() * 11,
                "{wc:?} does not divide into 11-bit words"
            );
            assert!(checksum <= 8, "{wc:?} needs more than one checksum byte");
            assert_eq!(WordCount::from_words(wc.words() as u8), Some(wc));
        }
        for bad in [0u8, 11, 13, 16, 20, 23, 25, 255] {
            assert_eq!(WordCount::from_words(bad), None, "{bad} is not BIP-39");
        }
    }
}
