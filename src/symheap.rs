//! Symmetric heap allocation, UCX registration, and per-PE rkeys.
//!
//! The heap is one contiguous process-local region registered once with UCX. A
//! small free-list allocator returns [`SymPtr`] values, which are deliberately
//! not raw pointers: a symmetric pointer is an address-like private token whose
//! offset is translated to a peer's heap base before an RMA operation.

use std::collections::BTreeMap;

use ucx_sys::{context::Context, memh};

use crate::{
    error::{Error, Result},
    rma::UcxTransport,
};

const DEFAULT_HEAP_SIZE: usize = 64 * 1024 * 1024;
const ALIGNMENT: usize = 8;

/// An address in the symmetric heap; application code cannot dereference it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SymPtr(pub(crate) usize);

impl SymPtr {
    /// Return the virtual offset from a local heap base.
    #[allow(dead_code)]
    pub(crate) fn offset_from(self, local_base: u64) -> u64 {
        (self.0 as u64).wrapping_sub(local_base)
    }

    /// Translate this allocation's offset to a target PE's heap address.
    #[allow(dead_code)]
    pub(crate) fn to_remote_addr(self, local_base: u64, peer_base: u64) -> u64 {
        peer_base.wrapping_add(self.offset_from(local_base))
    }
}

#[derive(Debug, Clone, Copy)]
struct Block {
    start: usize,
    len: usize,
}

/// Heap base and packed rkey advertised by one PE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRkey {
    pub heap_base: u64,
    pub rkey: Vec<u8>,
}

/// Per-PE heap metadata, populated by the later PMIx exchange.
#[derive(Debug, Default, Clone)]
pub struct PeerRkeys(BTreeMap<u32, PeerRkey>);

impl PeerRkeys {
    /// Insert or replace a peer's heap metadata.
    pub fn insert(&mut self, pe: u32, heap_base: u64, rkey: Vec<u8>) {
        self.0.insert(pe, PeerRkey { heap_base, rkey });
    }

    /// Look up a peer's heap metadata.
    pub fn get(&self, pe: u32) -> Option<&PeerRkey> {
        self.0.get(&pe)
    }
}

/// A contiguous UCX-registered symmetric heap and its free-list allocator.
pub struct SymHeap {
    /// Declared first so UCX unmaps before the backing region is dropped.
    #[allow(dead_code)]
    memh: memh::MemHandle,
    region: Vec<u64>,
    free: Vec<Block>,
    allocations: BTreeMap<usize, usize>,
    packed_rkey: Vec<u8>,
    local_base: u64,
}

// SAFETY: After construction, the MemHandle is dormant until Drop; malloc and
// free operate only on the Rust allocator metadata and never touch it. The
// owning UCX Context is unsafe impl Send + Sync in ucx-rs (context.rs:273-280),
// and the Context outlives this MemHandle because ShmemState declares heap
// before transport. Send is required because the process-global
// OnceLock<Mutex<Option<ShmemState>>> requires ShmemState: Send. There is no
// unsafe-free alternative that preserves the symmetric-heap semantics.
unsafe impl Send for SymHeap {}

impl SymHeap {
    /// Create, register, and rkey-pack a heap using the transport's UCX context.
    pub fn new(transport: &UcxTransport) -> Result<Self> {
        let size = std::env::var("SHMEM_SYMMETRIC_SIZE")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_HEAP_SIZE);
        Self::with_size(transport.context(), size)
    }

    /// Create a heap of exactly `size` bytes. Primarily useful for tests.
    pub fn with_size(context: &Context, size: usize) -> Result<Self> {
        if size < ALIGNMENT {
            return Err(Error::Usage("symmetric heap is too small"));
        }
        let words = size
            .checked_add(std::mem::size_of::<u64>() - 1)
            .ok_or(Error::Usage("symmetric heap size overflow"))?
            / std::mem::size_of::<u64>();
        let capacity = words * std::mem::size_of::<u64>();
        let mut region = vec![0_u64; words];
        let local_base = region.as_mut_ptr() as u64;
        let mut params = memh::MemMapParamsBuilder::new();
        params
            .address(region.as_mut_ptr() as *mut std::ffi::c_void)
            .length(capacity);
        let memh = memh::MemHandle::map(context, &mut params).map_err(Error::from)?;
        let packed_rkey = memh::pack_rkey(context, &memh)
            .map_err(Error::from)?
            .as_bytes()
            .to_vec();
        Ok(Self {
            memh,
            region,
            free: vec![Block {
                start: 0,
                len: capacity,
            }],
            allocations: BTreeMap::new(),
            packed_rkey,
            local_base,
        })
    }

    /// Allocate at least `size` bytes, aligned to eight bytes.
    pub fn malloc(&mut self, size: usize) -> Result<SymPtr> {
        if size == 0 {
            return Err(Error::Usage("symmetric allocation size must be nonzero"));
        }
        let requested = size
            .checked_add(ALIGNMENT - 1)
            .ok_or(Error::Usage("symmetric allocation size overflow"))?
            & !(ALIGNMENT - 1);
        let index = self
            .free
            .iter()
            .position(|b| b.len >= requested)
            .ok_or(Error::Usage("symmetric heap exhausted"))?;
        let block = self.free[index];
        let ptr = SymPtr(self.local_base as usize + block.start);
        if block.len == requested {
            self.free.remove(index);
        } else {
            self.free[index] = Block {
                start: block.start + requested,
                len: block.len - requested,
            };
        }
        self.allocations.insert(ptr.0, requested);
        Ok(ptr)
    }

    /// Return an allocation to the free list; double/foreign frees are errors.
    pub fn free(&mut self, ptr: SymPtr) -> Result<()> {
        let Some(len) = self.allocations.remove(&ptr.0) else {
            return Err(Error::Usage("invalid symmetric pointer"));
        };
        let start = ptr
            .0
            .checked_sub(self.local_base as usize)
            .ok_or(Error::Usage("invalid symmetric pointer"))?;
        if start
            .checked_add(len)
            .filter(|&end| end <= self.region.len() * std::mem::size_of::<u64>())
            .is_none()
            || start % ALIGNMENT != 0
        {
            return Err(Error::Usage("invalid symmetric pointer"));
        }
        self.free.push(Block { start, len });
        self.free.sort_by_key(|b| b.start);
        let mut merged: Vec<Block> = Vec::with_capacity(self.free.len());
        for block in self.free.drain(..) {
            if let Some(last) = merged.last_mut() {
                if last.start + last.len == block.start {
                    last.len += block.len;
                    continue;
                }
            }
            merged.push(block);
        }
        self.free = merged;
        Ok(())
    }

    pub fn local_base(&self) -> u64 {
        self.local_base
    }
    pub fn packed_rkey(&self) -> &[u8] {
        &self.packed_rkey
    }
    pub fn capacity(&self) -> usize {
        self.region.len() * std::mem::size_of::<u64>()
    }
}

/// Thin handle for allocations from a stored symmetric heap.
pub struct SymAlloc(std::sync::Mutex<SymHeap>);

impl SymAlloc {
    pub fn new(transport: &UcxTransport) -> Result<Self> {
        Ok(Self(std::sync::Mutex::new(SymHeap::new(transport)?)))
    }
    pub fn malloc(&self, size: usize) -> Result<SymPtr> {
        self.0
            .lock()
            .map_err(|_| Error::Internal("symmetric heap lock poisoned"))?
            .malloc(size)
    }
    pub fn free(&self, ptr: SymPtr) -> Result<()> {
        self.0
            .lock()
            .map_err(|_| Error::Internal("symmetric heap lock poisoned"))?
            .free(ptr)
    }
    pub fn local_base(&self) -> Result<u64> {
        Ok(self
            .0
            .lock()
            .map_err(|_| Error::Internal("symmetric heap lock poisoned"))?
            .local_base())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rma::UcxTransport;

    #[test]
    fn allocator_alignment_distinct_and_reuses_freed_space() {
        let transport = UcxTransport::new(1).unwrap();
        let mut heap = SymHeap::with_size(&transport.context(), 128).unwrap();
        let a = heap.malloc(1).unwrap();
        let b = heap.malloc(8).unwrap();
        assert_eq!(a.0 % ALIGNMENT, 0);
        assert_eq!(b.0 % ALIGNMENT, 0);
        assert_ne!(a, b);
        heap.free(a).unwrap();
        assert_eq!(heap.malloc(8).unwrap(), a);
    }

    #[test]
    fn frees_allocation_at_end_of_multimegabyte_heap() {
        let transport = UcxTransport::new(1).unwrap();
        let mut heap = SymHeap::with_size(&transport.context(), 2 * 1024 * 1024).unwrap();
        let capacity = heap.capacity();
        let _prefix = heap.malloc(capacity - ALIGNMENT).unwrap();
        let tail = heap.malloc(ALIGNMENT).unwrap();
        assert_eq!(tail.0 - heap.local_base() as usize, capacity - ALIGNMENT);
        heap.free(tail).unwrap();
    }

    #[test]
    fn registration_and_rkey_pack_succeed() {
        let transport = UcxTransport::new(1).unwrap();
        let heap = SymHeap::with_size(&transport.context(), 4096).unwrap();
        assert_ne!(heap.local_base(), 0);
        assert!(!heap.packed_rkey().is_empty());
    }

    #[test]
    fn peer_rkeys_insert_and_update() {
        let mut keys = PeerRkeys::default();
        keys.insert(2, 9, vec![1]);
        keys.insert(2, 10, vec![2]);
        assert_eq!(keys.get(2).unwrap().heap_base, 10);
        assert_eq!(keys.get(3), None);
    }

    #[test]
    fn symmetric_address_translation_preserves_offset() {
        let ptr = SymPtr(0x2234);
        assert_eq!(ptr.to_remote_addr(0x1000, 0x9000), 0xa234);
    }
}

#[cfg(test)]
mod legacy_tests {
    use super::SymPtr;
    #[test]
    fn symmetric_peers_compute_consistent_remote_addresses() {
        let a = SymPtr(0x2234);
        let b = SymPtr(0xa234);
        assert_eq!(a.to_remote_addr(0x1000, 0x9000), 0xa234);
        assert_eq!(b.to_remote_addr(0x9000, 0x1000), 0x2234);
    }
}
