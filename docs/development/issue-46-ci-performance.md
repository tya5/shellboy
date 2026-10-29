# Native test performance investigation (#46)

## Baseline evidence

Source `0469ca0` was pulled before work. The historical successful native run
[40954267575](https://github.com/tya5/shellboy/actions/runs/40954267575)
uses `a45f54e`; it is retained as historical timing evidence rather than an
exact-current-source pass. Its Windows Tests step took 653 seconds: about 290
seconds in build/link and 359.83 seconds summed harness execution. The complete
job took about 855 seconds. Windows used two test threads. The repository has
82 integration source suites;86 harnesses includes library, main and examples.

| Windows harness | Seconds | Passed |
| --- | ---: | ---: |
| Library |79.61|576|
| storage |65.58|23|
| index |29.87|50|
| control_db |23.64|23|
| response_retries |20.09|22|
| response_upload_recovery |18.04|3|

The same-SHA main run 40954266583 failed different watchdog tests on its first
two attempts before succeeding: IPC waiter completion, then two attachment
mock-arrival waits. These are not treated as evidence of shared database/port
collisions or fixed by automatic retries. Preserve their names and diagnostics.

## Chosen changes and attribution

- Consolidate 80 ordinary suites into one harness; retain two self-spawning
  native suites. Integration links fall from 82 to 3. A registration checker and
  same-OS normalized inventories prevent silent coverage loss.
- Batch only historical raw fixture seed DDL and the 405 excluded-source inserts.
  Real DbWorker migrations, production FULL/WAL, tested Store transaction
  boundaries,405 actual embeddings and all assertions remain unchanged.
- Keep Windows concurrency at 2 for the first comparison. A separate four-thread
  experiment records failures rather than retrying until success. No conflicting
  shared mutable database/port/env fixture was identified by the source audit.
- Give deadlock watchdogs generous bounds while retaining the observed condition.
  IPC status must answer with all 24 waiters still pending, before their much
  longer expiry. This strengthens the causal admission check.
- Keep underlying native tests on every OS; make repeated diagnostics opt-in.
  Ordinary CI records build/harness times, failed names and inventories; isolated
  profiles are explicitly labeled and add process-start cost.
- Do not add nextest/nightly or Defender exclusions: stable Cargo/libtest plus
  Python standard-library measurement suffices, and Defender causality is unproven.
- Do not mechanically use 1ms or pause virtual time around real DB/socket work.
  Partial 16MiB upload source consumption is a native transport observation;
  a pre-upload 1ms expiry would test a different phase. Existing pure deadline
  and retry matrices already test policy without waiting. Further elimination
  of duplicate waits must preserve that boundary, not introduce a Core test mode.

## Current verification boundary

macOS ARM64 isolated before/after measurements use fresh process execution of
all 23 storage, 50 index and 23 control_db tests. Storage samples total 1.014→0.662s;
public-schema convergence 0.232→0.089s; v6/v7 matrix 0.189→0.072s. The405-node
index case is 0.913→0.902s and index's aggregate isolated time did not improve
(5.171→5.320s); do not claim a native Windows saving from these local samples.
control_db is unchanged and 1.417→1.510s. Cache/process overhead and runner noise
remain part of these isolated measurements.

The initial aggregate local run passes 1,452 tests, 0 failed, 28 ignored with
all 1,480 identities and the ignored set equal to the original 82-suite inventory.
This includes real Unix termination/crash tests; it is not Windows success.
Final standard-check and native before/after results belong in
[acceptance](../validation/acceptance.md). Native CI measurement remains pending;
no Windows improvement or three-OS completion is claimed yet.
