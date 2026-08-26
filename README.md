# openshmem-rs

An OpenSHMEM-style PGAS library for Rust, built on the SedahsDev `pmix-rs`,
`ucx-rs`, and optional `ucc-rs` bindings.

## Native libraries

The crate requires native PMIx >= 6.1 and UCX. Set `PMIX_PREFIX` to the PMIx
installation and make UCX discoverable through `pkg-config` (or the binding's
normal build configuration). For the reference environment:

```bash
export PMIX_PREFIX=/home/bzf/pmix-env/openpmix-6.1.0
export LD_LIBRARY_PATH=/home/bzf/pmix-env/openpmix-6.1.0/lib:/home/bzf/pmix-env/deps/libevent-2.1.12-stable/lib:/home/bzf/pmix-env/deps/hwloc-2.9.2/lib
```

Collectives are optional: enable `--features collectives` when UCC 1.8.0 is
installed and set `UCC_INCLUDE_DIR` and `UCC_LIB_DIR` (the reference prefix is
`/home/bzf`). The `ucc` dependency is feature-gated because there is no system
`pkg-config` UCC entry.

The CI checks compile and test the library without requiring a PMIx DVM or
daemon. DVM-backed integration tests require a PRRTE installation and its
corresponding `PATH` and `LD_LIBRARY_PATH` exports.

See [docs/DESIGN.md](docs/DESIGN.md) for architecture and the implementation roadmap.
