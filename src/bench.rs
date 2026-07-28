//! Stage-by-stage timing, so "PBKDF2 is the bottleneck" is a measurement
//! rather than an assumption.
//!
//! Everything runs single-threaded on purpose: per-stage cost per core is what
//! decides where optimisation effort belongs. Scaling to all cores is measured
//! separately by the end-to-end figure.

use std::hint::black_box;
use std::time::Instant;

use secp256k1::{PublicKey, Secp256k1};
use sha2::{Digest, Sha512};

use crate::address::script_hashes;
use crate::bip32::Node;
use crate::bip39::{entropy_to_mnemonic, Pbkdf2Ctx, WordCount};
use crate::deriver::{external_chain, Deriver};

fn timed<F: FnMut()>(iters: u32, mut f: F) -> f64 {
    // Warm the caches and let the CPU settle on a clock before measuring.
    for _ in 0..(iters / 10).max(1) {
        f();
    }
    let t = Instant::now();
    for _ in 0..iters {
        f();
    }
    t.elapsed().as_secs_f64() / iters as f64
}

fn fmt_time(s: f64) -> String {
    if s >= 1e-3 {
        format!("{:>9.3} ms", s * 1e3)
    } else if s >= 1e-6 {
        format!("{:>9.3} us", s * 1e6)
    } else {
        format!("{:>9.1} ns", s * 1e9)
    }
}

pub fn run(n_addr: u32, word_count: WordCount) {
    println!("Schatzsuche benchmark");
    println!("  mnemonic       : {} words", word_count.words());
    println!(
        "  addresses/path : {n_addr} (x3 paths = {} per seed)",
        n_addr * 3
    );
    println!();

    let entropy = [0x5au8; 32];
    let entropy = &entropy[..word_count.entropy_bytes()];

    // --- Stage 1: raw SHA-512, to establish whether the M1's hardware
    // SHA-512 instructions are actually being used by the sha2 backend.
    // Streamed over 1 MiB so per-call setup is amortised away and what remains
    // is the compression function itself.
    const STREAM: usize = 1 << 20;
    let big = vec![0u8; STREAM];
    let t_stream = timed(200, || {
        let mut h = Sha512::new();
        h.update(black_box(&big));
        black_box(h.finalize());
    });
    let per_compress = t_stream / (STREAM / 128) as f64;
    println!(
        "  {:<26} {}  ({:.2} GiB/s)",
        "sha512 compression",
        fmt_time(per_compress),
        STREAM as f64 / t_stream / (1024.0 * 1024.0 * 1024.0)
    );

    // --- Stage 2: mnemonic assembly (entropy -> words).
    let mut mnemonic = String::with_capacity(256);
    let t_mnemonic = timed(200_000, || {
        entropy_to_mnemonic(black_box(entropy), word_count, &mut mnemonic);
        black_box(&mnemonic);
    });
    println!("  {:<26} {}", "entropy -> mnemonic", fmt_time(t_mnemonic));

    // --- Stage 3: PBKDF2. The claimed bottleneck.
    let mut pb = Pbkdf2Ctx::new();
    let mut seed = [0u8; 64];
    let t_pbkdf2 = timed(2_000, || {
        pb.seed(black_box(&mnemonic), "", &mut seed);
        black_box(&seed);
    });
    println!(
        "  {:<26} {}  ({:.0} compressions, {:.0}% of theoretical)",
        "pbkdf2 (2048 rounds)",
        fmt_time(t_pbkdf2),
        4096.0,
        (per_compress * 4096.0 / t_pbkdf2) * 100.0
    );

    // --- Stage 4: BIP-32 master node. Pure HMAC, no curve work.
    let secp = Secp256k1::signing_only();
    let t_master = timed(50_000, || {
        black_box(Node::master(black_box(&seed)));
    });
    println!("  {:<26} {}", "bip32 master", fmt_time(t_master));

    // --- Stage 5: one external chain, m/x'/0'/0'/0. Two ecmult_gen.
    let master = Node::master(&seed).unwrap();
    let mut buf = [0u8; 37];
    let t_account = timed(5_000, || {
        black_box(external_chain(&secp, &master, black_box(44), &mut buf));
    });
    println!(
        "  {:<26} {}  (x3 paths)",
        "bip32 external chain",
        fmt_time(t_account)
    );

    // --- Stage 6: one address child = HMAC-SHA512 + ecmult_gen.
    let account = external_chain(&secp, &master, 84, &mut buf).unwrap();
    let t_child = timed(20_000, || {
        black_box(account.child_pubkey(&secp, black_box(0), &mut buf));
    });
    println!("  {:<26} {}", "bip32 child pubkey", fmt_time(t_child));

    // --- Stage 6b: isolate the elliptic-curve half of that.
    let sk = *account.node().private_key();
    let t_ecmult = timed(20_000, || {
        black_box(PublicKey::from_secret_key(&secp, black_box(&sk)));
    });
    println!(
        "  {:<26} {}  (inside child pubkey)",
        "  of which ecmult_gen",
        fmt_time(t_ecmult)
    );

    // --- Stage 7: hashing a pubkey into all three script hashes.
    let pk = account.child_pubkey(&secp, 0, &mut buf).unwrap();
    let t_hash = timed(200_000, || {
        black_box(script_hashes(black_box(&pk)));
    });
    println!("  {:<26} {}", "script_hashes (3x)", fmt_time(t_hash));

    // --- Stage 8: end to end, one candidate.
    let mut d = Deriver::new();
    let t_total = timed(500, || {
        d.stretch(black_box(entropy), word_count);
        d.walk(n_addr, |h, o| {
            black_box((h, o));
        });
    });

    // Modelled split, for attributing the end-to-end number.
    let derive_model = t_master + 3.0 * t_account + 3.0 * n_addr as f64 * (t_child + t_hash);
    let total_model = t_pbkdf2 + t_mnemonic + derive_model;

    println!();
    println!("  {:<26} {}", "END TO END (1 seed)", fmt_time(t_total));
    println!("  {:<26} {:>12.0}", "keys/sec, 1 core", 1.0 / t_total);
    println!();
    println!("  time attribution (modelled from stages above):");
    println!(
        "    pbkdf2      {:>6.1}%   {}",
        t_pbkdf2 / total_model * 100.0,
        fmt_time(t_pbkdf2)
    );
    println!(
        "    ec + bip32  {:>6.1}%   {}",
        derive_model / total_model * 100.0,
        fmt_time(derive_model)
    );
    println!(
        "    mnemonic    {:>6.1}%   {}",
        t_mnemonic / total_model * 100.0,
        fmt_time(t_mnemonic)
    );
    println!(
        "    unmodelled  {:>6.1}%   {}",
        (t_total - total_model) / t_total * 100.0,
        fmt_time(t_total - total_model)
    );

    // The break-even point is the number that decides where to optimise.
    let per_addr = t_child + t_hash;
    let breakeven = t_pbkdf2 / (3.0 * per_addr);
    println!();
    println!(
        "  break-even: PBKDF2 dominates below {:.1} addresses/path;",
        breakeven
    );
    println!("  above that, elliptic-curve derivation is the bottleneck.");
}
