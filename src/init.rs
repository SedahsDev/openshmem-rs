//! Lifecycle: `shmem_init` / `shmem_finalize`, PE rank & size query.
//!
//! Skeleton — see issue tracker for the bootstrap design (PMIx-backed).

/// A PE's rank in the job, mirroring `shmem_my_pe()`.
pub fn my_pe() -> i32 {
    todo!("issue: PMIx-backed rank discovery")
}

/// Number of PEs, mirroring `shmem_n_pes()`.
pub fn n_pes() -> i32 {
    todo!("issue: PMIx-backed universe size")
}
