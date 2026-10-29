# Issue #34: long-conversation recovery

Baseline: `6c577c8`. Implementation and local verification are complete; authorized push/native CI are pending. This record
tracks the public issue's eight requirements without treating proposed mechanics
as facts the HTTP transport can establish.

| Item | Adopted change / evidence required |
| --- | --- |
| 1. Maximum safe reduction | Omit all eligible groups outside the current request, latest complete tool group, incomplete work and pending instructions; preserve immutable originals and replacement/restore identity. No half-size target or all-or-nothing goal. |
| 2. Earlier trigger | First eligible large-input header timeout/5xx/disconnect triggers a single reduction; small input keeps existing retry. Explicit compact_hint_tokens alone enables pre-POST reduction. |
| 3. Recovery notice | Warn against restoring a large range; direct to keyword history:THREAD search and selected summaries. |
| 4. Existing safety | One adopted reduction/restart per Turn/generation, then recovery after another failure. No replay of saved tool effects, new state/table/worker/queue/dependency or source deletion. |
| 5. Thread-scoped keyword search | Reuse one recursive current-path/replacement-source scope for semantic history and RAG keyword reads. Return current-path membership separately; reject foreign Thread mixing. |
| 6. Header deadline | Separate configurable response-header wait from stream idle wait. Observe successful header latency, preserve long post-header silence and cancellation. Document measurement limits and default rationale. |
| 7. Retry budget reassessment | Keep consecutive-failure accounting. Assess available recorded evidence; distinguish loopback mechanics from representative provider measurements. Do not claim a measured optimal default without data. |
| 8. Transport diagnosis | Persist bounded facts for request-body source consumption, headers/body receive phase, elapsed time and known pool generation. Never infer TCP write completion, peer receipt, provider fault or exact reuse from unavailable reqwest facts. Isolate failed pool generations; disclose limitations with concurrent/HTTP2 requests. |

## Validation and continuation

Use synthetic temporary stores and loopback HTTP fixtures, with no paid API or
changes to default user data. Validate protected paths, partial safe reduction,
first-failure/proactive/repeated-failure behavior, keyword retrieval of omitted
sources, exact equality of shared scope membership, separate timeout phases,
success latency and truthful diagnostics. Standard check is format, all-target
strict Clippy and all-target tests; the baseline's known unresolved HTTP mock
failures are not waived or claimed fixed. Native Windows/Linux CI remains
unperformed until an authorized push. Keep issue open pending its three-OS gate.

Implementation is split by existing subsystem owners: compaction/Driver,
HTTP/configuration, and shared history scope/RAG. Root integrates and runs Cargo
and Git after source freezes; each owner updates adopted design before code.

## Implemented source and verification

`96669a5`, `cf1e7f2`, `4c1d200` and final test evidence `d84a06f` implement the
tracked boundaries. Format, strict all-target Clippy and all-target tests with
no-fail-fast pass (80 groups / 1,365 passed / 0 failed / 25 ignored). Current
HTTP+PTY ten-conversation and offline docs/RAG CLI checks pass; ten completed
attempts retain header observations. The actual incomplete-upload Driver test
proves two prefix reads, no third request, no trim and retained originals.

The two-minute header default is provisional; loopback 3.48M-character samples
measured p50 2ms / max 3ms, not provider prefill latency. Item 7 is reassessed as
retain eight pending representative operational retry traces; the production
measurement itself remains unavailable. Exact socket reuse/TCP delivery cannot
be observed through the fixed public reqwest API and is reported unknown. These
are explicit measurement/transport limits, not successful stubs.

The user later authorized push. Three-OS CI remains the pending remote gate.
