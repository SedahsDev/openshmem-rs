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
//! This module is currently a skeleton. The phased implementation roadmap lives in
//! the repository's issue tracker.
#![forbid(unsafe_code)]

pub mod error;
pub mod init;
pub mod rma;
pub mod symheap;
