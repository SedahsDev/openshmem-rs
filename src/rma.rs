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
//! ## Atomic operations
//!
//! The typed atomic API follows the AMO surface exposed by `ucx-rs`: 32-bit and
//! 64-bit integer families (`u32`/`i32` and `u64`/`i64`) support add, xor,
//! swap, compare-and-swap, and non-fetch and/or. Fetch add, xor, swap, and
//! compare-and-swap are also provided. Signed values preserve their two's-
//! complement bit pattern when converted to the unsigned UCX operation.
//!
//! UCX exposes no 8/16-bit or floating-point AMOs, nor fetch-and/or,
//! increment/decrement, or fetch-only operations. Those operations are
//! deliberately not represented rather than emulated with non-atomic RMA.
//!
//! Loopback/self transport cannot run UCX atomics; its atomic path uses the
//! active-message interface without an atomic-message handler. Atomic
//! operations therefore require an AMO-capable transport (`ib`/`rc`,
//! `rc_verbs`). Cross-PE atomic tests are DVM-gated.

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
        let features = context::Flags::Tag | context::Flags::Rma;
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

    /// Wait for a request until UCX reports completion.
    ///
    /// The binding's helper has a fixed one-million-round budget. RMA
    /// operations can legitimately need more progress rounds for large
    /// transfers, so this wrapper retries that helper while it reports an
    /// incomplete request instead of treating the budget as a timeout.
    pub(crate) fn wait_request(&self, request: &Request) -> Result<()> {
        loop {
            match self.worker.wait_request(request).map_err(Error::from)? {
                true => return Ok(()),
                false => continue,
            }
        }
    }

    /// Borrow the underlying worker while holding its serialization lock.
    pub(crate) fn with_worker<F, T>(&self, operation: F) -> T
    where
        F: FnOnce(&ucx_sys::worker::Worker) -> T,
    {
        self.worker.with_worker(operation)
    }

    pub(crate) fn fence(&self) -> Result<()> {
        self.worker.fence().map_err(Error::from)
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
    fn loopback_rma_put_get_roundtrip_uses_real_completion_path() {
        use ucx_sys::rma::RemoteKey;

        let transport = UcxTransport::new(1).expect("UCX RMA context and worker");
        let mut target = vec![0_u8; 4096];
        let memh = ucx_sys::memh::MemHandle::map_slice(transport.context(), &mut target, 0)
            .expect("registered heap");
        let packed_rkey = ucx_sys::rma::RemoteKey::pack(transport.context(), memh.mem_handle())
            .expect("pack heap rkey");
        let endpoint = transport.loopback_endpoint().expect("self endpoint");
        let rkey = RemoteKey::unpack(&endpoint, &packed_rkey).expect("own framed rkey");
        let params = RequestParamBuilder::new().no_imm_cmpl().build();
        let source = b"loopback RMA";
        let mut destination = vec![0_u8; source.len()];

        if let Some(request) = endpoint
            .rma_put(source, target.as_mut_ptr() as u64, &rkey, &params)
            .expect("loopback put")
        {
            transport.wait_request(&request).expect("put completion");
            request.free();
        }
        transport.fence().expect("ordering fence");
        if let Some(request) = endpoint
            .rma_put(source, target.as_mut_ptr() as u64, &rkey, &params)
            .expect("second loopback put")
        {
            transport
                .wait_request(&request)
                .expect("second put completion");
            request.free();
        }
        let flush_params = RequestParamBuilder::new().build();
        if let Some(request) = endpoint.flush(&flush_params).expect("endpoint flush") {
            transport.wait_request(&request).expect("flush completion");
            request.free();
        }
        if let Some(request) = endpoint
            .rma_get(&mut destination, target.as_mut_ptr() as u64, &rkey, &params)
            .expect("loopback get")
        {
            transport.wait_request(&request).expect("get completion");
            request.free();
        }

        assert_eq!(destination, source);
    }
}

/// Plain-old-data supported by the typed RMA interface.
pub trait Pod: Copy + Sized {
    const SIZE: usize;
    fn encode(self, dst: &mut Vec<u8>);
    fn decode(src: &[u8]) -> Result<Self>;
    #[cfg(feature = "collectives")]
    fn ucc_datatype() -> ucc::collective::DataType;
    #[cfg(feature = "collectives")]
    fn reduction_supported(op: ucc::collective::UccReductionOp) -> bool;
}

macro_rules! pod_types {
    ($($ty:ty => $size:expr => $ucc:expr => $reduce:expr),+ $(,)?) => {$ (
        impl Pod for $ty {
            const SIZE: usize = $size;
            fn encode(self, dst: &mut Vec<u8>) { dst.extend_from_slice(&self.to_ne_bytes()); }
            fn decode(src: &[u8]) -> Result<Self> {
                let bytes: [u8; $size] = src.try_into().map_err(|_| Error::Internal("invalid typed RMA byte count"))?;
                Ok(<$ty>::from_ne_bytes(bytes))
            }
            #[cfg(feature = "collectives")]
            fn ucc_datatype() -> ucc::collective::DataType { $ucc }
            #[cfg(feature = "collectives")]
            fn reduction_supported(op: ucc::collective::UccReductionOp) -> bool { $reduce(op) }
        }
    )+ };
}

pod_types!(
    u8 => 1 => ucc::collective::DataType::Uint8 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    i8 => 1 => ucc::collective::DataType::Int8 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    u16 => 2 => ucc::collective::DataType::Uint16 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    i16 => 2 => ucc::collective::DataType::Int16 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    u32 => 4 => ucc::collective::DataType::Uint32 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    i32 => 4 => ucc::collective::DataType::Int32 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    u64 => 8 => ucc::collective::DataType::Uint64 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    i64 => 8 => ucc::collective::DataType::Int64 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max | ucc::collective::UccReductionOp::Band | ucc::collective::UccReductionOp::Bor | ucc::collective::UccReductionOp::Bxor),
    f32 => 4 => ucc::collective::DataType::Float32 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max),
    f64 => 8 => ucc::collective::DataType::Float64 => |op| matches!(op, ucc::collective::UccReductionOp::Sum | ucc::collective::UccReductionOp::Min | ucc::collective::UccReductionOp::Max),
);

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

fn complete_request(transport: &UcxTransport, request: Option<Request>) -> Result<()> {
    if let Some(request) = request {
        transport.wait_request(&request)?;
        request.free();
    }
    Ok(())
}

fn complete_fetch<T>(
    worker: &ucx_sys::worker::Worker,
    request: ucx_sys::rma::FetchAmoRequest<'_, '_, T>,
) -> Result<()> {
    loop {
        match request.check_finished().map_err(Error::from)? {
            true => break,
            false => {
                worker.progress();
            }
        }
    }
    request.free();
    Ok(())
}

/// Typed AMO operations supported by UCX's 32/64-bit integer surface.
///
/// The `T` type parameter is public so callers can use the generated API with
/// `u32`, `i32`, `u64`, or `i64`; implementations are generated by one macro
/// per width family.
/// Signed implementations preserve the unsigned two's-complement bit pattern.
pub trait AtomicValue: Copy + 'static {
    type Bits: Copy;
    const SIZE: usize;
    fn bits(self) -> Self::Bits;
    fn from_bits(bits: Self::Bits) -> Self;
    fn add(
        ep: &ep::Ep,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn xor(
        ep: &ep::Ep,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn swap(
        ep: &ep::Ep,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn and(
        ep: &ep::Ep,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn or(
        ep: &ep::Ep,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn cswap(
        ep: &ep::Ep,
        expected: Self::Bits,
        replacement: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        p: &ucx_sys::RequestParam,
    ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t>;
    fn fadd<'w, 'a>(
        ep: &ep::Ep,
        w: &'w ucx_sys::worker::Worker,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        reply: &'a mut Self::Bits,
    ) -> std::result::Result<ucx_sys::rma::FetchAmoRequest<'w, 'a, Self::Bits>, ucx_sys::ucs_status_t>;
    fn fxor<'w, 'a>(
        ep: &ep::Ep,
        w: &'w ucx_sys::worker::Worker,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        reply: &'a mut Self::Bits,
    ) -> std::result::Result<ucx_sys::rma::FetchAmoRequest<'w, 'a, Self::Bits>, ucx_sys::ucs_status_t>;
    fn fswap<'w, 'a>(
        ep: &ep::Ep,
        w: &'w ucx_sys::worker::Worker,
        operand: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        reply: &'a mut Self::Bits,
    ) -> std::result::Result<ucx_sys::rma::FetchAmoRequest<'w, 'a, Self::Bits>, ucx_sys::ucs_status_t>;
    fn fcswap<'w, 'a>(
        ep: &ep::Ep,
        w: &'w ucx_sys::worker::Worker,
        expected: Self::Bits,
        replacement: Self::Bits,
        addr: u64,
        key: &ucx_sys::rma::RemoteKey,
        reply: &'a mut Self::Bits,
    ) -> std::result::Result<ucx_sys::rma::FetchAmoRequest<'w, 'a, Self::Bits>, ucx_sys::ucs_status_t>;
}

macro_rules! atomic_family {
    ($unsigned:ty, $signed:ty, $add:ident, $xor:ident, $swap:ident, $and:ident, $or:ident, $cswap:ident, $fadd:ident, $fxor:ident, $fswap:ident, $fcswap:ident) => {
        impl AtomicValue for $unsigned {
            type Bits = $unsigned;
            const SIZE: usize = std::mem::size_of::<$unsigned>();
            fn bits(self) -> Self::Bits {
                self
            }
            fn from_bits(v: Self::Bits) -> Self {
                v
            }
            fn add(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$add(v, a, k, p)
            }
            fn xor(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$xor(v, a, k, p)
            }
            fn swap(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$swap(v, a, k, p)
            }
            fn and(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$and(v, a, k, p)
            }
            fn or(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$or(v, a, k, p)
            }
            fn cswap(
                e: &ep::Ep,
                x: $unsigned,
                y: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                e.$cswap(x, y, a, k, p)
            }
            fn fadd<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                e.$fadd(w, v, a, k, r)
            }
            fn fxor<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                e.$fxor(w, v, a, k, r)
            }
            fn fswap<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                e.$fswap(w, v, a, k, r)
            }
            fn fcswap<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                x: $unsigned,
                y: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                e.$fcswap(w, x, y, a, k, r)
            }
        }
        impl AtomicValue for $signed {
            type Bits = $unsigned;
            const SIZE: usize = std::mem::size_of::<$signed>();
            fn bits(self) -> Self::Bits {
                self as $unsigned
            }
            fn from_bits(v: Self::Bits) -> Self {
                v as $signed
            }
            fn add(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::add(e, v, a, k, p)
            }
            fn xor(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::xor(e, v, a, k, p)
            }
            fn swap(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::swap(e, v, a, k, p)
            }
            fn and(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::and(e, v, a, k, p)
            }
            fn or(
                e: &ep::Ep,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::or(e, v, a, k, p)
            }
            fn cswap(
                e: &ep::Ep,
                x: $unsigned,
                y: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                p: &ucx_sys::RequestParam,
            ) -> std::result::Result<Option<Request>, ucx_sys::ucs_status_t> {
                <$unsigned>::cswap(e, x, y, a, k, p)
            }
            fn fadd<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                <$unsigned>::fadd(e, w, v, a, k, r)
            }
            fn fxor<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                <$unsigned>::fxor(e, w, v, a, k, r)
            }
            fn fswap<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                v: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                <$unsigned>::fswap(e, w, v, a, k, r)
            }
            fn fcswap<'w, 'a>(
                e: &ep::Ep,
                w: &'w ucx_sys::worker::Worker,
                x: $unsigned,
                y: $unsigned,
                a: u64,
                k: &ucx_sys::rma::RemoteKey,
                r: &'a mut $unsigned,
            ) -> std::result::Result<
                ucx_sys::rma::FetchAmoRequest<'w, 'a, $unsigned>,
                ucx_sys::ucs_status_t,
            > {
                <$unsigned>::fcswap(e, w, x, y, a, k, r)
            }
        }
    };
}
atomic_family!(
    u32,
    i32,
    amo_add32,
    amo_xor32,
    amo_swap32,
    amo_and32,
    amo_or32,
    amo_cswap32,
    amo_fadd32,
    amo_fxor32,
    amo_fswap32,
    amo_fcswap32
);
atomic_family!(
    u64,
    i64,
    amo_add64,
    amo_xor64,
    amo_swap64,
    amo_and64,
    amo_or64,
    amo_cswap64,
    amo_fadd64,
    amo_fxor64,
    amo_fswap64,
    amo_fcswap64
);

macro_rules! atomic_op {
    ($name:ident, $method:ident) => {
        pub fn $name<T: AtomicValue>(pe: usize, offset: usize, value: T) -> Result<()> {
            crate::init::with_state(|state| {
                let (peer, addr) = peer_and_address(state, pe, offset, T::SIZE)?;
                let p = RequestParamBuilder::new()
                    .datatype(ucx_sys::dt::dt_make_contig(T::SIZE))
                    .build();
                complete_request(
                    &state.transport,
                    T::$method(&peer.endpoint, value.bits(), addr, &peer.rkey, &p)
                        .map_err(Error::from)?,
                )
            })
        }
    };
}
atomic_op!(atomic_add, add);
atomic_op!(atomic_xor, xor);
atomic_op!(atomic_swap, swap);
atomic_op!(atomic_and, and);
atomic_op!(atomic_or, or);

pub fn atomic_cswap<T: AtomicValue>(
    pe: usize,
    offset: usize,
    expected: T,
    replacement: T,
) -> Result<()> {
    crate::init::with_state(|state| {
        let (peer, addr) = peer_and_address(state, pe, offset, T::SIZE)?;
        let p = RequestParamBuilder::new()
            .datatype(ucx_sys::dt::dt_make_contig(T::SIZE))
            .build();
        complete_request(
            &state.transport,
            T::cswap(
                &peer.endpoint,
                expected.bits(),
                replacement.bits(),
                addr,
                &peer.rkey,
                &p,
            )
            .map_err(Error::from)?,
        )
    })
}

macro_rules! atomic_fetch {
    ($name:ident, $method:ident) => {
        pub fn $name<T: AtomicValue>(pe: usize, offset: usize, operand: T) -> Result<T> {
            crate::init::with_state(|state| {
                let (peer, addr) = peer_and_address(state, pe, offset, T::SIZE)?;
                let mut reply = operand.bits();
                state.transport.with_worker(|worker| {
                    let request = T::$method(
                        &peer.endpoint,
                        worker,
                        operand.bits(),
                        addr,
                        &peer.rkey,
                        &mut reply,
                    )
                    .map_err(Error::from)?;
                    complete_fetch(worker, request)?;
                    Ok(T::from_bits(reply))
                })
            })
        }
    };
}
atomic_fetch!(atomic_fadd, fadd);
atomic_fetch!(atomic_fxor, fxor);
atomic_fetch!(atomic_fswap, fswap);

pub fn atomic_fcswap<T: AtomicValue>(
    pe: usize,
    offset: usize,
    expected: T,
    replacement: T,
) -> Result<T> {
    crate::init::with_state(|state| {
        let (peer, addr) = peer_and_address(state, pe, offset, T::SIZE)?;
        let mut reply = expected.bits();
        state.transport.with_worker(|worker| {
            let request = T::fcswap(
                &peer.endpoint,
                worker,
                expected.bits(),
                replacement.bits(),
                addr,
                &peer.rkey,
                &mut reply,
            )
            .map_err(Error::from)?;
            complete_fetch(worker, request)?;
            Ok(T::from_bits(reply))
        })
    })
}

/// Order RMA operations issued before this call before RMA operations issued afterward.
///
/// This uses UCX's worker-scoped `ucp_worker_fence`, because ucx-rs exposes no
/// per-endpoint fence wrapper. OpenSHMEM defines `shmem_fence` on the default
/// context, so this process-wide ordering is stronger than per-PE ordering.
/// It is not a completion operation: use [`quiet`] when remote data must be
/// complete before proceeding.
pub fn fence() -> Result<()> {
    crate::init::with_state(|state| state.transport.fence())
}

/// Complete all outstanding RMA operations on every connected PE.
///
/// This is completion, not merely ordering: each endpoint flush waits until
/// prior RMA has completed remotely. In contrast, [`fence`] only orders RMA
/// before and after the call. Use quiet before observing remote results or
/// reusing data that was the source of a put. This loops over endpoints so
/// PE-specific completion remains explicit.
pub fn quiet() -> Result<()> {
    crate::init::with_state(|state| {
        let params = RequestParamBuilder::new().build();
        for peer in state.peers.values() {
            if let Some(request) = peer.endpoint.flush(&params).map_err(Error::from)? {
                state.transport.wait_request(&request)?;
                request.free();
            }
        }
        Ok(())
    })
}

/// Complete outstanding RMA operations for one PE.
pub fn quiet_pe(pe: usize) -> Result<()> {
    crate::init::with_state(|state| {
        let pe = u32::try_from(pe).map_err(|_| Error::Usage("PE number is out of range"))?;
        let peer = state
            .peers
            .get(&pe)
            .ok_or(Error::Usage("PE number is not in the job"))?;
        let params = RequestParamBuilder::new().build();
        if let Some(request) = peer.endpoint.flush(&params).map_err(Error::from)? {
            state.transport.wait_request(&request)?;
            request.free();
        }
        Ok(())
    })
}

/// Alias for [`quiet`], completing RMA operations for all PEs.
pub fn quiet_all() -> Result<()> {
    quiet()
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

    #[test]
    // The public `atomic_*` wrappers and signed bit-pattern casts are runtime-
    // untested pending an AMO-capable DVM; the gap is accepted until then.
    #[ignore = "self transport cannot execute UCX atomics (no AM handler); requires atomic-capable transport or DVM"]
    fn loopback_integer_atomics_use_real_completion_and_fetch_replies() {
        let transport = UcxTransport::new(1).expect("UCX transport");
        let mut target = vec![0_u64; 1];
        // SAFETY: u64 has no invalid bit patterns and the slice covers exactly
        // the initialized allocation for the duration of the registered handle.
        let target_bytes = unsafe {
            std::slice::from_raw_parts_mut(
                target.as_mut_ptr() as *mut u8,
                std::mem::size_of_val(target.as_slice()),
            )
        };
        let memh = ucx_sys::memh::MemHandle::map_slice(transport.context(), target_bytes, 0)
            .expect("registered atomic target");
        let packed = ucx_sys::rma::RemoteKey::pack(transport.context(), memh.mem_handle())
            .expect("packed target rkey");
        let endpoint = transport.loopback_endpoint().expect("loopback endpoint");
        let rkey = ucx_sys::rma::RemoteKey::unpack(&endpoint, &packed).expect("unpacked rkey");
        let params = RequestParamBuilder::new()
            .datatype(ucx_sys::dt::dt_make_contig(std::mem::size_of::<u64>()))
            .build();
        let address = target.as_mut_ptr() as u64;

        complete_request(
            &transport,
            endpoint
                .amo_swap64(10, address, &rkey, &params)
                .expect("swap"),
        )
        .expect("swap completion");
        complete_request(
            &transport,
            endpoint.amo_add64(5, address, &rkey, &params).expect("add"),
        )
        .expect("add completion");
        complete_request(
            &transport,
            endpoint
                .amo_xor64(0b11, address, &rkey, &params)
                .expect("xor"),
        )
        .expect("xor completion");
        complete_request(
            &transport,
            endpoint
                .amo_and64(!0b10, address, &rkey, &params)
                .expect("and"),
        )
        .expect("and completion");
        complete_request(
            &transport,
            endpoint
                .amo_or64(0b100, address, &rkey, &params)
                .expect("or"),
        )
        .expect("or completion");
        complete_request(
            &transport,
            endpoint
                .amo_cswap64(12, 20, address, &rkey, &params)
                .expect("compare-and-swap"),
        )
        .expect("compare-and-swap completion");
        assert_eq!(target[0], 20);

        let old = transport.with_worker(|worker| {
            let mut reply = u64::MAX;
            let request = endpoint
                .amo_fadd64(worker, 2, address, &rkey, &mut reply)
                .expect("fetch-add");
            complete_fetch(worker, request).expect("fetch-add completion");
            reply
        });
        assert_eq!(old, 20);
        assert_eq!(target[0], 22);

        let old = transport.with_worker(|worker| {
            let mut reply = u64::MAX;
            let request = endpoint
                .amo_fxor64(worker, 0b11, address, &rkey, &mut reply)
                .expect("fetch-xor");
            complete_fetch(worker, request).expect("fetch-xor completion");
            reply
        });
        assert_eq!(old, 22);
        assert_eq!(target[0], 21);

        let old = transport.with_worker(|worker| {
            let mut reply = u64::MAX;
            let request = endpoint
                .amo_fswap64(worker, 7, address, &rkey, &mut reply)
                .expect("fetch-swap");
            complete_fetch(worker, request).expect("fetch-swap completion");
            reply
        });
        assert_eq!(old, 21);
        assert_eq!(target[0], 7);

        let old = transport.with_worker(|worker| {
            let mut reply = u64::MAX;
            let request = endpoint
                .amo_fcswap64(worker, 7, 9, address, &rkey, &mut reply)
                .expect("fetch-compare-and-swap");
            complete_fetch(worker, request).expect("fetch-compare-and-swap completion");
            reply
        });
        assert_eq!(old, 7);
        assert_eq!(target[0], 9);
    }

    #[test]
    fn atomic_value_dispatch_preserves_signed_bit_patterns_without_ucx() {
        assert_eq!(<i32 as AtomicValue>::from_bits(u32::MAX), -1);
        assert_eq!(<i64 as AtomicValue>::from_bits(u64::MAX), -1);
        assert_eq!(<i32 as AtomicValue>::bits(-1), u32::MAX);
        assert_eq!(<i64 as AtomicValue>::bits(-1), u64::MAX);
        assert_eq!(<u32 as AtomicValue>::SIZE, std::mem::size_of::<u32>());
        assert_eq!(<u64 as AtomicValue>::SIZE, std::mem::size_of::<u64>());
    }
}
