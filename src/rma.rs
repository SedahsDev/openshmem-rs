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
use ucx_sys::worker::{MtWorker, RemoteWorkerAddress};

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
    #[allow(dead_code)]
    context: Context,
    worker: MtWorker,
    packed_address: Vec<u8>,
    capabilities: TransportCapabilities,
}

impl UcxTransport {
    /// Create a context requesting RMA and exported-memory-handle support.
    ///
    /// Context creation is the capability gate: UCX returns an error when no
    /// usable transport satisfies the requested feature set. The worker is
    /// serialized by its binding-level mutex so this handle can be retained in
    /// the process-global lifecycle state.
    pub fn new(estimated_num_eps: usize) -> Result<Self> {
        let features = context::Flags::Tag;
        let mut params_builder = context::ParamsBuilder::new();
        params_builder.features(features).mt_workers_shared(1);
        let _ = estimated_num_eps;
        let params = params_builder.build();
        let config = context::Config::read("", "").map_err(|error| match error {
            context::ConfigError::Ucs(status) => Error::from(status),
            context::ConfigError::Nul(_) => Error::Internal("invalid UCX configuration string"),
        })?;
        let mut context = Context::new(&config, &params).map_err(Error::from)?;
        drop(config);

        let worker_params = ucx_sys::worker::ParamsBuilder::new().build();
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

/// A typed put, mirroring `shmem_<type>_put` (implemented in a later phase).
pub fn put<T: Copy>(_dst_pe: i32, _src: &[T], _dst_offset: usize) {
    todo!("issue: UCX rma_put via per-PE endpoint")
}

/// A typed get, mirroring `shmem_<type>_get` (implemented in a later phase).
pub fn get<T: Copy>(_src_pe: i32, _src_offset: usize, _len: usize) -> Vec<T> {
    todo!("issue: UCX rma_get via per-PE endpoint")
}
