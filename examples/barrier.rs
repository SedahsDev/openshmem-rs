//! Run with a live PRRTE/PMIx DVM, for example:
//! `prterun -np 4 -x PMIX_SIZE=4 cargo run --features collectives --example barrier`
//! See docs/RDMA-RUNNING.md for the required DVM and UCC setup.

#[cfg(feature = "collectives")]
fn main() {
    if let Err(error) = run() {
        eprintln!("barrier failed: {error}");
        std::process::exit(1);
    }
}

#[cfg(feature = "collectives")]
fn run() -> openshmem::error::Result<()> {
    openshmem::init::init()?;
    let rank = openshmem::init::my_pe()?;
    let size = openshmem::init::n_pes()?;
    println!("PE {rank}/{size} entering barrier");
    let barrier_result = openshmem::coll::barrier();
    let finalize_result = openshmem::init::finalize();
    barrier_result?;
    finalize_result?;
    println!("PE {rank}/{size} passed barrier");
    Ok(())
}

#[cfg(not(feature = "collectives"))]
fn main() {
    eprintln!("barrier example requires the `collectives` feature");
    std::process::exit(1);
}
