//! Run with a live PRRTE/PMIx DVM, for example:
//! `prterun -np 2 -x PMIX_SIZE=2 cargo run --example ping_put_get`
//! See docs/RDMA-RUNNING.md for the required DVM and UCX setup.

use openshmem::{init, rma};

const TARGET_PE: usize = 1;
const VALUE_OFFSET: usize = 0;
const VALUE: u64 = 0x5049_4e47_2d52_4d41;

fn run() -> openshmem::error::Result<()> {
    init::init()?;
    let rank = init::my_pe()? as usize;
    let size = init::n_pes()?;
    if size != 2 {
        return Err(openshmem::error::Error::Usage(
            "ping_put_get requires exactly two PEs",
        ));
    }

    let buffer = init::malloc(std::mem::size_of::<u64>())?;
    let offset = buffer.offset_from(init::heap_base()?);
    if rank == 0 {
        rma::put(TARGET_PE, &[VALUE], offset + VALUE_OFFSET)?;
        rma::fence()?;
        rma::quiet_pe(TARGET_PE)?;
        println!("PE 0 put {VALUE:#x} to PE 1");
    } else {
        let received = rma::get::<u64>(0, offset + VALUE_OFFSET, 1)?;
        if received.as_slice() != [VALUE] {
            return Err(openshmem::error::Error::Internal(
                "PE 1 received an unexpected value",
            ));
        }
        println!("PE 1 got and verified {VALUE:#x}");
    }
    init::free(buffer)?;
    Ok(())
}

fn main() {
    let result = run();
    let finalize_result = init::finalize();
    if let Err(error) = result {
        eprintln!("ping_put_get failed: {error}");
        std::process::exit(1);
    }
    if let Err(error) = finalize_result {
        eprintln!("finalize failed: {error}");
        std::process::exit(1);
    }
}
