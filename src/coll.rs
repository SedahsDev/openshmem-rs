//! UCC-backed blocking collectives, selected at compile time by `collectives`.
//! UCC team creation uses PMIx for the required out-of-band allgather.
#![cfg(feature = "collectives")]
#![allow(unsafe_op_in_unsafe_fn)]

use crate::{
    error::{Error, Result},
    rma::Pod,
};
use pmix::{get_value, put_value, PmixScope};
use std::{ffi::c_void, ptr};
use ucc::{
    bindings::{ucc_oob_coll_t, ucc_status_t_UCC_ERR_NO_MESSAGE, ucc_status_t_UCC_OK},
    collective::{CollectiveBuilder, UccCollective, UccCollectiveType, UccReductionOp},
    context::UccContext,
    lib_init::UccLib,
    team::{UccTeam, UccTeamParams},
};

const OOB_KEY: &[u8] = b"openshmem.ucc.oob\0";

struct OobState {
    client: pmix::PmixClient,
    #[allow(dead_code)]
    rank: u32,
    size: u32,
}
struct OobRequest {
    status: pmix::ffi::pmix_status_t,
}

unsafe extern "C" fn allgather(
    src: *mut c_void,
    dst: *mut c_void,
    len: usize,
    info: *mut c_void,
    request: *mut *mut c_void,
) -> pmix::ffi::pmix_status_t {
    if src.is_null() || dst.is_null() || info.is_null() || request.is_null() || len == 0 {
        return -27;
    }
    let state = &*(info as *const OobState);
    let bytes = std::slice::from_raw_parts(src.cast::<u8>(), len);
    let result = (|| {
        let key = std::ffi::CStr::from_bytes_with_nul(OOB_KEY).ok()?;
        let mut value = pmix::PmixValueBuilder::new()
            .byte_object(bytes)
            .ok()?
            .build()
            .ok()?;
        put_value(PmixScope::Global.to_raw(), key, &mut value).ok()?;
        pmix::commit().ok()?;
        let wildcard = state.client.proc_with_nspace(pmix::RANK_WILDCARD).ok()?;
        pmix::fence(&wildcard, None).ok()?;
        for peer in 0..state.size {
            let proc = state.client.proc_with_nspace(peer).ok()?;
            let value = get_value(&proc, OOB_KEY, None).ok()?;
            let peer_bytes = value.bytes_copy();
            if peer_bytes.len() != len {
                return None;
            }
            ptr::copy_nonoverlapping(
                peer_bytes.as_ptr(),
                (dst.cast::<u8>()).add(peer as usize * len),
                len,
            );
        }
        Some(0)
    })()
    .unwrap_or(-1);
    *request = Box::into_raw(Box::new(OobRequest { status: result })).cast();
    result
}
unsafe extern "C" fn req_test(request: *mut c_void) -> pmix::ffi::pmix_status_t {
    if request.is_null() {
        -27
    } else {
        (*(request as *const OobRequest)).status
    }
}
unsafe extern "C" fn req_free(request: *mut c_void) -> pmix::ffi::pmix_status_t {
    if request.is_null() {
        -27
    } else {
        drop(Box::from_raw(request as *mut OobRequest));
        0
    }
}

pub(crate) struct Runtime {
    pub(crate) team: UccTeam,
    pub(crate) context: UccContext,
    _lib: UccLib,
    _oob: Box<OobState>,
}
// SAFETY: Every collective operation enters `with_state`, which holds the
// lifecycle mutex for the entire operation and therefore serializes UCC ops.
unsafe impl Send for Runtime {}

impl Runtime {
    pub(crate) fn new(client: pmix::PmixClient, rank: u32, size: usize) -> Result<Self> {
        let size = u32::try_from(size).map_err(|_| Error::Usage("job size exceeds UCC limits"))?;
        let oob = Box::new(OobState { client, rank, size });
        let lib = UccLib::init().map_err(map_ucc)?;
        let context = UccContext::new(lib.clone()).map_err(map_ucc)?;
        let mut params = UccTeamParams::default();
        params
            .with_team_size(size as u64)
            .with_oob(ucc_oob(&oob, rank, size));
        let team = UccTeam::with_params(context.clone(), params).map_err(map_ucc)?;
        Ok(Self {
            team,
            context,
            _lib: lib,
            _oob: oob,
        })
    }
}
fn ucc_oob(oob: &OobState, rank: u32, size: u32) -> ucc_oob_coll_t {
    ucc_oob_coll_t {
        allgather: Some(allgather),
        req_test: Some(req_test),
        req_free: Some(req_free),
        coll_info: oob as *const _ as *mut c_void,
        n_oob_eps: size,
        oob_ep: rank,
    }
}
fn map_ucc(status: ucc::UccStatus) -> Error {
    Error::Ucc(status)
}
fn wait(context: &UccContext, mut op: UccCollective) -> Result<()> {
    loop {
        let status = unsafe { (*op.request()).status };
        if status == ucc_status_t_UCC_OK {
            return op.finalize().map_err(map_ucc);
        }
        if status != ucc_status_t_UCC_ERR_NO_MESSAGE {
            return Err(map_ucc(ucc::UccStatus::from_raw(status)));
        }
        context.progress();
    }
}
fn dtype<T: Pod>() -> u32 {
    T::ucc_datatype().as_raw() as u32
}
fn encode<T: Pod>(values: &[T]) -> Result<Vec<u8>> {
    let capacity = values
        .len()
        .checked_mul(T::SIZE)
        .ok_or(Error::Usage("collective buffer size overflow"))?;
    let mut bytes = Vec::with_capacity(capacity);
    for &value in values {
        value.encode(&mut bytes);
    }
    Ok(bytes)
}
fn decode<T: Pod>(bytes: &[u8], values: &mut [T]) -> Result<()> {
    if bytes.len()
        != values
            .len()
            .checked_mul(T::SIZE)
            .ok_or(Error::Usage("collective buffer size overflow"))?
    {
        return Err(Error::Usage("collective buffer has an invalid byte length"));
    }
    for (value, chunk) in values.iter_mut().zip(bytes.chunks_exact(T::SIZE)) {
        *value = T::decode(chunk)?;
    }
    Ok(())
}

/// Synchronize all PEs in the process-global UCC team.
pub fn barrier() -> Result<()> {
    crate::init::with_state(|state| {
        let runtime = state
            .collectives
            .as_ref()
            .ok_or(Error::Internal("collectives are not initialized"))?;
        let mut stub = [0u8];
        let op = CollectiveBuilder::new(UccCollectiveType::Barrier)
            .with_inplace(&mut stub)
            .with_count(1)
            .init(&runtime.team)
            .map_err(map_ucc)?;
        op.post().map_err(map_ucc)?;
        wait(&runtime.context, op)
    })
}
/// Broadcast a typed buffer from `root`.
pub fn broadcast<T: Pod>(root: usize, values: &mut [T]) -> Result<()> {
    if values.is_empty() {
        return Err(Error::Usage("collective buffer must be non-empty"));
    }
    let mut bytes = encode(values)?;
    crate::init::with_state(|state| {
        let runtime = state
            .collectives
            .as_ref()
            .ok_or(Error::Internal("collectives are not initialized"))?;
        if root >= runtime.team.size().map_err(map_ucc)? as usize {
            return Err(Error::Usage("broadcast root is outside the UCC team"));
        }
        let op = CollectiveBuilder::new(UccCollectiveType::Broadcast)
            .with_inplace(&mut bytes)
            .with_count(values.len() as u64)
            .with_dtype(dtype::<T>())
            .with_root(root as u64)
            .init(&runtime.team)
            .map_err(map_ucc)?;
        op.post().map_err(map_ucc)?;
        wait(&runtime.context, op)
    })?;
    decode(&bytes, values)
}
/// Gather equal-sized typed contributions in rank order.
pub fn collect<T: Pod>(values: &[T]) -> Result<Vec<T>> {
    if values.is_empty() {
        return Err(Error::Usage("collective contribution must be non-empty"));
    }
    crate::init::with_state(|state| {
        let runtime = state
            .collectives
            .as_ref()
            .ok_or(Error::Internal("collectives are not initialized"))?;
        let send = encode(values)?;
        let ranks = runtime.team.size().map_err(map_ucc)? as usize;
        let mut recv = vec![
            0u8;
            send.len()
                .checked_mul(ranks)
                .ok_or(Error::Usage("collective receive buffer overflow"))?
        ];
        let op = CollectiveBuilder::new(UccCollectiveType::Allgather)
            .with_src(&send)
            .with_dst(&mut recv)
            .with_count(values.len() as u64)
            .with_dtype(dtype::<T>())
            .init(&runtime.team)
            .map_err(map_ucc)?;
        op.post().map_err(map_ucc)?;
        wait(&runtime.context, op)?;
        recv.chunks_exact(send.len())
            .map(|chunk| chunk.chunks_exact(T::SIZE).map(T::decode).collect())
            .collect::<Result<Vec<Vec<T>>>>()
            .map(|parts| parts.into_iter().flatten().collect())
    })
}
/// Allreduce in place using the supported subset of OpenSHMEM reductions.
///
/// The logical product and boolean reductions (`Prod`, `Land`, `Lor`, and
/// `Lxor`) are not currently exposed; integer bitwise reductions are supported
/// where applicable.
pub fn reduce<T: Pod>(op: UccReductionOp, values: &mut [T]) -> Result<()> {
    if !T::reduction_supported(op) {
        return Err(Error::Usage(
            "reduction operation is not supported for this POD type",
        ));
    }
    let mut bytes = encode(values)?;
    crate::init::with_state(|state| {
        let runtime = state
            .collectives
            .as_ref()
            .ok_or(Error::Internal("collectives are not initialized"))?;
        let operation = CollectiveBuilder::new(UccCollectiveType::Allreduce)
            .with_inplace(&mut bytes)
            .with_count(values.len() as u64)
            .with_dtype(dtype::<T>())
            .with_reduction_op(op)
            .init(&runtime.team)
            .map_err(map_ucc)?;
        operation.post().map_err(map_ucc)?;
        wait(&runtime.context, operation)
    })?;
    decode(&bytes, values)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn public_symbols_compile() {
        let _: fn() -> Result<()> = barrier;
        let _: fn(usize, &mut [u8]) -> Result<()> = broadcast;
        let _: fn(&[u8]) -> Result<Vec<u8>> = collect;
        let _: fn(UccReductionOp, &mut [u8]) -> Result<()> = reduce;
    }

    #[test]
    fn unknown_ucc_status_preserves_raw_code() {
        let error = map_ucc(ucc::UccStatus::Unknown(-777));
        assert!(matches!(error, Error::Ucc(ucc::UccStatus::Unknown(-777))));
    }
    #[test]
    #[ignore = "requires DVM + UCC"]
    fn execution_requires_dvm() {
        crate::init::init().unwrap();
        barrier().unwrap();
        crate::init::finalize().unwrap();
    }
}
