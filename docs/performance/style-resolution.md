# Style resolution benchmark

Baseline: `aebaa2a80dee9fa1715d30e2c7ea49623fe06617` (`main`).

The state fallback chain previously allocated a Vec for each property lookup.
A complete style asks for 27 properties; inherited component lookups can add
more chains. Replace it with a four-element stack array and bounded iterator,
retaining the exact order and stopping point for every state. GUI document
drawing calls `resolve_style` for object and text styles.

## Reproduce

```sh
cargo run --release -p om-layout --example style_resolution_bench --locked
cargo run --release -p om-layout --example style_resolution_bench --locked -- --dump
cargo test -p om-layout --all-targets --locked
```

Copy this same example into a baseline checkout and build with the same
compiler for comparison. The dump mode emits 16,896 complete ResolvedStyle
outputs across three themes, 256 objects, 11 states and two components.
Both outputs were compared in full and are identical (SHA256:
`c7646d997a37b3aa26ac12ae4c23a63ea76d5cfb570be8744a428c70bb95df07`).

## Measurement on 2026-10-29

Rust 1.99.1, normal Cargo release profile, x86_64 Linux KVM VM exposing
eight QEMU virtual CPUs, both binaries pinned to CPU 7. Runs in
baseline/candidate/candidate/baseline order. Each run emits 11 samples per
workload; sample 0 is warmup. The table combines all 20 remaining samples per
workload/version. Raw measurements, including warmups, are committed in
`style-resolution-samples.csv`.

Each timed sample resolves 51,200 styles: 256 objects x two components x 100
iterations, mostly normal state with some hover/combined states. Theme data
is empty, sparse (roughly a quarter of queried properties), or fully populated;
some objects have local font-weight overrides. Fixture construction and output
comparison are outside timing. Return-value construction/destruction is timed.

| Theme | Baseline ms | Candidate ms | Time reduction |
| --- | ---: | ---: | ---: |
| empty | 52.324 | 29.182 | 44.2% |
| sparse | 94.267 | 74.091 | 21.4% |
| full | 70.282 | 45.642 | 35.1% |

These synthetic microbenchmark results do not measure end-to-end GUI frame
time. Shared VM scheduling produces noise. Independent earlier direct-rustc
batches also improved sparse and full themes; the table here uses the actual
Cargo-built example only.

Validation includes existing style tests and a regression that exercises each
fallback in order, then proves resolution stops before unrelated padded states.
Cargo tests pass (29 passed, four existing fixture-dependent tests ignored);
Clippy with warnings denied passes. No FileMaker UI acceptance was run.
