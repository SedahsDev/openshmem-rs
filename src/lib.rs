//! # openshmem
//!
//! An OpenSHMEM-style **partitioned global address space (PGAS)** library for Rust,
//! modeled on the OSSS-UCX reference implementation and built on three SedahsDev
//! binding crates:
//!
//! - `pmix` — process bootstrap, rank/size discovery, key/value exchange
//! - `ucx-sys` — RMA (`put`/`get`), atomic ops, `fence`/`flush`, symmetric-memory
//!   registration and rkey packing
//! - `ucc` (optional) — collectives (`barrier`, `broadcast`, `collect`, `reduce`)
//!
//! Phase 1 provides a contiguous UCX-registered symmetric heap and address-only
//! allocation tokens; the phased implementation roadmap lives in the issue tracker.
// The crate-level `forbid(unsafe_code)` policy is intentionally relaxed for
// the single narrow `SymHeap` Send implementation required by global state;
// all other library code remains safe Rust.

pub mod bootstrap;
pub mod error;
pub mod init;
pub mod rma;
pub mod symheap;
