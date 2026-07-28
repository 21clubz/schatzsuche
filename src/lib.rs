//! Schatzsuche — a BIP-39 keyspace search.
//!
//! Generates mnemonics from OS entropy, derives BIP-44/49/84 addresses, and
//! tests them against a local set of funded addresses. Throughput is the only
//! performance goal; `bench` reports where the time actually goes.
//!
//! Two invariants hold across the whole crate:
//!
//! * A hit is durably on disk before any notification is attempted.
//! * A mnemonic never reaches a notification channel. See [`alert`].

pub mod address;
pub mod alert;
pub mod bench;
pub mod bip32;
pub mod bip39;
pub mod config;
pub mod deriver;
pub mod engine;
pub mod gui;
pub mod hits;
pub mod icon_data;
pub mod lookup;
pub mod machine;
pub mod recover;
pub mod recover_ui;
pub mod startup;
pub mod stats;
pub mod tui;
pub mod util;
