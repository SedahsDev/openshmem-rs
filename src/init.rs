//! PMIx-backed OpenSHMEM process lifecycle.
//!
//! A process must call [`init`] before [`my_pe`] or [`n_pes`], and should call
//! [`finalize`] exactly once when it is done. The latter is safe to call more
//! than once. This module explicitly calls `PmixClient::disconnect`: dropping a
//! `pmix-rs` client does not finalize the PMIx session.
//!
//! PMIx's FFI is serialized by the mutex protecting the process-global state.
//! A successful initialization stores the PMIx client and cached rank and job
//! size. Re-initialization after finalization is not supported by `pmix-rs`.

use std::sync::{Mutex, OnceLock};

use pmix::{get_value, PmixClient, RANK_WILDCARD};

use crate::bootstrap::{handshake, PeerConnection};
use crate::error::{Error, Result};
use crate::rma::UcxTransport;
use crate::symheap::SymHeap;

struct ShmemState {
    // Fields are dropped in declaration order. Drop peer UCX handles first,
    // then the heap, and finally the transport that owns their worker/context.
    #[allow(dead_code)]
    peers: std::collections::BTreeMap<u32, PeerConnection>,
    #[allow(dead_code)]
    heap: SymHeap,
    #[allow(dead_code)]
    transport: UcxTransport,
    client: PmixClient,
    rank: u32,
    size: usize,
    #[allow(dead_code)]
    peer_rkeys: crate::symheap::PeerRkeys,
}

static STATE: OnceLock<Mutex<Option<ShmemState>>> = OnceLock::new();

fn state() -> &'static Mutex<Option<ShmemState>> {
    STATE.get_or_init(|| Mutex::new(None))
}

fn lock_state() -> Result<std::sync::MutexGuard<'static, Option<ShmemState>>> {
    state()
        .lock()
        .map_err(|_| Error::Internal("lifecycle state lock poisoned"))
}

/// Initialize the OpenSHMEM process and cache its PE identity.
///
/// This is one logical initialization per process. Calling it while already
/// initialized returns [`Error::AlreadyInitialized`]. PMIx job size is read
/// from `PMIX_JOB_SIZE`; if that value is unavailable, `PMIX_SIZE` is used as a
/// compatibility fallback for PRRTE environments that do not publish it.
pub fn init() -> Result<()> {
    let mut state = lock_state()?;
    if state.is_some() {
        return Err(Error::AlreadyInitialized);
    }

    let client = PmixClient::connect_new(None).map_err(Error::from)?;
    let rank = client.require_rank();
    let transport = match UcxTransport::new(1) {
        Ok(transport) => transport,
        Err(error) => {
            let _ = client.disconnect(None);
            return Err(error);
        }
    };
    let heap = match SymHeap::new(&transport) {
        Ok(heap) => heap,
        Err(error) => {
            let _ = client.disconnect(None);
            return Err(error);
        }
    };
    let size = (|| {
        let wildcard = client.proc_with_nspace(RANK_WILDCARD).ok()?;
        get_value(&wildcard, pmix::JOB_SIZE, None)
            .ok()
            .map(|value| value.uint32() as usize)
    })()
    .or_else(|| {
        std::env::var("PMIX_SIZE")
            .ok()
            .and_then(|value| value.parse::<usize>().ok())
    });
    let Some(size) = size else {
        let _ = client.disconnect(None);
        return Err(Error::Internal(
            "PMIx job size unavailable and PMIX_SIZE is not set to a valid value",
        ));
    };

    let bootstrap = match handshake(&client, &transport, &heap, size) {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let _ = client.disconnect(None);
            return Err(error);
        }
    };

    let crate::bootstrap::Bootstrap { peer_rkeys, peers } = bootstrap;
    *state = Some(ShmemState {
        peers,
        heap,
        transport,
        client,
        rank,
        size,
        peer_rkeys,
    });
    Ok(())
}

/// Return this process's PE rank, mirroring `shmem_my_pe()`.
pub fn my_pe() -> Result<u32> {
    lock_state()?
        .as_ref()
        .map(|state| state.rank)
        .ok_or(Error::NotInitialized)
}

/// Return the number of PEs in the job, mirroring `shmem_n_pes()`.
pub fn n_pes() -> Result<usize> {
    lock_state()?
        .as_ref()
        .map(|state| state.size)
        .ok_or(Error::NotInitialized)
}

/// Finalize the PMIx session.
///
/// Finalization is idempotent. The stored client is explicitly disconnected
/// before the state is cleared because `pmix-rs` does not finalize on `Drop`.
pub fn finalize() -> Result<()> {
    let mut state = lock_state()?;
    if let Some(shmem_state) = state.take() {
        shmem_state.client.disconnect(None).map_err(|status| {
            Error::Pmix(pmix::PmixError::from_raw(status).unwrap_or(pmix::PmixError::Error))
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queries_fail_before_initialization() {
        assert!(matches!(my_pe(), Err(Error::NotInitialized)));
        assert!(matches!(n_pes(), Err(Error::NotInitialized)));
    }

    #[test]
    fn finalize_is_idempotent_without_a_pmix_server() {
        finalize().expect("finalize before init is a no-op");
        finalize().expect("repeated finalize is a no-op");
    }

    /// Run this test under a PMIx DVM (for example, `prterun -n 2 ...`).
    #[test]
    #[ignore = "requires a live PMIx server / PRRTE DVM"]
    fn dvm_reports_rank_and_size() {
        init().expect("PMIx init");
        assert!(my_pe().expect("rank") < n_pes().expect("size") as u32);
        assert!(n_pes().expect("size") >= 1);
        finalize().expect("PMIx finalize");
    }
}
