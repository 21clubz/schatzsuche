//! BIP-32 derivation, split by what each step actually needs.
//!
//! The type split here is a performance decision, not decoration. Hardened
//! derivation reads the *private* key; only normal derivation needs the parent
//! public key, and computing one costs an `ecmult_gen` — the single most
//! expensive primitive in the pipeline. A [`Node`] therefore carries no public
//! key, and you pay for one exactly when you convert to a [`Parent`].
//!
//! Walking `m/44'/0'/0'/0` this way costs 2 `ecmult_gen` instead of 5.
//!
//! Private derivation is used throughout. Public derivation (`parent_pub +
//! IL*G`) needs the same `ecmult_gen` *plus* a point addition, and would leave
//! us without the private key we need if a candidate ever hits.

use secp256k1::{PublicKey, Scalar, Secp256k1, SecretKey, SignOnly};

use crate::bip39::hmac_sha512;

/// Offset marking a hardened child index.
pub const HARDENED: u32 = 0x8000_0000;

/// A derived node: key material only, no public key computed.
#[derive(Copy, Clone)]
pub struct Node {
    key: SecretKey,
    chain_code: [u8; 32],
}

/// A node whose compressed public key has been computed, so it can derive
/// normal children and address public keys.
#[derive(Clone)]
pub struct Parent {
    node: Node,
    pub_ser: [u8; 33],
}

impl Node {
    /// BIP-32 master node from a BIP-39 seed. No elliptic-curve work.
    ///
    /// Returns `None` if IL is zero or >= n, which BIP-32 says to reject.
    pub fn master(seed: &[u8; 64]) -> Option<Node> {
        Self::from_hmac(&hmac_sha512(b"Bitcoin seed", seed))
    }

    fn from_hmac(i: &[u8; 64]) -> Option<Node> {
        let key = SecretKey::from_slice(&i[..32]).ok()?;
        let mut chain_code = [0u8; 32];
        chain_code.copy_from_slice(&i[32..]);
        Some(Node { key, chain_code })
    }

    pub fn private_key(&self) -> &SecretKey {
        &self.key
    }

    /// Derives a hardened child. One HMAC-SHA512, no curve operations.
    ///
    /// `index` is taken as an absolute child number, so callers pass
    /// `44 | HARDENED`.
    #[inline]
    pub fn derive_hardened(&self, index: u32, buf: &mut [u8; 37]) -> Option<Node> {
        debug_assert!(index >= HARDENED, "index must carry the hardened bit");
        buf[0] = 0;
        buf[1..33].copy_from_slice(&self.key.secret_bytes());
        buf[33..].copy_from_slice(&index.to_be_bytes());
        self.tweak(buf)
    }

    /// Computes this node's public key so it can parent normal children.
    /// Costs one `ecmult_gen`.
    #[inline]
    pub fn into_parent(self, secp: &Secp256k1<SignOnly>) -> Parent {
        let pub_ser = PublicKey::from_secret_key(secp, &self.key).serialize();
        Parent {
            node: self,
            pub_ser,
        }
    }

    /// Applies the BIP-32 tweak `ki = IL + kpar (mod n)` to the HMAC of `buf`.
    #[inline]
    fn tweak(&self, buf: &[u8; 37]) -> Option<Node> {
        let i = hmac_sha512(&self.chain_code, buf);
        let il = Scalar::from_be_bytes(i[..32].try_into().unwrap()).ok()?;
        let key = self.key.add_tweak(&il).ok()?;
        let mut chain_code = [0u8; 32];
        chain_code.copy_from_slice(&i[32..]);
        Some(Node { key, chain_code })
    }
}

impl Parent {
    pub fn node(&self) -> &Node {
        &self.node
    }

    pub fn public_key_bytes(&self) -> &[u8; 33] {
        &self.pub_ser
    }

    /// Derives a normal (non-hardened) child. One HMAC-SHA512, no curve work.
    #[inline]
    pub fn derive_normal(&self, index: u32, buf: &mut [u8; 37]) -> Option<Node> {
        debug_assert!(index < HARDENED, "hardened index on normal derivation");
        buf[..33].copy_from_slice(&self.pub_ser);
        buf[33..].copy_from_slice(&index.to_be_bytes());
        self.node.tweak(buf)
    }

    /// Derives the child public key at `index` — the hot-loop entry point.
    ///
    /// One HMAC-SHA512 plus one `ecmult_gen`.
    #[inline]
    pub fn child_pubkey(
        &self,
        secp: &Secp256k1<SignOnly>,
        index: u32,
        buf: &mut [u8; 37],
    ) -> Option<[u8; 33]> {
        let child = self.derive_normal(index, buf)?;
        Some(PublicKey::from_secret_key(secp, &child.key).serialize())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(b: &[u8]) -> String {
        b.iter().map(|x| format!("{x:02x}")).collect()
    }

    /// BIP-32 test vector 1 (seed 000102...0f), master and m/0'.
    #[test]
    fn bip32_vector_1() {
        let seed16: Vec<u8> = (0u8..16).collect();
        let i = hmac_sha512(b"Bitcoin seed", &seed16);
        let m = Node::from_hmac(&i).unwrap();

        assert_eq!(
            hex(&m.key.secret_bytes()),
            "e8f32e723decf4051aefac8e2c93c9c5b214313817cdb01a1494b917c8436b35"
        );
        assert_eq!(
            hex(&m.chain_code),
            "873dff81c02f525623fd1fe5167eac3a55a049de3d314bb42ee227ffed37d508"
        );

        let mut buf = [0u8; 37];
        let c = m.derive_hardened(HARDENED, &mut buf).unwrap();
        assert_eq!(
            hex(&c.key.secret_bytes()),
            "edb2e14f9ee77d26dd93b4ecede8d16ed408ce149b6cd80b0715a2d911a0afea"
        );
        assert_eq!(
            hex(&c.chain_code),
            "47fdacbd0f1097043b78c63c20c34ef4ed9a111d980047ad16282c7ae6236141"
        );

        // m/0'/1 exercises the normal-derivation path.
        let secp = Secp256k1::signing_only();
        let c1 = c.into_parent(&secp).derive_normal(1, &mut buf).unwrap();
        assert_eq!(
            hex(&c1.key.secret_bytes()),
            "3c6cb8d0f6a264c91ea8b5030fadaa8e538b020f0a387421a12de9319dc93368"
        );
        assert_eq!(
            hex(&c1.chain_code),
            "2a7857631386ba23dacac34180dd1983734e444fdbf774041578e9b6adb37c19"
        );
    }
}
