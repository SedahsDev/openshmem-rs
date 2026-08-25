# openshmem-rs

An OpenSHMEM-style **partitioned global address space (PGAS)** library for Rust,
modeled on the [OSSS-UCX](https://github.com/Sandia-OpenSHMEM/osss-ucx) reference
implementation and built on the SedahsDev Rust bindings:

| Layer | Binding crate | Role |
|-------|---------------|------|
| Process bootstrap | [`pmix`](../pmix-rs) | `init`/`finalize`, rank & universe-size discovery, key/value exchange (worker addr + rkey + heap base) |
| RMA data plane | [`ucx-sys`](../ucx-rs) | `put`/`get`, atomic ops, `fence`/`flush`/`quiet`, symmetric-memory registration & rkey packing |
| Collectives | [`ucc`](../ucc-rs) | `barrier`, `broadcast`, `collect`, `reduce`/`to_all` |

## Design

OpenSHMEM gives every process (PE) a **symmetric heap**: an address space where an
object allocated at rank `i` is addressable from every other rank at the same
virtual offset. The reference implementation splits this across three substrates,
and so do we:

- **Symmetric memory** is allocated with a Rust allocator and registered with UCX
  (`MemHandle::map`). The registration yields an rkey (`pack_rkey`) that each peer
  unpacks (`RemoteKey::unpack`) to form a remote-memory handle for that PE's heap.
- **Put/get/atomics** are issued on a UCX worker/endpoint pair to each peer PE
  (`rma_put`, `rma_get`, `amo_add64`, …), giving us `shmem_*_put`, `shmem_*_get`,
  and the `shmem_atomic_*` family.
- **Ordering/completion** map to UCX primitives: `shmem_fence` → `ucp_ep_fence_nbx`
  (ordering), `shmem_quiet` → `ucp_ep_flush_nbx` (completion). See the ucx-rs
  checklist for the exact semantics.
- **Bootstrap** uses PMIx the way OSSS-UCX's `pmix_client.c` does: `PMIx_Put` the
  worker address and rkey, `PMIx_Commit`, `PMIx_Fence`, then `PMIx_Get` each peer's
  handle before any RMA is issued.
- **Collectives** (feature-gated) delegate to UCC team + collective builders.

## Status

Early design. See the issue tracker for the phased implementation roadmap.

## Building

Requires the sibling crates checked out next to this repo (path deps `../pmix-rs`,
`../ucx-rs`, `../ucc-rs`), plus native PMIx + UCX (and UCC for collectives). See
`hpc-workspace-map` / `hpc-binding-checklist` skills for native-lib discovery and
the UCX transport capabilities table.
