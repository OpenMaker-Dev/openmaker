//! Reads one PDF request for `om-gui` in a process of its own, its
//! allocations capped (`om_calc::functions::pdf_worker`, #566); with
//! `--svg`, draws one report SVG picture (`om_gui::report::svg_worker`).
//! `om-gui` runs it from beside its own executable.
use std::alloc::System;
use std::process::ExitCode;

use cap::Cap;

#[global_allocator]
static ALLOCATOR: Cap<System> = Cap::new(System, usize::MAX);

fn main() -> ExitCode {
    let limit = |budget: usize| {
        ALLOCATOR
            .set_limit(ALLOCATOR.allocated().saturating_add(budget))
            .is_ok()
    };
    if std::env::args_os().nth(1).as_deref() == Some(om_gui::report::svg_worker::ARG.as_ref()) {
        return om_gui::report::svg_worker::serve(limit);
    }
    om_data::calc::functions::pdf_worker::serve(limit)
}
