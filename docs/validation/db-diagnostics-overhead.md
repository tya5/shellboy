# DB diagnostics hook measurement

2026-11-03, Apple M1, macOS 26.3.1. `cargo test --release --lib measure_hook_overhead -- --ignored --nocapture`: five sequential passes, each 50,000 cached indexed point reads against 10,000 in-memory rows. Each pass compares no trace hook with statement/profile/row tracing and statement counters; it excludes worker scheduling and slow backtrace capture. This microbenchmark is not an end-to-end workload.

Overheads: 19.97%, 19.49%, 26.67%, 29.51%, 30.24%; median 26.67%. No variance or system-load control was applied.

The release test executable was 48 MiB; this is not a production-binary size comparison. The release test executable had no global Shellboy symbols under `nm -g`; a production release backtrace and size comparison with line tables have not been established. No debug profile change is made on this evidence.
