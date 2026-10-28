# Latest review follow-up and issues #32 / #33

Status: implemented and verified locally as `02b994a` and `0a72f9c`; no push or release.
Baseline: `df5b6b5`. Final standard check: 76 groups / 1,334 passed / 0 failed /
24 ignored, plus isolated delivery pressure, actual docs/RAG CLI and HTTP/PTY checks. The user authorized #32 and #33 implementation in addition
to the latest review comments. Keep issues open until their push/three-OS CI
requirements are met.

| Item | Decision / implementation | Verification |
| --- | --- | --- |
| #19 unknown deletion outcome | An unwind carries an uncertainty fact; UI does not prefix it with a definitive failure claim | UI dialog/status and retry regressions |
| #20 replaced source roots | Require a directory or regular file, reject symlink/FIFO/socket as incomplete; never remove old docs on incomplete scan | Actual Unix special-file dry/live tests |
| #27 busy vs corrupt readonly hints | Use typed SQLite codes; retry busy/locked, diagnose invalid schema/corruption; preserve readonly and repeat-charge warning | Real SQLite failures and writer behavior |
| #30 shutdown notification | Poll shared Notify even when no flush timer is active in owned shutdown | Empty-outbox/raw-full progress+force test |
| #31 intermediate agreements | Only User Guide Japanese; no prescribed search language; UI language evaluation only; deprecated alias removal belongs to #33 | Full catalogue captions, links and docs CLI |
| #32 priority delivery | Worker cap 480 + main reserve 32 within critical capacity 512; bounded receipt continuation; poll controls through terminal shutdown | Actual frontend saturation/cancel/quit/force; FIFO/permits/disconnect |
| #33 A migrations | Historical SQL/transforms live in storage::migrate and rag::store::migrate; fresh DB uses current snapshot; runtime validates current schema | v8/v9/v10 versus fresh, repeated reopen, readonly old rejection |
| #33 B index/GC/draft | Remove legacy index keys/marker inference, Work defaults, integer streak, missing GC horizon and attachment positions, old draft fingerprints and retry-display defaults | Current-format fixtures, missing-marker no-request rejection |
| #33 C API/config | Remove status alias/wrapper and history_read_bytes; required history provenance/context/selection and Message/Run/Turn context; protocol 2 | clap/serde/IPC version rejection and public get results |
| #33 D genuine current state | Remove receipt NULL-turn compaction fallback after writer audit; rename pending unknown key; warnings only in warnings, real failures in scope_errors | Compaction/interruption/readiness tests |
| #33 E documents | Adopted designs and decision records supersede earlier compatibility choices | Source/doc audit and guide links |

## Boundaries and reasons

- Root type changes between directory and regular file are accepted as the current
  explicitly configured path. Persisting historical root types solely to reject
  this rare transition would add schema state without protecting a current
  invariant. Non-file/non-directory roots cannot prove absence of previous files.
- Current query-profile markers cannot be reconstructed from database schema
  alone because endpoint identity depends on configuration. Reject missing markers
  and require an explicit index rebuild; do not fabricate identity or initiate
  paid embedding in a migration.
- A held node and its receipt may both have no Turn before assignment. Current
  assignment backfills both in the same transaction, so no current writer needs
  the receipt-based NULL-node-turn compaction fallback. The historical schema
  index stays in the current snapshot to preserve exact schema equivalence.
- Thread context may be absent on a real fork result. Shell process facts and
  unobserved timestamps may genuinely be unknown. These Options stay; required
  context/provenance is enforced where all current writers provide it.
- `PENDING_UNKNOWN_SNAPSHOT` still describes current pending receipts. Rename its
  stored payload key without a fallback reader; this pre-1.0 format break is
  intentional. Hybrid warnings no longer appear twice under scope_errors.
- No new Core operation/table/worker/channel/dependency or migration framework.
  Existing critical outbox capacity and receipt state carry the bounded work.
- Native Windows/Linux testing requires a later authorized push/CI run. Local
  success does not satisfy that gate; no installed binary/default DB is changed.

## Runtime compatibility audit

The final source audit checks removal of legacy index preparation/keys, unused
history_read_bytes, the previous pending snapshot key, old-owner IPC guidance and
status operation arguments. Historical migration SQL is imported only by
storage::migrate in production; control/receipt and gc/usage imports are confined
to test modules. Remaining legacy terms describe current ownerless-source and
unknown-fact policies, defensive oversized-input bounds or performance baselines.
Ordinary Compact still coalesces current requests without an explicit context
edit; this behavior is retained and renamed automatic rather than removed.

The retry-display writer always records delay and partial-output facts, so their
serde defaults are removed. History list/get factories always produce context
and provenance (plus list selection); these are required, as are observed count,
decode and receipt-budget facts. Tombstone payload length remains nullable and
empty execution lists may still be omitted by the current serializer.
