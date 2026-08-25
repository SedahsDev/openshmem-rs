//! Remote memory access: put/get and atomics.
//!
//! Skeleton — see issue tracker.

/// A typed put, mirroring `shmem_<type>_put`.
pub fn put<T: Copy>(_dst_pe: i32, _src: &[T], _dst_offset: usize) {
    todo!("issue: UCX rma_put via per-PE endpoint")
}

/// A typed get, mirroring `shmem_<type>_get`.
pub fn get<T: Copy>(_src_pe: i32, _src_offset: usize, _len: usize) -> Vec<T> {
    todo!("issue: UCX rma_get via per-PE endpoint")
}
