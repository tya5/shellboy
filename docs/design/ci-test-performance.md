# Measured native test performance (#46)

Optimize development/CI execution without changing production durability,
platform coverage, test assertions, or ignored-test policy. Record source,
runner, build/link time, per-binary/test observations and failures separately.
Earlier logs are a baseline, not evidence that a later source passes.

## Layout and process boundaries

Group ordinary integration suites as sorted modules in one explicit integration
harness. Keep native termination and crash-owner subprocess suites as explicit
standalone targets so their self-executable child filters retain their contract.
Disable automatic discovery only with a completeness check: every direct
`tests/*.rs` suite must be registered exactly once, as a module or standalone
target. Preserve all existing test functions and platform cfgs. Verify inventory
identity and ignored sets before/after, normalizing only the added module prefix.
Do not copy test bodies or merge mutable fixture databases.

Initial measurement retains two Windows test threads. Grouping reduces repeated
linking and lets the existing harness schedule across modules; raising concurrency
at the same time would obscure attribution. Tests with native child ownership,
process-global configuration or internal serialization require explicit auditing.
No automatic retry may convert a failed test into success.

## Measurement and fixture work

Use stable Cargo/libtest and standard-library tooling; no nextest dependency or
nightly feature is required. Capture Cargo JSON artifacts from the complete
all-target build, then record native harness wall time, exit status, test counts
and names. Optional per-test profiling runs selected tests individually and must
be labeled isolated-process timing, not reconstructed concurrent duration.
Keep failure diagnostics and publish measurement artifacts even when a harness
fails. Compare Windows job/Tests/build/run timings and the three-OS test inventory.

Historical-schema fixture construction may batch raw setup DDL into a transaction;
production DbWorker startup/migrations, WAL/FULL settings, reopen checks and tested
receipt transactions remain unchanged. Large index source seeds may batch inserts
in one DB call/transaction while preserving every identity and assertion. Measure
these fixture-only I/O savings; never disable fsync in the implementation under test.

## Time and failure evidence

Distinguish product deadline selection, failure classification and retry policy
from native HTTP/process wiring. Pure policy tests should construct the relevant
facts or use an isolated virtual clock. Preserve meaningful native wiring tests;
a one-millisecond timeout can fire before a large upload starts and cannot prove
partial body-source consumption. Do not mechanically replace every timeout with
1ms or pause time around real SQLite/socket workers.

Deadlock watchdogs are generous upper bounds, not latency assertions. Success
returns immediately; increasing their bound does not add a sleep. Keep explicit
responsiveness/deadline checks separately named. Prefer existing change
notifications to polling when the observation is available, with subscription
before the state check and lag handled by reinspection. Do not introduce a new
Core clock, worker, event, queue, persistent table or test-only product mode.

Repeated native diagnostics remain available as an explicit workflow option.
The ordinary all-target suite still executes the same substantive cases on all
three OSes. Moving repetitions out of normal push does not permit omitting their
underlying tests. Defender exclusions and new concurrency limits require measured
causal evidence rather than assumptions.

The IPC saturation regression must demonstrate that status succeeds while all
24 wait receipts are still pending, before stopping their Run. Its client wait
expiry is longer than the deadlock watchdog, so waiter expiry cannot silently
free admission and make a blocked dispatcher appear responsive. Attachment
mock-arrival watchdogs retain saved-phase diagnostics. These guards are not
product response-time SLAs.
