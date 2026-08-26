//! UCX transport setup and capability gating.
//!
//! The transport is deliberately gated by UCX context creation with the RMA
//! feature. This asks UCX, rather than parsing `ucx_info -d`, whether the
//! locally configured transports can provide the requested UCP API.
//!
//! UCX transport expectations:
//! - `self`/`cma`/intra-node transports support single-PE loopback.
//! - `tcp` supports zcopy RMA, but does not provide atomics for this phase.
//! - `ib`/`rc` and `rc_verbs` are the production inter-node transports.
//!
//! Transport selection and cross-node endpoint exchange are intentionally left
//! to later phases. A successful [`UcxTransport::new`] only claims that UCX
//! accepted the RMA context features on this process.

use ucx_sys::context;
use ucx_sys::context::Context;
use ucx_sys::ep;
use ucx_sys::ucs_thread_mode_t;
use ucx_sys::worker::{MtWorker, RemoteWorkerAddress};
use ucx_sys::{Request, RequestParamBuilder};

use crate::error::{Error, Result};

/// The capabilities required by the phase-0 transport bootstrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilities {
    /// UCP RMA put/get operations are available.
    pub rma: bool,
    /// The worker can be used for single-PE loopback addressing.
    pub loopback: bool,
}

/// An owned UCX context and thread-safe worker for one OpenSHMEM PE.
#[derive(Debug)]
pub struct UcxTransport {
    worker: MtWorker,
    #[allow(dead_code)]
    context: Context,
    packed_address: Vec<u8>,
    capabilities: TransportCapabilities,
}

impl UcxTransport {
    /// Borrow the UCX context for registration and key packing.
    pub(crate) fn context(&self) -> &context::Context {
        &self.context
    }

    /// Create a context requesting RMA and exported-memory-handle support.
    ///
    /// Context creation is the capability gate: UCX returns an error when no
    /// usable transport satisfies the requested feature set. The worker is
    /// serialized by its binding-level mutex so this handle can be retained in
    /// the process-global lifecycle state.
    pub fn new(estimated_num_eps: usize) -> Result<Self> {
        let features = context::Flags::Tag;
        let mut params_builder = context::ParamsBuilder::new();
        params_builder
            .features(features)
            .mt_workers_shared(1)
            .estimated_num_eps(estimated_num_eps);
        let params = params_builder.build();
        let config = context::Config::read("", "").map_err(|error| match error {
            context::ConfigError::Ucs(status) => Error::from(status),
            context::ConfigError::Nul(_) => Error::Internal("invalid UCX configuration string"),
        })?;
        let mut context = Context::new(&config, &params).map_err(Error::from)?;
        drop(config);

        let worker_params = ucx_sys::worker::ParamsBuilder::new()
            .thread_mode(ucs_thread_mode_t::UCS_THREAD_MODE_SERIALIZED)
            .build();
        let worker = context.worker_create(&worker_params).map_err(Error::from)?;
        let packed_address = worker.pack_address().map_err(Error::from)?.to_vec();
        let worker = MtWorker::new(worker).map_err(Error::from)?;
        Ok(Self {
            context,
            worker,
            capabilities: TransportCapabilities {
                rma: true,
                loopback: !packed_address.is_empty(),
            },
            packed_address,
        })
    }

    /// Create an endpoint addressed to this worker, useful for PE-0 loopback.
    pub fn loopback_endpoint(&self) -> std::result::Result<ep::Ep, Error> {
        if !self.capabilities.loopback {
            return Err(Error::Internal("UCX worker has no loopback address"));
        }
        let address = RemoteWorkerAddress::new(self.packed_address.clone());
        self.worker
            .create_ep(ep::ParamsBuilder::new().address(&address).build())
            .map_err(Error::from)
    }

    /// Return the capability decision made during construction.
    pub fn capabilities(&self) -> TransportCapabilities {
        self.capabilities
    }

    /// Return the packed worker address for a future PMIx exchange.
    pub fn packed_address(&self) -> &[u8] {
        &self.packed_address
    }

    pub(crate) fn create_endpoint(
        &self,
        address: &RemoteWorkerAddress,
    ) -> std::result::Result<ep::Ep, Error> {
        self.worker
            .create_ep(ep::ParamsBuilder::new().address(address).build())
            .map_err(Error::from)
    }

    pub(crate) fn wait_request(&self, request: &Request) -> Result<()> {
        match self.worker.wait_request(request).map_err(Error::from)? {
            true => Ok(()),
            false => Err(Error::Internal("UCX request completion timed out")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn creates_rma_worker_and_loopback_capability() {
        let transport = UcxTransport::new(1).expect("UCX RMA context and worker");
        assert!(transport.capabilities().rma);
        assert!(transport.capabilities().loopback);
        assert!(!transport.packed_address().is_empty());
    }

    #[test]
    fn creates_a_self_addressed_endpoint() {
        let transport = UcxTransport::new(1).expect("UCX RMA context and worker");
        let _endpoint = transport.loopback_endpoint().expect("self endpoint");
    }
}

/// Plain-old-data supported by the typed RMA interface.
pub trait Pod: Copy + Sized {
    const SIZE: usize;
    fn encode(self, dst: &mut Vec<u8>);
    fn decode(src: &[u8]) -> Result<Self>;
}

macro_rules! pod_types {
    ($($ty:ty => $size:expr),+ $(,)?) => {$ (
        impl Pod for $ty {
            const SIZE: usize = $size;
            fn encode(self, dst: &mut Vec<u8>) { dst.extend_from_slice(&self.to_ne_bytes()); }
            fn decode(src: &[u8]) -> Result<Self> {
                let bytes: [u8; $size] = src.try_into().map_err(|_| Error::Internal("invalid typed RMA byte count"))?;
                Ok(<$ty>::from_ne_bytes(bytes))
            }
        }
    )+ };
}

pod_types!(u8 => 1, i8 => 1, u16 => 2, i16 => 2, u32 => 4, i32 => 4,
           u64 => 8, i64 => 8, f32 => 4, f64 => 8);

fn peer_and_address(
    state: &crate::init::ShmemState,
    pe: usize,
    offset: usize,
    len: usize,
) -> Result<(&crate::bootstrap::PeerConnection, u64)> {
    let pe = u32::try_from(pe).map_err(|_| Error::Usage("PE number is out of range"))?;
    let peer = state
        .peers
        .get(&pe)
        .ok_or(Error::Usage("PE number is not in the job"))?;
    let offset = u64::try_from(offset).map_err(|_| Error::Usage("RMA offset is out of range"))?;
    let address = peer
        .heap_base
        .checked_add(offset)
        .ok_or(Error::Usage("RMA address overflow"))?;
    offset
        .checked_add(u64::try_from(len).map_err(|_| Error::Usage("RMA length is out of range"))?)
        .ok_or(Error::Usage("RMA range overflow"))?;
    Ok((peer, address))
}

/// Put raw bytes in the destination PE's symmetric heap.
///
/// UCX may return an asynchronous request. This safe wrapper waits for that
/// request before returning, because the source is borrowed; future `quiet`
/// support can provide deferred OpenSHMEM completion for owned buffers.
pub fn putmem(dst_pe: usize, bytes: &[u8], dst_offset: usize) -> Result<()> {
    crate::init::with_state(|state| {
        let (peer, address) = peer_and_address(state, dst_pe, dst_offset, bytes.len())?;
        if bytes.is_empty() {
            return Ok(());
        }
        let params = RequestParamBuilder::new().build();
        let request = peer
            .endpoint
            .rma_put(bytes, address, &peer.rkey, &params)
            .map_err(Error::from)?;
        if let Some(request) = request {
            state.transport.wait_request(&request)?;
            request.free();
        }
        Ok(())
    })
}

/// Get raw bytes, decoding only after UCX request completion.
pub fn getmem(src_pe: usize, src_offset: usize, len: usize) -> Result<Vec<u8>> {
    crate::init::with_state(|state| {
        let (peer, address) = peer_and_address(state, src_pe, src_offset, len)?;
        let mut bytes = vec![0_u8; len];
        if bytes.is_empty() {
            return Ok(bytes);
        }
        let params = RequestParamBuilder::new().build();
        let request = peer
            .endpoint
            .rma_get(&mut bytes, address, &peer.rkey, &params)
            .map_err(Error::from)?;
        if let Some(request) = request {
            state.transport.wait_request(&request)?;
            request.free();
        }
        Ok(bytes)
    })
}

/// Put typed POD values in native-endian representation.
pub fn put<T: Pod>(dst_pe: usize, src: &[T], dst_offset: usize) -> Result<()> {
    let capacity = src
        .len()
        .checked_mul(T::SIZE)
        .ok_or(Error::Usage("typed RMA length overflow"))?;
    let mut bytes = Vec::with_capacity(capacity);
    for &value in src {
        value.encode(&mut bytes);
    }
    putmem(dst_pe, &bytes, dst_offset)
}

/// Get typed POD values after the underlying UCX request has completed.
pub fn get<T: Pod>(src_pe: usize, src_offset: usize, len_elems: usize) -> Result<Vec<T>> {
    let len = len_elems
        .checked_mul(T::SIZE)
        .ok_or(Error::Usage("typed RMA length overflow"))?;
    getmem(src_pe, src_offset, len)?
        .chunks_exact(T::SIZE)
        .map(T::decode)
        .collect()
}

#[cfg(test)]
mod pod_tests {
    use super::*;

    fn roundtrip<T: Pod + PartialEq + std::fmt::Debug>(value: T) {
        let mut bytes = Vec::new();
        value.encode(&mut bytes);
        assert_eq!(T::decode(&bytes).unwrap(), value);
        assert_eq!(bytes.len(), T::SIZE);
    }

    #[test]
    fn pod_covers_all_supported_scalar_types() {
        roundtrip(1_u8);
        roundtrip(-1_i8);
        roundtrip(2_u16);
        roundtrip(-2_i16);
        roundtrip(3_u32);
        roundtrip(-3_i32);
        roundtrip(4_u64);
        roundtrip(-4_i64);
        roundtrip(1.5_f32);
        roundtrip(-2.5_f64);
    }

    #[test]
    fn typed_length_overflow_is_rejected_before_state_lookup() {
        assert!(matches!(get::<u64>(0, 0, usize::MAX), Err(Error::Usage(_))));
    }
}
