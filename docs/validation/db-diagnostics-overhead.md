# DB diagnostics hook measurement

2026-11-03, Apple M1, macOS 26.3.1. `cargo test --release --lib measure_hook_overhead -- --ignored --nocapture`: five sequential passes, each 50,000 cached indexed point reads against 10,000 in-memory rows. Each pass compares no trace hook with statement/profile/row tracing and statement counters; it excludes worker scheduling and slow backtrace capture. This microbenchmark is not an end-to-end workload.

Overheads: 19.97%, 19.49%, 26.67%, 29.51%, 30.24%; median 26.67%. No variance or system-load control was applied.

On the same machine, two production `cargo build --locked --release --bin shellboy` outputs measured 31,651,664 bytes without release debug information and 33,618,744 bytes with `CARGO_PROFILE_RELEASE_DEBUG=line-tables-only` (+1,967,080 bytes, 6.21%). The normal binary contains local Shellboy symbols under `nm`; a release-profile capture test passed, but it did not verify the filtered symbol names in a production event. The first build started while the branch was rebased, so these sizes are indicative, not a controlled same-revision comparison. No debug profile change is made on this evidence.
