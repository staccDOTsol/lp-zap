//! lp-zap: a venue-agnostic, stateless, PDA-less transaction composer.
//!
//! One instruction (`compose`, tag 0) CPIs a client-supplied sequence of
//! opaque instructions. Before every step and after the last one it snapshots
//! the balances of designated ("watched") accounts and patches `u64` fields of
//! later steps from the real on-chain deltas. The program holds no funds,
//! derives no authority and never signs: every CPI runs with exactly the
//! writable/signer flags the outer transaction granted.
//!
//! Byte layout, error codes and the security model are documented in README.md.
#![no_std]
#![no_main]
#![allow(unexpected_cfgs)]

pub mod error;
pub mod fee;
pub mod layout;
pub mod processor;

// Program id. Replace the literal with the deploy keypair's pubkey before the
// final `cargo build-sbf` (see README "Program id").
pinocchio::address::declare_id!("BHYw1FAWPriW9Gh7BG49X4UVe96CDjaxFrFFUtGSQmRx");

#[cfg(not(feature = "no-entrypoint"))]
pinocchio::program_entrypoint!(processor::process_instruction);
#[cfg(not(feature = "no-entrypoint"))]
pinocchio::no_allocator!();
#[cfg(not(feature = "no-entrypoint"))]
pinocchio::nostd_panic_handler!();
