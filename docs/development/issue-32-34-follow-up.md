# Additional reviews of #32–#34

Baseline: `37bbdb1` (reviewed implementation `b8f1f84`). Review comments:
#32/6486175965, #33/6486176260, #34/6486175656.
Prior results do not establish success for this follow-up. The user has now
explicitly set the goal: finish these fixes, push, publish v0.2.0 and update the
local binary. Tag/publication follow successful local/native gates; verify all
public archives/checksums and the installed native executable. Record new native
results separately.

| Finding | Disposition (implemented unless noted) |
| --- | --- |
| #32 L1: completed receipt scan plus a later send looks like a partial scan | Clear checked bits at completed-pass boundary; preserve repeat fairness during actual partial passes. Test completed scan, new receipt and Changed together. |
| #32 L2: stopped saver only warns the first Thread | One notification bit per existing backlog entry; warn each affected Thread once, retain its latest snapshot and shutdown failure. No unbounded notification registry. |
| #32 shutdown-stage timing | Add bounded stage/elapsed facts to existing Shutdown diagnostics, keeping I/O on the existing diagnostic writer and preserving cleanup ordering. No new Core state/event/queue. |
| #32 mock split-write note | No timing workaround without measured cause; keep semantic assertions. |
| #33 L1: index stop loses causes and overwrites durable pause | Store one bounded pair of typed failure codes in service memory; preserve durable status reason and expose local stop evidence alongside it. |
| #33 initialize lock handoff note | Keep the existing unlock/reopen boundary: another writer may legitimately return busy. A null migration result correctly means this call performed no migration; no compatibility or retry mechanism is added. |
| #33 repeated attachment preparation watchdogs | Treat recurrence across macOS/Windows as a shared unresolved preparation symptom. Preserve watchdogs, add stage evidence if needed, run multiple native gates and diagnose any recurrence rather than infer resolution from one success. |
| #34 M1: trim_used disables later normal retry/overflow | Separate once-per-Turn trim right from first-resumed-request guard; successful adoption clears only the guard. |
| #34 M2: connect failures become incomplete upload | Distinguish before-body/connect failure from a true partial source; retain ordinary connection retry. |
| #34 M3: scoped keyword search repeatedly expands whole scope | Use indexed reverse membership and avoid rebuilding full scope per candidate/batch; measure thousands of synthetic nodes. |
| #34 ML1: slow uploads hit header timeout without reduction | Make true partial large-input failures eligible for the one safe reduction; document total pre-header deadline and lack of TCP delivery proof. |
| #34 ML2: pre-POST rejection fails Turn | Handle cancellation/stale selection/ordinary rejection conservatively and reload or continue without converting them into an unrelated recovery failure. |
| #34 L1: rejected/no-op trim spends right; oversized set rejects all | Spend right only after adoption; safely adopt a bounded prefix of eligible whole groups when the node bound permits. |
| #34 L2: small requests build full snapshots and noisy errors | Use existing attempt/input estimate before expensive planning; omit irrelevant threshold diagnostics. |
| #34 L3: inconsistent threshold estimates | Carry the actual activation decision/estimate rather than reclassify from post-planning estimates. |
| #34 L4: pool rebuild on every failure and permanent Err cache | Retire only appropriate Responses failures; rebuild off the async worker and permit later construction attempts after failure. |
| #34 L5: overlapping scopes double-count RRF evidence | Deduplicate the same ranked evidence across history scopes while preserving scope provenance. |
| #34 L6: deleted Thread reported as history_changed | Revalidate existence and return thread_not_found after embedding, with existing snapshot-change distinction intact. |

## Verification plan

Use synthetic histories, temporary stores and loopback HTTP only. Targeted
regressions precede the standard format/strict Clippy/all-target checks; root
owns Cargo/Git while subsystem owners edit separate files. Measure large scoped
search without a flaky latency assertion. Push only after local gates; use
multiple native CI runs to observe the known attachment symptom and inspect
its added phase/state diagnostics if it recurs. Track exact commands, sources,
results and untested environments in acceptance and status.


## Local verification before push

The 0.2.0 standard check passes 80 groups / 1,380 passed / 0 failed / 26 ignored.
Focused integration cases pass 56 / 0 / 2 ignored; explicit scope measurement
passes 1k and 5k synthetic originals with equal foreign populations (106/717ms,
zero embeddings). See acceptance for exact conditions, limits and earlier
compile corrections. No native/release/public-download success is inferred from
these local results. The user authorizes publication and local installation
only after the review fixes and final gates, without an additional approval step.
