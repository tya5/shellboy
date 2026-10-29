# Review follow-up: issues #19 / #20 / #27 / #30 / #31 / #32 / #33

Baseline: `e977dfe`. Status: review fixes committed locally; focused checks passed; whole-suite gate unresolved.
Read the full comments, including both #33 A/B/D and C/E records; severity and
proposed fixes are inputs to decisions, not automatic implementation instructions.
No push, release, installed-binary update or default-data mutation is in scope.

| Issue / review | Finding | Disposition / verification plan |
| --- | --- | --- |
| #19 / 6482768184 | Previous uncertainty-prefix Low resolved | Keep behavior. Minor repeated uncertainty wording is harmless and preserves standalone diagnostic clarity; no product change. |
| #20 / 6482768589 | Previous special-file root Low resolved | Keep directory/regular-file policy and existing FIFO/socket tests. |
| #27 / 6482769021 | Previous cause-specific hint Low resolved | Keep typed SQLite error handling and rollback-journal test scope. |
| #30 / 6482769570 | Previous shutdown Notify Medium resolved | Preserve unconditional Notify through the #32 follow-up. |
| #31 / 6482770154 | All agreements and previous Low resolved | Preserve guide scope and label audit; clarify migration policy in #33. |
| #32 / 6482770797 Low 1 | UI delivery delays shutdown persistence | Advance accepted sends/draft saving before presentation-only waits; prove actual persistence with stalled UI. |
| #32 Low 2 | UI reserve gates autosave pump | Pump independently; retain bounded failure projection until main capacity exists. |
| #32 Low 3 | Completion after a checked receipt batch can remain stale | Fair bounded scans plus a repeat bit when Changed occurs during a pass; test over 128 pending receipts. |
| #32 Low 4 | Clone can accidentally lend main reserve to a worker | Make ordinary Clone yield only worker admission; retain exclusive main ownership without another service. |
| #33 A/B/D / 6482771473 Medium | Integer failure counters defeat failure recording and breaker | Startup schema upgrade cleans non-current derived counter records; Core keeps strict current decoding. Exercise old counter + real failed work and breaker. Audit other changed metadata. |
| #33 A/B/D Low 1 | General Core tests depend on old migrations | Use current schema for non-migration tests; keep historical SQL in explicit upgrade fixtures. Record SQL file placement decision. |
| #33 A/B/D Low 2 | RAG upgrade hint suggests adding an unrelated source | Make rag init writer-owned for existing stores; no source registration, indexing or API, and test preservation. |
| #33 A/B/D Low 3 | Unused receipt_turn_source index | Remove in upgrade and current snapshot together; retain schema-equivalence regression. |
| #33 A/B/D Low 4 | Old interruption key no longer grouped | Keep the intentional format break and document original/F6 access in known limitations; do not add a fallback decoder. |
| #33 C/E / 6482772085 Low 1 | Policy appears to forbid isolated upgrades | Explicitly allow startup migration modules while prohibiting Core shims; align effective spec. |
| #33 C/E Low 2 | Protocol mismatch incorrectly recommends retry | Separate version mismatch from endpoint identity change; report both versions and same-release recovery, before dispatch. |
| #33 C/E Low 3 | Four stale old-record guide notes | Document current unknown conditions; remove obsolete append and old-owner claims. |
| #33 C/E Low 4 | Old status alias in known limitations | State removed alias and canonical operation get. |

## Constraints and validation

Use existing owners, queues and status information. Do not add a job engine,
Core operation, task service, compatibility lookup or paid automatic rebuilding.
Root runs Cargo and Git after all code/bundled guides are frozen. Verify format,
strict all-target Clippy, all-target tests, targeted stalled-delivery and migration
regressions, offline docs/links, actual RAG CLI, local HTTP/PTY and isolated
10-thread delivery pressure. Record final source and results in acceptance;
previous 1,334 passing tests do not validate this follow-up. Keep all issues open
until their requested push and native three-OS CI gate is actually satisfied.

## Implemented boundaries

The current conversation schema is 11, wire protocol remains 2 and RAG schema
remains 3. See the decision log for the metadata audit, conventional SQL location,
exceptional double-write diagnostic limitation and the closing-send presentation
choice. Existing capacities stay 512 critical / 480 worker / 32 main and receipt
query 128 / projection 32; only current owner state gains the finite repeat bit.
No old data directory or installed binary is modified during verification.

## Local implementation and focused evidence

- `7fb41d9`: shutdown persistence, autosave delivery, receipt rescan and handle
  ownership (#32). All 104 frontend tests passed (one opt-in test ignored);
  the actual stopped-UI forty-message/draft persistence regression also passed.
- `c6f2b39`: schema-11 failure-counter migration, original-failure preservation,
  explicit RAG upgrade, protocol diagnostics and policy/guide corrections (#33).
  All 50 index integration tests, including both real-worker regressions, passed; IPC 44 passed, storage 22 passed,
  hot-query storage 3 passed and RAG selection/migration 14 passed.
- Isolated ten-Thread/250ms stopped-UI pressure: p50 7.29ms, p95 12.16ms,
  zero lagged events. This is a local synthetic observation, not an SLA.

Earlier attempts exposed two obsolete RAG test expectations and timing-sensitive
HTTP mocks. The former were updated to the current contract; the latter are
tracked separately from product changes. A baseline summary test failed with the
same HTTP-header timeout. The intermediate whole-library run completed with
571 passed / 10 failed / 10 ignored and one known hanging summary test excluded;
it is not a passing standard gate. The final standard check on `8368874` passed format, strict Clippy and all-target
compilation. Its library run finished 579 cases successfully, failed two and
ignored ten; one summary remained stuck after all other 591 finished, so the
owned test process was interrupted (exit 101). Acceptance names every remaining
failure and distinguishes the one proven baseline failure from unproven causes.
No successful full-suite/HTTP-PTY claim or issue closure is made.

Final implementation: `7fb41d9` / `c6f2b39`; test fixture transport: `8368874`.
Final link audit: 612 relative links, 38 fragments, four TOML blocks, no errors.
Staged diffs and all three added commits passed gitleaks. Raw evidence remains
ignored locally; the current blocker is verification, not an untracked review
finding. Push, native three-OS CI, release and installed-binary update remain
unperformed.

## Post-restart correction

The later #33 review supplied direct raw-TCP/reboot evidence for the earlier
loopback transfer fault. An isolated exact `6c577c8` rerun now passes the full
standard check (76 groups / 1,343 passed / 0 failed / 24 ignored) and the original
ten-conversation HTTP+PTY script. See [review round 5](review-round-5.md) and
[acceptance](../validation/acceptance.md) for attribution and the variant check.
This supersedes the old-source unresolved gate above without rewriting the
actual earlier failed attempts or claiming that new #34 changes are verified.
