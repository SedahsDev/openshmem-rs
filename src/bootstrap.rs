//! PMIx KVS bootstrap for UCX worker addresses and symmetric-heap keys.

use std::{collections::BTreeMap, ffi::CStr};

use pmix::{PmixClient, PmixScope, PmixValueBuilder, get_value, put_value};
use ucx_sys::{ep::Ep, rma::RemoteKey, worker::RemoteWorkerAddress};

use crate::{
    error::{Error, Result},
    rma::UcxTransport,
    symheap::{PeerRkeys, SymHeap},
};

pub static WORKER_ADDR_KEY: &CStr = c"shmem.worker.addr";
pub static HEAP_RKEY_KEY: &CStr = c"shmem.heap.rkey";
pub static HEAP_BASE_KEY: &CStr = c"shmem.heap.base";

const WORKER_ADDR_GET_KEY: &[u8] = b"shmem.worker.addr\0";
const HEAP_RKEY_GET_KEY: &[u8] = b"shmem.heap.rkey\0";
const HEAP_BASE_GET_KEY: &[u8] = b"shmem.heap.base\0";

pub struct PeerConnection {
    pub(crate) endpoint: Ep,
    pub(crate) rkey: RemoteKey,
    pub heap_base: u64,
}

// SAFETY: `Ep` and `RemoteKey` contain raw UCX handles and are intentionally
// `!Send`. A `PeerConnection` is constructed only by `handshake` and is kept
// in the private `ShmemState`; every access to that state, including the
// eventual drop of these handles, is serialized by `STATE`. `ShmemState`
// declares peers before the heap and transport, so endpoints and rkeys are
// destroyed while their UCX worker/context are still alive; the heap then
// unmaps while the context remains alive. No handle is moved or accessed
// concurrently with another thread while live. `Send` is required because
// the process-global `OnceLock<Mutex<Option<ShmemState>>>` owns the state and
// may move it between the initializing and finalizing threads. There is no
// safe alternative that preserves ownership of these opaque UCX handles and
// the OpenSHMEM process-global lifecycle.
unsafe impl Send for PeerConnection {}

pub struct Bootstrap {
    pub peer_rkeys: PeerRkeys,
    pub peers: BTreeMap<u32, PeerConnection>,
}

fn pmix_error(status: pmix::ffi::pmix_status_t) -> Error {
    Error::Pmix(pmix::PmixError::from_raw(status).unwrap_or(pmix::PmixError::Error))
}

fn put_bytes(key: &CStr, bytes: &[u8]) -> Result<()> {
    let mut value = PmixValueBuilder::new()
        .byte_object(bytes)
        .map_err(|_| Error::Usage("bootstrap byte value must not be empty"))?
        .build()
        .map_err(|_| Error::Usage("failed to build bootstrap byte value"))?;
    put_value(PmixScope::Global.to_raw(), key, &mut value).map_err(pmix_error)
}

/// Publish metadata, fence, then create an endpoint and unpacked rkey per PE.
pub fn handshake(
    client: &PmixClient,
    transport: &UcxTransport,
    heap: &SymHeap,
    size: usize,
) -> Result<Bootstrap> {
    if size == 0 {
        return Err(Error::Usage("bootstrap job size must be nonzero"));
    }
    put_bytes(WORKER_ADDR_KEY, transport.packed_address())?;
    put_bytes(HEAP_RKEY_KEY, heap.packed_rkey())?;
    let mut base = PmixValueBuilder::new()
        .uint64(heap.local_base())
        .build()
        .map_err(|_| Error::Usage("failed to build heap base value"))?;
    put_value(PmixScope::Global.to_raw(), HEAP_BASE_KEY, &mut base).map_err(pmix_error)?;
    pmix::commit().map_err(pmix_error)?;
    let wildcard = client
        .proc_with_nspace(pmix::RANK_WILDCARD)
        .map_err(Error::from)?;
    pmix::fence(&wildcard, None).map_err(pmix_error)?;

    let mut peer_rkeys = PeerRkeys::default();
    let mut peers = BTreeMap::new();
    for pe in 0..size as u32 {
        let peer_proc = client.proc_with_nspace(pe).map_err(Error::from)?;
        let address = get_value(&peer_proc, WORKER_ADDR_GET_KEY, None)
            .map_err(Error::from)?
            .bytes_copy();
        let rkey_bytes = get_value(&peer_proc, HEAP_RKEY_GET_KEY, None)
            .map_err(Error::from)?
            .bytes_copy();
        let heap_base = get_value(&peer_proc, HEAP_BASE_GET_KEY, None)
            .map_err(Error::from)?
            .uint64();
        if address.is_empty() || rkey_bytes.len() < 4 || heap_base == 0 {
            return Err(Error::Internal(
                "peer published incomplete bootstrap metadata",
            ));
        }
        let remote_address = RemoteWorkerAddress::new(address);
        let endpoint = transport.create_endpoint(&remote_address)?;
        let rkey = RemoteKey::unpack(&endpoint, &rkey_bytes).map_err(Error::from)?;
        peer_rkeys.insert(pe, heap_base, rkey_bytes);
        peers.insert(
            pe,
            PeerConnection {
                endpoint,
                rkey,
                heap_base,
            },
        );
    }
    Ok(Bootstrap { peer_rkeys, peers })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_nul_free_content_and_nul_terminated_for_get() {
        for (put, get) in [
            (WORKER_ADDR_KEY, WORKER_ADDR_GET_KEY),
            (HEAP_RKEY_KEY, HEAP_RKEY_GET_KEY),
            (HEAP_BASE_KEY, HEAP_BASE_GET_KEY),
        ] {
            assert!(!put.to_bytes().contains(&0));
            assert_eq!(get.last(), Some(&0));
            assert_eq!(&get[..get.len() - 1], put.to_bytes());
        }
    }

    #[test]
    fn framed_rkey_roundtrip_is_preserved_in_peer_map() {
        let framed = [3, 0, 0, 0, 9, 8, 7];
        let mut peers = PeerRkeys::default();
        peers.insert(1, 0x1000, framed.to_vec());
        assert_eq!(peers.get(1).unwrap().rkey, framed);
    }

    #[test]
    #[ignore = "requires DVM-launched process"]
    fn dvm_exchanges_worker_heap_metadata() {
        let client = PmixClient::connect_new(None).expect("PMIx init");
        let transport = UcxTransport::new(2).expect("UCX worker");
        let heap = SymHeap::new(&transport).expect("symmetric heap");
        let size = client
            .proc_with_nspace(pmix::RANK_WILDCARD)
            .ok()
            .and_then(|proc| get_value(&proc, pmix::JOB_SIZE, None).ok())
            .map(|value| value.uint32() as usize)
            .unwrap_or(2);
        let result = handshake(&client, &transport, &heap, size).expect("bootstrap handshake");
        assert_eq!(result.peers.len(), size);
        assert!(result.peers.values().all(|peer| peer.heap_base != 0));
        client.disconnect(None).expect("PMIx finalize");
    }
}
