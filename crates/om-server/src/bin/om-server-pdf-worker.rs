//! Reads one PDF request for `om-server` in a process of its own, its
//! allocations capped (`om_calc::functions::pdf_worker`, #566).
//! `om-server` runs it from beside its own executable.
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
