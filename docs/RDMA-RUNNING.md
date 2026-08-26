# Running the multi-PE suite on RDMA hardware

This document describes the manual validation path for the ignored PMIx/PRRTE,
UCX RMA, atomic, and UCC tests. It is intentionally host-specific where the
current development installation is referenced; replace those paths with the
corresponding installation prefixes on another machine.

The current development host has no usable RDMA transport. In particular, UCX
can report `UCS_ERR_UNREACHABLE` for cross-process RMA. The commands below are
therefore a runbook for an RDMA-capable system such as the planned DGX Spark,
not a claim that the local host can execute the multi-PE tests.

## Build and runtime environment

Build the default targets with PMIx 6.1.0 available. On the current host:

```bash
export PMIX_PREFIX=/home/bzf/pmix-env/openpmix-6.1.0
export LD_LIBRARY_PATH="$PMIX_PREFIX/lib:/home/bzf/pmix-env/deps/libevent-2.1.12-stable/lib:/home/bzf/pmix-env/deps/hwloc-2.9.2/lib${LD_LIBRARY_PATH:+:$LD_LIBRARY_PATH}"
```

On another host, substitute its PMIx, libevent, and hwloc prefixes. Do not copy
these paths blindly: the PMIx and dependency versions must be ABI-compatible
with the `pmix` binding and with PRRTE.

For the current PRRTE scratch installation, add the launcher to `PATH` and its
libraries to the runtime search path:

```bash
export PRRTE_PREFIX=/home/bzf/projects/prrte/scratch/install
export PATH="$PRRTE_PREFIX/bin:$PATH"
export LD_LIBRARY_PATH="$PRRTE_PREFIX/lib:$LD_LIBRARY_PATH"
```

Build the binaries before launching them. The collectives feature additionally
requires UCC; the normal non-collective build does not.

```bash
cargo test --no-run
cargo test --no-run --features collectives
```

The test binary path is normally under `target/debug/`; use `target/<profile>/`
for a release or other explicitly selected profile.

## Preconditions

Before launching a multi-PE test, verify each item:

- [ ] The PRRTE daemon/DVM is running and its URI file exists. With the current
      user installation, check `/run/user/1000/prte/uri` and, if applicable,
      `systemctl --user status prte.service`.
- [ ] The URI file is readable by the launching user and refers to the DVM that
      will accept the test processes. A stale URI file is not evidence that a
      live server is reachable.
- [ ] PMIx 6.1.0 is active (`PMIX_PREFIX` and `LD_LIBRARY_PATH` point to the
      matching installation), and the PMIx server is reachable from each PE.
- [ ] `prterun` resolves to the intended installation (`command -v prterun`).
- [ ] UCX exposes an RDMA transport. Run `ucx_info -d` and confirm that an
      `ib/rc` or `rc_verbs` device/transport is listed. Seeing only `self`,
      `cma`, `sysv`, `posix`, or `tcp` is not sufficient for production
      cross-process RMA/AMO validation.
- [ ] For collectives, UCC is installed and discoverable by the build and by
      every launched process.

If a precondition fails, fix the environment first. Do not remove `#[ignore]`
or turn a runtime failure into a passing test with an early return.

## Launching a test binary

Run one test binary at a time and serialize its test threads. `N` must match the
number of launched PEs and the exported PMIx size:

```bash
prterun -np N -x PMIX_SIZE=N ./target/<profile>/<test-bin> --ignored --test-threads=1
```

Examples (use the actual binary names printed by `cargo test --no-run`):

```bash
prterun -np 2 -x PMIX_SIZE=2 ./target/debug/openshmem-<test-bin> --ignored --test-threads=1
prterun -np 2 -x PMIX_SIZE=2 ./target/debug/collectives --ignored --test-threads=1
```

The second form is illustrative: Rust test binary names can change with the
crate's test layout. Discover them with `cargo test --no-run --message-format
short` or `find target -type f -perm -111` rather than assuming a name.

Run the ignored tests selectively when diagnosing a failure, for example with
`<test-bin> <test-name> --ignored --exact --test-threads=1`. Keep the full
`--ignored` invocation as the final smoke test so every documented gate is
exercised.

## UCX transport expectations

Transport selection determines which claims a run can validate:

- `self` and `cma` are loopback-only transports. They are useful for local
  single-process checks, but cannot validate cross-process RMA.
- `tcp` can provide cross-process zcopy RMA, but it provides no AMO support and
  no self-transport atomics. Consequently it is not a full production
  validation path for this crate's atomic API. The self-transport atomic
  limitation is tracked by issue #20.
- `ib/rc` and `rc_verbs` are the expected RDMA transports for full production
  validation, including cross-process RMA and AMO operations. Confirm the
  selected UCX configuration actually uses one of them; merely having the
  component installed is not enough.

## Ignored-test audit and un-gating matrix

The repository audit command is:

```bash
grep -rn "#\[ignore" src/ tests/
```

At the time this document was written it reports five ignored tests. All remain
in-tree and are skipped by default CI/local test runs. The gate is a test
configuration choice: on a correctly configured DVM, run the same binary with
`--ignored`; no source edit or test rewrite is required.

| Location and test | Gate reason | Expected behavior on RDMA hardware |
| --- | --- | --- |
| `src/bootstrap.rs::tests::dvm_exchanges_worker_heap_metadata` | Requires a DVM-launched process and PMIx KVS exchange. | Un-gates with a live multi-PE PRRTE/PMIx launch; every PE publishes and retrieves worker address, packed heap rkey, and heap base, and the peer map has one entry per PE. RDMA is required for the resulting peer UCX connections. |
| `src/init.rs::tests::dvm_reports_rank_and_size` | Requires a live PMIx server/PRRTE DVM. | Un-gates with the DVM launch; each PE initializes, observes its PMIx rank and the common PE count, checks rank is in range, then finalizes. |
| `src/rma.rs::pod_tests::loopback_integer_atomics_use_real_completion_and_fetch_replies` | The test deliberately creates a one-PE loopback endpoint; UCX self transport cannot execute atomics (no AM handler). | **Does not un-gate merely because RDMA hardware is present.** It still uses a self endpoint, and the limitation from #20 remains. The test should stay ignored until it is deliberately converted to a multi-PE/`ib/rc` fixture; that would be a separate code change, not a configuration flip. |
| `src/coll.rs::tests::execution_requires_dvm` | Requires a DVM and UCC. | Un-gates with `--features collectives`, UCC available to all PEs, and the DVM launch; blocking and non-blocking barrier, broadcast, reduce, and collect operations complete, and collect returns one value per PE. |
| `tests/collectives.rs::prterun_collectives_smoke` | Requires a DVM and UCC. | Un-gates with the same feature/build/runtime prerequisites; PMIx initializes, a UCC barrier completes across the launched PEs, and finalization succeeds. |

Thus four ignored tests are configuration-un-gated on a suitable DVM/RDMA
setup; the loopback atomic test is explicitly documented as the one exception.
This distinction prevents a hardware run from falsely claiming to validate
self-transport AMOs.

## Troubleshooting

- `UCS_ERR_UNREACHABLE` during a peer operation: inspect `ucx_info -d`, network
  reachability, and UCX transport selection. The local host's loopback/tcp-only
  transports cannot substitute for `ib/rc`/`rc_verbs` production validation.
- PMIx initialization or rank lookup failures: check the URI file, DVM
  lifetime, matching PMIx/PRRTE libraries, and that `PMIX_SIZE=N` equals
  `-np N`.
- A collective cannot initialize: verify `--features collectives`, UCC headers
  and libraries at build time, and UCC/UCX/PMIx libraries in every launched
  process's `LD_LIBRARY_PATH`.
- A test hangs: stop the DVM and launched processes, remove only a stale URI
  file if no DVM is running, then restart the DVM. Keep
  `--test-threads=1`; PMIx-dependent tests are not intended to run concurrently
  in one test process.

Do not run `--ignored` as a substitute for the precondition checklist. An
ignored test that fails because its server or transport is absent is an
expected environment failure, not evidence that the test should be changed.

## Issue checklist

- [x] Documented the current PMIx/PRRTE environment and generic substitutions.
- [x] Documented the DVM/URI/PMIx/UCX preconditions.
- [x] Audited every `#[ignore]` in `src/` and `tests/`.
- [x] Listed the four configuration-un-gated tests and the loopback atomic
      exception.
- [x] Kept all tests in-tree and preserved their default ignored status.
- [ ] Run the ignored suite on RDMA-capable hardware after the DGX Spark is
      available.

The final unchecked item is intentionally hardware-dependent.

## References

- Issue [#11](https://github.com/SedahsDev/openshmem-rs/issues/11)
- Issue [#20](https://github.com/SedahsDev/openshmem-rs/issues/20), self/tcp
  atomic limitation
- [OpenSHMEM design notes](DESIGN.md)

----

This runbook documents the current repository and host setup; update the
versioned paths and transport observations when the deployment changes.
