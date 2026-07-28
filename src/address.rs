//! Script hashing and address encoding.
//!
//! The hot loop never builds an address *string*. All three supported script
//! types reduce to a 20-byte HASH160 plus a one-byte discriminator, and that
//! pair is what the lookup structures store. Base58 and bech32 encoding exist
//! only for importing a dump and for printing a hit.

use ripemd::Ripemd160;
use sha2::{Digest, Sha256};

/// Which script type a HASH160 belongs to.
///
/// Two different script types can share the same 20 bytes (a P2PKH and a
/// P2WPKH for the same key do), so the discriminator is part of the lookup key.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Kind {
    /// Legacy pay-to-pubkey-hash, addresses starting `1`. BIP-44.
    P2pkh = 0,
    /// Pay-to-script-hash, addresses starting `3`. BIP-49 nests P2WPKH here.
    P2sh = 1,
    /// Native segwit v0 pay-to-witness-pubkey-hash, `bc1q...`. BIP-84.
    P2wpkh = 2,
}

impl Kind {
    pub fn from_u8(v: u8) -> Option<Kind> {
        match v {
            0 => Some(Kind::P2pkh),
            1 => Some(Kind::P2sh),
            2 => Some(Kind::P2wpkh),
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::P2pkh => "p2pkh",
            Kind::P2sh => "p2sh-p2wpkh",
            Kind::P2wpkh => "p2wpkh",
        }
    }
}

/// RIPEMD160(SHA256(data)).
#[inline]
pub fn hash160(data: &[u8]) -> [u8; 20] {
    let sha = Sha256::digest(data);
    Ripemd160::digest(sha).into()
}

/// The three script hashes a single public key produces, in BIP-44/49/84 order.
///
/// P2PKH and P2WPKH share the key hash; only P2SH needs the extra HASH160 over
/// the nested witness program.
#[inline]
pub fn script_hashes(pubkey: &[u8; 33]) -> [[u8; 20]; 3] {
    let kh = hash160(pubkey);

    // BIP-49 redeem script: OP_0 PUSH20 <keyhash>
    let mut redeem = [0u8; 22];
    redeem[0] = 0x00;
    redeem[1] = 0x14;
    redeem[2..].copy_from_slice(&kh);
    let sh = hash160(&redeem);

    [kh, sh, kh]
}

const B58: &[u8; 58] = b"123456789ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnopqrstuvwxyz";

/// Base58Check over an arbitrary payload (the version byte, if any, is part of
/// `payload`).
fn b58check(payload: &[u8]) -> String {
    let mut num = Vec::with_capacity(payload.len() + 4);
    num.extend_from_slice(payload);
    let ck = Sha256::digest(Sha256::digest(payload));
    num.extend_from_slice(&ck[..4]);

    // Leading zero bytes are encoded as '1' and sit outside the bignum.
    let leading = num.iter().take_while(|&&b| b == 0).count();
    let len = num.len();

    let mut out = Vec::with_capacity(len * 2);
    let mut start = leading;
    while start < len {
        let mut rem = 0u32;
        for b in num[start..len].iter_mut() {
            let cur = rem * 256 + *b as u32;
            *b = (cur / 58) as u8;
            rem = cur % 58;
        }
        out.push(B58[rem as usize]);
        while start < len && num[start] == 0 {
            start += 1;
        }
    }

    let mut s = String::with_capacity(leading + out.len());
    for _ in 0..leading {
        s.push('1');
    }
    for c in out.iter().rev() {
        s.push(*c as char);
    }
    s
}

fn base58check(version: u8, payload: &[u8; 20]) -> String {
    let mut data = [0u8; 21];
    data[0] = version;
    data[1..].copy_from_slice(payload);
    b58check(&data)
}

/// Mainnet WIF for a compressed-pubkey private key.
///
/// Written only to the local hit files; it never leaves the machine.
pub fn wif_compressed(key: &[u8; 32]) -> String {
    let mut data = [0u8; 34];
    data[0] = 0x80;
    data[1..33].copy_from_slice(key);
    data[33] = 0x01;
    b58check(&data)
}

fn base58check_decode(s: &str) -> Option<(u8, [u8; 20])> {
    // A 25-byte payload never exceeds 35 base58 digits; reject longer input
    // before doing work.
    if s.len() > 35 || s.is_empty() {
        return None;
    }

    // Leading '1's are digit zero: they add nothing to the numeric value and
    // are re-attached as zero bytes afterwards.
    let leading = s.bytes().take_while(|&b| b == b'1').count();

    let mut num = [0u8; 32];
    for c in s.bytes() {
        let d = B58.iter().position(|&x| x == c)? as u32;
        let mut carry = d;
        for b in num.iter_mut().rev() {
            let cur = *b as u32 * 58 + carry;
            *b = cur as u8;
            carry = cur >> 8;
        }
        if carry != 0 {
            return None;
        }
    }

    let first = num.iter().position(|&b| b != 0).unwrap_or(num.len());
    let sig = &num[first..];
    if leading + sig.len() != 25 {
        return None;
    }
    let mut data = [0u8; 25];
    data[leading..].copy_from_slice(sig);

    let ck = Sha256::digest(Sha256::digest(&data[..21]));
    if ck[..4] != data[21..] {
        return None;
    }
    let mut h = [0u8; 20];
    h.copy_from_slice(&data[1..21]);
    Some((data[0], h))
}

pub fn encode(kind: Kind, h: &[u8; 20]) -> String {
    match kind {
        Kind::P2pkh => base58check(0x00, h),
        Kind::P2sh => base58check(0x05, h),
        Kind::P2wpkh => {
            let hrp = bech32::Hrp::parse("bc").expect("static hrp");
            bech32::segwit::encode_v0(hrp, h).expect("20-byte v0 program")
        }
    }
}

/// Parses a mainnet address into its lookup key.
///
/// Returns `None` for address types this collider cannot derive (P2WSH, P2TR,
/// bare multisig, testnet), which are skipped during import.
pub fn decode(s: &str) -> Option<(Kind, [u8; 20])> {
    if s.starts_with("bc1") || s.starts_with("BC1") {
        let (hrp, version, program) = bech32::segwit::decode(s).ok()?;
        if hrp.as_str() != "bc" || version.to_u8() != 0 || program.len() != 20 {
            return None;
        }
        let mut h = [0u8; 20];
        h.copy_from_slice(&program);
        return Some((Kind::P2wpkh, h));
    }

    let (version, h) = base58check_decode(s)?;
    match version {
        0x00 => Some((Kind::P2pkh, h)),
        0x05 => Some((Kind::P2sh, h)),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bip32::{Node, Parent};
    use crate::bip39::{entropy_to_mnemonic, Pbkdf2Ctx, WordCount};
    use crate::deriver::external_chain;
    use secp256k1::Secp256k1;

    fn abandon_chain(purpose: u32) -> (Secp256k1<secp256k1::SignOnly>, Parent) {
        let secp = Secp256k1::signing_only();
        let mut m = String::new();
        entropy_to_mnemonic(&[0u8; 16], WordCount::W12, &mut m);
        let mut seed = [0u8; 64];
        Pbkdf2Ctx::new().seed(&m, "", &mut seed);

        let mut buf = [0u8; 37];
        let master = Node::master(&seed).unwrap();
        let chain = external_chain(&secp, &master, purpose, &mut buf).unwrap();
        (secp, chain)
    }

    fn addr_at(purpose: u32, kind: Kind, i: u32) -> String {
        let (secp, chain) = abandon_chain(purpose);
        let mut buf = [0u8; 37];
        let pk = chain.child_pubkey(&secp, i, &mut buf).unwrap();
        let hs = script_hashes(&pk);
        encode(kind, &hs[kind as usize])
    }

    /// BIP-84's own vector for the "abandon ... about" mnemonic.
    #[test]
    fn bip84_official_vector() {
        assert_eq!(
            addr_at(84, Kind::P2wpkh, 0),
            "bc1qcr8te4kr609gcawutmrza0j4xv80jy8z306fyu"
        );
        assert_eq!(
            addr_at(84, Kind::P2wpkh, 1),
            "bc1qnjg0jd8228aq7egyzacy8cys3knf9xvrerkf9g"
        );
    }

    /// Widely published first address for m/44'/0'/0'/0/0 of the same mnemonic.
    #[test]
    fn bip44_known_address() {
        assert_eq!(
            addr_at(44, Kind::P2pkh, 0),
            "1LqBGSKuX5yYUonjxT5qGfpUsXKYYWeabA"
        );
    }

    #[test]
    fn base58_roundtrip() {
        for v in [0x00u8, 0x05] {
            for n in 0u8..64 {
                let mut h = [0u8; 20];
                for (i, b) in h.iter_mut().enumerate() {
                    *b = n.wrapping_mul(7).wrapping_add(i as u8);
                }
                let kind = if v == 0 { Kind::P2pkh } else { Kind::P2sh };
                let s = encode(kind, &h);
                assert_eq!(decode(&s), Some((kind, h)), "roundtrip failed for {s}");
            }
        }
    }

    #[test]
    fn bech32_roundtrip() {
        for n in 0u8..64 {
            let mut h = [0u8; 20];
            for (i, b) in h.iter_mut().enumerate() {
                *b = n.wrapping_mul(11).wrapping_add(i as u8);
            }
            let s = encode(Kind::P2wpkh, &h);
            assert_eq!(decode(&s), Some((Kind::P2wpkh, h)));
        }
    }

    /// Leading-zero HASH160 values are the classic base58 edge case.
    #[test]
    fn base58_leading_zeros() {
        let mut h = [0u8; 20];
        h[19] = 1;
        let s = encode(Kind::P2pkh, &h);
        assert!(s.starts_with("11"), "expected leading ones, got {s}");
        assert_eq!(decode(&s), Some((Kind::P2pkh, h)));
    }

    /// Full cross-check of all three script types against the `bitcoin` crate.
    #[test]
    fn matches_reference_crate() {
        use bitcoin::bip32::{DerivationPath, Xpriv};
        use bitcoin::{Address, CompressedPublicKey, Network};
        use std::str::FromStr;

        let secp = bitcoin::secp256k1::Secp256k1::new();
        let our_secp: Secp256k1<secp256k1::SignOnly> = Secp256k1::signing_only();

        for n in 0u32..16 {
            let mut entropy = [0u8; 32];
            for (i, b) in entropy.iter_mut().enumerate() {
                *b = (n.wrapping_mul(37).wrapping_add(i as u32 * 5)) as u8;
            }
            let mut m = String::new();
            entropy_to_mnemonic(&entropy, WordCount::W24, &mut m);
            let mut seed = [0u8; 64];
            Pbkdf2Ctx::new().seed(&m, "", &mut seed);

            let xprv = Xpriv::new_master(Network::Bitcoin, &seed).unwrap();
            let ours_master = Node::master(&seed).unwrap();
            let mut buf = [0u8; 37];

            for (purpose, kind) in [(44u32, Kind::P2pkh), (49, Kind::P2sh), (84, Kind::P2wpkh)] {
                let ours = external_chain(&our_secp, &ours_master, purpose, &mut buf).unwrap();

                for i in 0u32..4 {
                    let path =
                        DerivationPath::from_str(&format!("m/{purpose}'/0'/0'/0/{i}")).unwrap();
                    let child = xprv.derive_priv(&secp, &path).unwrap();
                    let cpk = CompressedPublicKey(child.private_key.public_key(&secp));
                    let theirs = match kind {
                        Kind::P2pkh => Address::p2pkh(cpk, Network::Bitcoin),
                        Kind::P2sh => Address::p2shwpkh(&cpk, Network::Bitcoin),
                        Kind::P2wpkh => Address::p2wpkh(&cpk, Network::Bitcoin),
                    }
                    .to_string();

                    let pk = ours.child_pubkey(&our_secp, i, &mut buf).unwrap();
                    let hs = script_hashes(&pk);
                    let mine = encode(kind, &hs[kind as usize]);

                    assert_eq!(mine, theirs, "m/{purpose}'/0'/0'/0/{i} of {m}");
                }
            }
        }
    }
}
