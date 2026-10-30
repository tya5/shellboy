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

## Adopted #47 Model-Only Aggregates

The latest explicit scope decision limits #47 to normal/summary model-request
usage (success, non-success and per-field unknown), separate Run/model/shell time,
and existing Turn/Run/Thread/child-Thread totals. All embedding indexing/query
and standalone RAG recording are out_of_scope/not_recorded, never known-zero
usage/counts or unused aggregate rows. No index/index-CLI or #50 work is included.

Adopt [Model Usage Aggregates](usage-aggregates.md): all-time/day/exact-instant
saved totals, aggregate-only bounded period reads, independent source-marker
accounting CAS, both new tables in existing logical-history byte accounting,
existing-limit admission/terminal rollback, scope-lifetime retention, and fair
bounded initialization on the existing maintenance lifecycle. No generic future
reservation, new timer, fixed age or warning-triggered pressure GC is introduced.

The leader approves implementation only after independent design review resolves
P1/P2 findings. The model-only non-2xx typed extension preserves existing <=64KiB
diagnostics/redaction and catalog/embedding APIs. Its producer cleanup and paging
coverage contracts are part of the adopted specification, not implemented proof.
