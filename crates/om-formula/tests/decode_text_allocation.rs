//! Text, symbol and variable operands declare their length in UTF-16 units
//! (calc-encoding.md 2.3), up to a 32-bit count. Decoding reserves for the
//! bytes the stream holds, not for the declared length, so a few bytes
//! cannot make it allocate gigabytes.
//!
//! The counting allocator is this binary's, and this is its only test.

use std::alloc::System;

use om_formula::{polish, DecodeErrorKind};
use stats_alloc::{Region, StatsAlloc, INSTRUMENTED_SYSTEM};

#[global_allocator]
static GLOBAL: &StatsAlloc<System> = &INSTRUMENTED_SYSTEM;

#[test]
fn declared_text_lengths_reserve_only_the_bytes_present() {
    for (what, opcode) in [("text", 0x13), ("symbol", 0x0b), ("variable", 0x1a)] {
        for count in [0x0010_0000u32, u32::MAX] {
            // The length header, then three bytes where `count` units belong.
            let mut stream = vec![opcode, 0xc4];
            stream.extend(count.to_be_bytes());
            stream.extend([0x1b, 0x1b, 0x1b]);
            // The opcode catalog is loaded on first use.
            let _ = polish::decode(&stream);
            let region = Region::new(GLOBAL);
            let error = polish::decode(&stream).unwrap_err();
            let allocated = region.change().bytes_allocated;
            assert_eq!(error.kind, DecodeErrorKind::Text, "{what} {count:#x}");
            assert!(
                allocated < 4096,
                "{what} {count:#x}: {allocated} bytes allocated for a {}-byte stream",
                stream.len()
            );
        }
    }
}
