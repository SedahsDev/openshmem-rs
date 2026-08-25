//! Symmetric heap: allocation/registration and the PE→rkey map.
//!
//! Skeleton — see issue tracker.

/// A symmetric-memory allocation backed by a UCX-registered buffer.
pub struct SymAlloc;

impl SymAlloc {
    /// Allocate `size` bytes in the symmetric heap (`shmem_malloc`).
    pub fn malloc(_size: usize) -> Option<*mut u8> {
        todo!("issue: UCX mem registration + heap allocator")
    }

    /// Free a symmetric allocation (`shmem_free`).
    pub fn free(_ptr: *mut u8) {
        todo!("issue: UCX mem deregistration + heap free")
    }
}
