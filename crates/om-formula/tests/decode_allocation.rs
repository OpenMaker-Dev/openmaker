//! A Polish stream's headers declare operand counts of up to 1000 (3.1).
//! Decoding allocates for the operands the stream holds, not for the counts
//! it declares, so a short hostile stream cannot make it allocate much.
//!
//! The counting allocator is this binary's, and this is its only test.

use std::alloc::System;

use om_formula::{polish, DecodeErrorKind};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

fn hex(s: &str) -> Vec<u8> {
    let s: String = s.split_whitespace().collect();
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
        .collect()
}

#[test]
fn declared_operand_counts_reserve_nothing() {
    // Each header declares 1000 operands; nested past the depth limit, the
    // first operand of each is the next header.
    for (what, header) in [
        ("Case", "4c 03e8"),
        ("List", "19 03e8"),
        ("Choose", "4d 03e8"),
        ("Let", "df 03e8"),
        ("While", "f800 03e8"),
        ("Evaluate", "dc 03e8 0000"),
        ("counted call", "87 03e8"),
        ("custom function call", "1b 03e8 14 02 01 04"),
        ("plug-in call", "1d 03e8 15 54455354 0001"),
    ] {
        let stream = hex(header).repeat(polish::MAX_DEPTH + 1);
        // The opcode catalog is loaded on first use.
        let _ = polish::decode(&stream);
        let region = Region::new(GLOBAL);
        let error = polish::decode(&stream).unwrap_err();
        let allocated = region.change().bytes_allocated;
        assert_eq!(
            error.kind,
            DecodeErrorKind::Malformed("nesting depth"),
            "{what}"
        );
        assert!(
            allocated < 16 * stream.len(),
            "{what}: {allocated} bytes allocated for a {}-byte stream",
            stream.len()
        );
    }
}
