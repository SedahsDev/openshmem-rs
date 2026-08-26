#![cfg(feature = "collectives")]

//! DVM integration scaffold for Phase 5. Every execution test remains ignored
//! until the PRRTE/UCC launcher harness is available.

#[test]
#[ignore = "requires DVM + UCC"]
fn prterun_collectives_smoke() {
    openshmem::init::init().expect("PMIx initialization");
    openshmem::coll::barrier().expect("UCC barrier");
    openshmem::init::finalize().expect("PMIx finalization");
}
