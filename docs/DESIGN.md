# openshmem-rs — Design

An OpenSHMEM-style partitioned global address space (PGAS) library for Rust, modeled
on the OSSS-UCX reference implementation and built on the SedahsDev Rust bindings.

## Architecture

| Layer | Binding | Role |
|-------|---------|------|
| Bootstrap | `pmix` | `init`/`finalize`, PE rank & size, symmetric KVS (worker addr, rkey, heap base) |
| RMA data plane | `ucx-sys` | `put`/`get`, atomics, `fence`/`flush`, memory registration + rkey packing |
| Collectives | `ucc` (optional) | `barrier`, `broadcast`, `collect`, `reduce`/`to_all` |

The mapping mirrors how `osss-ucx/src/shmemc/ucx/*` wires the same three C libraries.

## OpenSHMEM API → Rust mapping

| OpenSHMEM | Rust (this crate) | Underlying |
|-----------|-------------------|------------|
| `shmem_init` / `shmem_finalize` | `init::init()` / `init::finalize()` | `pmix::init` / finalize |
| `shmem_my_pe` / `shmem_n_pes` | `init::my_pe()` / `init::n_pes()` | PMIx rank / universe size |
| `shmem_malloc` / `shmem_free` | `symheap::SymAlloc::{malloc,free}` | UCX `MemHandle::map` / unmap |
| `shmem_<t>_put` / `shmem_<t>_get` | `rma::put<T>` / `rma::get<T>` | UCX `rma_put` / `rma_get` |
| `shmem_atomic_*` / fetch ops | `rma::atomic_*` | UCX `amo_*64` (+ `reply_buffer`) |
| `shmem_fence` / `shmem_quiet` | `rma::fence()` / `rma::quiet()` | `ucp_ep_fence_nbx` / `ucp_ep_flush_nbx` |
| `shmem_barrier*` / collectives | `coll::barrier` / `coll::*` | UCC team + collective builders |

## Conventions

- **Safe-Rust-only at the crate boundary.** No `unsafe` in app-facing code; all FFI
  `unsafe` lives in the binding crates.
- **No backward compat.** Simplest correct implementation first; grow in layers.
- **Path deps** to sibling crates `../pmix-rs`, `../ucx-rs`, `../ucc-rs`.
- `ucc` is **feature-gated** (no system UCC is installed). `cargo check --lib` must
  stay green without it.

## Bootstrap sequence (per PE)

1. `pmix::init(None)` → rank, nspace, universe size.
2. Create UCX worker; `MemHandle::map` the symmetric heap.
3. `pmix::put_value` worker addr + packed rkey + heap base; `commit`; `fence`.
4. `pmix::get_value` each peer PE → unpack rkey → `RemoteKey`, build per-PE endpoint.
5. RMA/atomics now addressable on the peer's symmetric heap.

## Native libs

System PMIx 5.0.7, system UCX 1.19.0, **no system UCC**. PMIx daemon/DVM tests need
the PRRTE scratch install env (`LD_LIBRARY_PATH` / `PATH`). RMA unit tests can run on
single-PE loopback (`self`/`cma`) without a DVM.
