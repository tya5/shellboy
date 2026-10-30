# Standard Usage Measurements (#49)

## Contract And Ownership

Responses usage persistence keeps only validated `input_tokens`,
`input_tokens_details.cached_tokens`, `output_tokens`,
`output_tokens_details.reasoning_tokens` and `total_tokens`. Unknown top-level
and nested fields (including provider attribution) are discarded, not truncated.
A count is known only for a JSON integer in `0..=9223372036854775807` (SQLite's
signed 64-bit range). Missing, null, negative, floating/exponent, boolean,
string and out-of-range values are unknown per field, never inferred as zero.
Zero remains a valid reported measurement. Counts are independent; no sums or
cache ratios are invented and inconsistent provider totals are not repaired.

One ordinary Rust usage module owns validation, canonical JSON encoding and
SQL scalar projection. Response completion/failure and summary audit persistence
normalize the usage member only; response output, reasoning, errors and identities
remain unchanged. Terminal model-attempt persistence uses the same normalization
and records reported failed/incomplete-response usage when available. Preserve
existing transition CAS, transaction, rollback and late-result behavior. This
is not an aggregate ledger or a change to retry/adoption policy.

## Existing Large Rows And Readers

Context usage, the latest-64 successful-attempt estimation baseline and F6's
exact-adopted-attempt cache display share scalar SQL extraction. An outer
`CASE WHEN json_valid(...)` protects every JSON operation, including malformed
existing rows. A value must have JSON type `integer`, SQLite extraction type
`integer`, and be nonnegative; SQLite may otherwise extract an oversized JSON
integer as REAL despite its JSON integer type. Do not cast such a REAL to INTEGER.
Only the five nullable scalars cross the DB/Rust boundary; never select the
whole usage document or reject a row based on its byte length. SQLite still
parses the source JSON; this is not a promise of constant work for a large row.
The existing bounded attempt lookup and 64KiB manifest limit stay unchanged.

No usage-row rewriting migration, new table, worker, queue, event, dependency,
periodic scan or user-data edit is added. Existing GC owns old usage bytes.
This normalizer is reusable by #47's separately agreed incremental aggregates;
#49 itself does not implement aggregates or edit the parallel index/CLI work.

The pre-existing non-2xx HTTP error path is diagnostic-only: it caps its body at
64KiB and currently exposes no response usage to persistence. #49 does not alter
its limits, diagnostics or payload exposure. #47 must separately agree how a
small valid error body supplies typed usage without retaining the whole body;
oversized, malformed and unreported values stay unknown, not zero.

## Guidance And Validation

The Agent Guide describes one review at a completed task boundary using the
last actual input tokens and large history ranges, not estimates. Preserve
unfinished work, unapplied instructions and information needed for the current
task. Spill unnecessary completed outputs or summarize completed ranges, with
get/search/restore preserving access to original facts. Do not compact each
request or add an automatic Core mechanism.

Synthetic temporary DBs and loopback providers must cover >64KiB legacy usage,
large unknown nested details, all three readers, saved whitelist-only usage,
completed/failed/incomplete/summary responses, null/invalid values and signed
64-bit boundaries. Model behavior at task boundaries requires later real-model
dogfood; mocks and guide wording cannot establish it.

## Adopted #47 Scope, Pending Aggregate Design

The latest explicit scope decision limits #47 to normal/summary model-request
usage (success, non-success and per-field unknown), separate Run/model/shell time,
and existing Turn/Run/Thread/child-Thread totals. Embedding indexing/query usage
and standalone RAG are out of scope. Their public coverage must say out_of_scope
with reason not_recorded, with no invented zero totals or request counts. Do not
add unused embedding aggregate rows/columns. Do not mix standalone RAG's separate
DB into owner statistics. A later small follow-up may add hooks after parallel
#42/#43/#55 index work lands; no such hook or index/index-CLI edit is authorized here.

Only the model non-2xx typed measurement extension remains proposed, preserving
existing error diagnostics, redaction and the 64KiB limit. Wiring shared HTTP
measurements into embedding is withdrawn from this scope. B (saved fine-grained additive aggregates) is now the only period direction:
all-time plus UTC coarse buckets plus exact-instant aggregate rows, with shared
4,096-row query budget and explicit partial coverage/continuation. A's typed-fact
edge exception, minute-only filtering and prefix/Fenwick amplification are withdrawn.
The full minimal DDL, capacity, retention and initialization candidate still needs
leader and independent design review; this does not authorize implementation.
See the [revised proposal](../development/issue-47-design-proposal.md), which
separates accepted directions from open tradeoffs. #49 native success is a separate
gate before any #47 change can join main.
