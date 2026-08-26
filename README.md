# openshmem-rs

An OpenSHMEM-style PGAS library for Rust, built on the SedahsDev `pmix-rs`,
`ucx-rs`, and optional `ucc-rs` bindings.

## Native libraries

The crate requires native PMIx (>= 5.0 for the base API) and UCX. Set
`PMIX_PREFIX` to the PMIx installation, or leave it unset to use `pkg-config`,
and make UCX discoverable through `pkg-config` (or the binding's normal build
configuration). For a custom PMIx installation:

```bash
export PMIX_PREFIX=/path/to/openpmix
export LD_LIBRARY_PATH="$PMIX_PREFIX/lib:${LD_LIBRARY_PATH:-}"
```

Collectives are optional: enable `--features collectives` when UCC 1.8.0 is
installed and set `UCC_INCLUDE_DIR` and `UCC_LIB_DIR` (or set `UCC_PREFIX`). The
`ucc` dependency is feature-gated because there is no system `pkg-config` UCC
entry.

Blocking collectives wait before returning. The feature-gated `coll::*_nb`
variants return a `CollectiveRequest`; call `test()` to poll or `wait()` to
drive UCC progress to completion. UCX tag-matching fallback collectives are
not included yet and remain future work.

The CI checks compile and test the library without requiring a PMIx DVM or
daemon. DVM-backed integration tests require a PRRTE installation and its
corresponding `PATH` and `LD_LIBRARY_PATH` exports.

## Examples

Both examples require a live PMIx/PRRTE DVM and are intended for multi-PE runs.
See [docs/RDMA-RUNNING.md](docs/RDMA-RUNNING.md) for the environment checklist.

```bash
# Two-PE symmetric RMA put/get demonstration
prterun -np 2 -x PMIX_SIZE=2 cargo run --example ping_put_get

# N-PE UCC barrier demonstration
prterun -np 4 -x PMIX_SIZE=4 cargo run --features collectives --example barrier
```

The examples deliberately fail with a clear message when launched with the
wrong PE count or without the required `collectives` feature. Build them
without a DVM using `cargo check --examples`; use the collectives feature for
`barrier`'s full implementation.

See [docs/DESIGN.md](docs/DESIGN.md) for architecture and the implementation roadmap. For the manual multi-PE procedure on RDMA-capable hardware, see [docs/RDMA-RUNNING.md](docs/RDMA-RUNNING.md).
