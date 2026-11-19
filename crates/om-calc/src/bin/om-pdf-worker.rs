//! Reads one PDF request in a process of its own, its allocations capped
//! (`om_calc::functions::pdf_worker`): this crate's worker, for its tests
//! and for hosts without their own.
use std::alloc::System;
use std::process::ExitCode;

use cap::Cap;

#[global_allocator]
static ALLOCATOR: Cap<System> = Cap::new(System, usize::MAX);

fn main() -> ExitCode {
    om_calc::functions::pdf_worker::serve(|budget| {
        ALLOCATOR
            .set_limit(ALLOCATOR.allocated().saturating_add(budget))
            .is_ok()
    })
}
