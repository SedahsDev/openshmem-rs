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
| `shmem_malloc` / `shmem_free` | `symheap::SymAlloc::{malloc,free}` | Vendored jemalloc pool + UCX `MemHandle::map` / unmap |
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

`bootstrap::handshake` implements steps 3–4. It publishes the worker address,
framed heap rkey, and heap base, then performs `commit` and a PMIx `fence`
before any peer `get`. Each peer address creates a UCX endpoint and its complete
framed rkey is passed to `RemoteKey::unpack`; the framed bytes and base are also
retained in `PeerRkeys`.

## Symmetric addressing (`SymPtr`)

Allocations from the symmetric heap are wrapped in a private `usize` ([`SymPtr`]
in `src/symheap.rs`) rather than exposed as raw pointers, because the same virtual
offset maps to different physical addresses on different PEs. The mapping follows
OSSS-UCX's `comms.c` `translate_address` / `get_remote_key_and_addr` flow:

- `SymPtr::offset_from(local_heap_base) -> u64` — the allocation's virtual offset.
- `SymPtr::to_remote_addr(local_heap_base, peer_heap_base) -> u64` — the UCX
  remote pointer for a target PE, computed as `peer_heap_base + (local_address -
  local_heap_base)` so every PE preserves the same virtual offset.
- `rma::put/get` pass this `u64` plus the peer's unpacked `RemoteKey` to UCX.

Application code cannot build a `SymPtr` from a raw pointer; only `SymAlloc`
produces them, and only the crate converts them to remote addresses.

## Native libs

System PMIx 5.0.7, system UCX 1.19.0, **no system UCC**. PMIx daemon/DVM tests need
the PRRTE scratch install env (`LD_LIBRARY_PATH` / `PATH`). RMA unit tests can run on
single-PE loopback (`self`/`cma`) without a DVM.
