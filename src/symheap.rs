//! Symmetric heap: allocation/free and the PE→rkey map.
//!
//! A symmetric allocation is represented by [`SymPtr`], a private `usize` (the
//! allocation's address within the registered heap region) that converts into the
//! UCX remote pointer (`u64`) for a target PE. It is deliberately NOT a raw
//! `*mut u8`: on a remote PE the same virtual offset points at a different physical
//! address, so application code must never dereference a `SymPtr` across PEs.
//!
//! Mirrors the OSSS-UCX `translate_address` / `get_remote_key_and_addr` flow: a
//! local address (as `uint64_t`) is resolved to a `(remote u64 address, rkey)` pair
//! for the target PE.

use crate::error::Result;

/// A symmetric-heap address. The inner `usize` is private to this crate.
///
/// Construct via [`SymAlloc::malloc`]; convert to a UCX remote address for a
/// specific PE with [`SymPtr::to_remote_addr`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymPtr(pub(crate) usize);

impl SymPtr {
    /// Virtual offset of this allocation from the local heap base.
    #[allow(dead_code)] // consumed by later RMA phases
    pub(crate) fn offset_from(self, local_base: u64) -> u64 {
        (self.0 as u64).wrapping_sub(local_base)
    }

    /// The UCX remote pointer for a target PE, preserving the virtual offset:
    /// `peer_base + (local_address - local_base)`, with wrapping arithmetic.
    #[allow(dead_code)] // consumed by later RMA phases
    pub(crate) fn to_remote_addr(self, local_base: u64, peer_base: u64) -> u64 {
        peer_base.wrapping_add(self.offset_from(local_base))
    }
}

#[cfg(test)]
mod tests {
    use super::SymPtr;

    #[test]
    fn offset_is_the_same_for_symmetric_bases() {
        let offset = 0x1234_u64;
        let local = SymPtr((0x1000 + offset) as usize);
        let peer = SymPtr((0x9000 + offset) as usize);
        assert_eq!(local.offset_from(0x1000), peer.offset_from(0x9000));
    }

    #[test]
    fn remote_address_preserves_virtual_offset() {
        let local_base = 0x1000_u64;
        let address = 0x2234_u64;
        let peer_base = 0x9000_u64;
        let ptr = SymPtr(address as usize);
        assert_eq!(
            ptr.to_remote_addr(local_base, peer_base),
            peer_base + (address - local_base)
        );
    }

    #[test]
    fn symmetric_peers_compute_consistent_remote_addresses() {
        let heap_a = 0x1000_u64;
        let heap_b = 0x9000_u64;
        let offset = 0x1234_u64;
        let address_a = SymPtr((heap_a + offset) as usize);
        let address_b = SymPtr((heap_b + offset) as usize);

        assert_eq!(address_a.to_remote_addr(heap_a, heap_b), heap_b + offset);
        assert_eq!(address_b.to_remote_addr(heap_b, heap_a), heap_a + offset);
    }
}

/// The symmetric-heap allocator, backed by a vendored pool registered with UCX.
pub struct SymAlloc;

impl SymAlloc {
    /// Allocate `size` bytes in the symmetric heap (`shmem_malloc`).
    pub fn malloc(_size: usize) -> Result<SymPtr> {
        todo!("issue #4: vendored jemalloc pool + UCX registration")
    }

    /// Free a symmetric allocation (`shmem_free`).
    pub fn free(_ptr: SymPtr) -> Result<()> {
        todo!("issue #4: pool free")
    }
}
