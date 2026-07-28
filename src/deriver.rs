//! The per-candidate derivation pipeline.
//!
//! One [`Deriver`] lives per worker thread for the lifetime of the run and owns
//! every buffer it touches, so the inner loop allocates nothing. Derived script
//! hashes are handed to a caller-supplied sink rather than collected, which
//! keeps the lookup out of this module and the benchmark honest.

use secp256k1::{Secp256k1, SignOnly};

use crate::address::{script_hashes, Kind};
use crate::bip32::{Node, Parent, HARDENED};
use crate::bip39::{entropy_to_mnemonic, Pbkdf2Ctx, WordCount};

/// The three derivation schemes, in the order their hashes are produced.
pub const PURPOSES: [(u32, Kind); 3] = [(44, Kind::P2pkh), (49, Kind::P2sh), (84, Kind::P2wpkh)];

/// Where a derived script hash came from, for reconstructing a hit.
#[derive(Copy, Clone, Debug)]
pub struct Origin {
    /// Index into [`PURPOSES`].
    pub purpose: usize,
    /// Address index within the external chain.
    pub index: u32,
}

impl Origin {
    pub fn path(&self) -> String {
        format!("m/{}'/0'/0'/0/{}", PURPOSES[self.purpose].0, self.index)
    }

    pub fn kind(&self) -> Kind {
        PURPOSES[self.purpose].1
    }
}

/// Walks `m/<purpose>'/0'/0'/0` from a master node.
///
/// Costs two `ecmult_gen`: one to parent the normal child at depth 4, one for
/// the external chain itself. The three hardened levels need none.
#[inline]
pub(crate) fn external_chain(
    secp: &Secp256k1<SignOnly>,
    master: &Node,
    purpose: u32,
    buf: &mut [u8; 37],
) -> Option<Parent> {
    let account = master
        .derive_hardened(purpose | HARDENED, buf)?
        .derive_hardened(HARDENED, buf)?
        .derive_hardened(HARDENED, buf)?
        .into_parent(secp);
    Some(account.derive_normal(0, buf)?.into_parent(secp))
}

pub struct Deriver {
    secp: Secp256k1<SignOnly>,
    pbkdf2: Pbkdf2Ctx,
    /// Reused mnemonic text; also the PBKDF2 password.
    mnemonic: String,
    seed: [u8; 64],
    /// BIP-32 child-derivation scratch (0x00||key||idx or pubkey||idx).
    buf: [u8; 37],
}

impl Default for Deriver {
    fn default() -> Self {
        Self::new()
    }
}

impl Deriver {
    pub fn new() -> Self {
        Deriver {
            // Signing-only: we never verify, and the smaller context skips the
            // ecmult table we would never touch.
            secp: Secp256k1::signing_only(),
            pbkdf2: Pbkdf2Ctx::new(),
            mnemonic: String::with_capacity(24 * 9),
            seed: [0u8; 64],
            buf: [0u8; 37],
        }
    }

    pub fn mnemonic(&self) -> &str {
        &self.mnemonic
    }

    pub fn seed(&self) -> &[u8; 64] {
        &self.seed
    }

    /// Turns raw entropy into a mnemonic and its stretched seed.
    ///
    /// This is the PBKDF2 step: 4096 SHA-512 compressions, independent of how
    /// many addresses are derived afterwards.
    #[inline]
    pub fn stretch(&mut self, entropy: &[u8], wc: WordCount) {
        entropy_to_mnemonic(entropy, wc, &mut self.mnemonic);
        let (pb, mn, sd) = (&mut self.pbkdf2, &self.mnemonic, &mut self.seed);
        pb.seed(mn, "", sd);
    }

    /// Derives `n_addr` addresses on each of the three chains from the current
    /// seed, invoking `sink` once per script hash.
    ///
    /// Returns the number of hashes produced. A `None` from the BIP-32 layer
    /// means an invalid child index, which the spec says to skip.
    #[inline]
    pub fn walk<F>(&mut self, n_addr: u32, mut sink: F) -> u32
    where
        F: FnMut(&[u8; 20], Origin),
    {
        let master = match Node::master(&self.seed) {
            Some(m) => m,
            None => return 0,
        };

        let mut produced = 0;
        for (p_idx, (purpose, kind)) in PURPOSES.iter().enumerate() {
            let chain = match external_chain(&self.secp, &master, *purpose, &mut self.buf) {
                Some(c) => c,
                None => continue,
            };

            for i in 0..n_addr {
                let pk = match chain.child_pubkey(&self.secp, i, &mut self.buf) {
                    Some(p) => p,
                    None => continue,
                };
                let hashes = script_hashes(&pk);
                sink(
                    &hashes[*kind as usize],
                    Origin {
                        purpose: p_idx,
                        index: i,
                    },
                );
                produced += 1;
            }
        }
        produced
    }

    /// Re-derives the private key for a specific origin, used only on a hit.
    pub fn private_key_at(&self, origin: Origin) -> Option<secp256k1::SecretKey> {
        let mut buf = [0u8; 37];
        let master = Node::master(&self.seed)?;
        let chain = external_chain(&self.secp, &master, PURPOSES[origin.purpose].0, &mut buf)?;
        chain
            .derive_normal(origin.index, &mut buf)
            .map(|n| *n.private_key())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::address::encode;

    /// The walk must reproduce the published BIP-84 and BIP-44 vectors, which
    /// pins the ordering the sink sees.
    #[test]
    fn walk_matches_known_addresses() {
        let mut d = Deriver::new();
        d.stretch(&[0u8; 16], WordCount::W12);

        let mut seen: Vec<(String, String)> = Vec::new();
        d.walk(2, |h, o| {
            seen.push((o.path(), encode(o.kind(), h)));
        });

        assert_eq!(seen.len(), 6);
        assert_eq!(seen[0].0, "m/44'/0'/0'/0/0");
        assert_eq!(seen[0].1, "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA");
        assert_eq!(seen[4].0, "m/84'/0'/0'/0/0");
        assert_eq!(seen[4].1, "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu");
        assert_eq!(seen[5].1, "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g");
    }

    /// A recovered private key must regenerate the address that matched.
    #[test]
    fn private_key_roundtrip() {
        use secp256k1::{PublicKey, Secp256k1};

        let mut d = Deriver::new();
        d.stretch(&[7u8; 32], WordCount::W24);

        let mut found: Vec<([u8; 20], Origin)> = Vec::new();
        d.walk(2, |h, o| found.push((*h, o)));

        let secp = Secp256k1::signing_only();
        for (hash, origin) in found {
            let sk = d.private_key_at(origin).unwrap();
            let pk = PublicKey::from_secret_key(&secp, &sk).serialize();
            let hs = script_hashes(&pk);
            assert_eq!(hs[origin.kind() as usize], hash, "{}", origin.path());
        }
    }
}
