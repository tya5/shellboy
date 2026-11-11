# Core 298: Bounded Structural Tool Facts (Proposal)

Status: source/decision checkpoint only. No payload authority changes yet.
Base: 05e358703c7c407c26ef9c7dafeb8714d71e95ef. Existing 298 branches were
not found at task start; this task owns issue-298/structural-tools.

## Verified Sources And Authority

- history/items.rs: function_call / function_call_output produce canonical
  node/pointer item identities and verified execution IDs; zero links are legal.
- model/reconcile.rs:16-19: exact call_id and name are reconstructed structured
  fields. model/dialect/messages.rs:584 and chat.rs:404 persist function_call
  name/call_id separately from private serialized arguments.
- compaction.rs:1370-1405: spill copies call items, but replaces result bodies
  with omitted metadata. Original retained nodes do NOT necessarily remain on the
  selected path; replacement references do not authorize global payload hydration.
- history/spill_tools.rs: from_outputs/load expose at most eight spill-local
  associations, 256-byte IDs, 16 KiB metadata, explicit truncation. This is #274,
  not a generic tool-detail endpoint and not to be reimplemented.

## Proposed Case / Branch / Named-Test Table

| Case | Qualification / authority | Proposed bounded public result | Named control |
| --- | --- | --- | --- |
| Generic zero-exec call | exact saved function_call name/call_id | canonical item + bounded name/call ID, executions empty, detail unavailable | generic_call_zero_exec |
| Generic zero-exec result | exact function_call_output call_id, no saved name | call ID only, no invented name/result summary, detail unavailable | generic_result_zero_exec |
| Shell-backed call/result | existing verified execution links | retain execution IDs; shell detail uses existing actual execution resource | shell_backed_preserves_links |
| Missing/invalid descriptor | absent/empty/wrong-type/oversized name or call ID | explicit per-field unavailable, never derive from arguments/body | invalid_descriptor_unavailable |
| Retained compacted spill | call item copied with actual name/call ID; omitted result metadata is private | copied call fields only; existing spill_tools coverage separate, no universal original+spill coexistence | compacted_call_descriptor |
| Pruned/source-budget source | existing unavailable reason, no parsed payload | existing typed unavailability, no recovery read | pruned_budget_no_hydration |
| Paged spill coverage | existing spill page + explicit local call/execution IDs | preserve existing truncated/unknown status; no association completeness inference | paged_spill_coverage_not_authority |
| Duplicate/conflicting call IDs | only current selected-scope structural facts | preserve distinct canonical identities; unresolved association, no name/time/adjacency join | conflicting_calls_not_merged |
| Secret/opaque extras | arguments/output/provider opaque/arbitrary extra text | never publish or parse for descriptor | descriptor_secret_exclusion |

## Contract Decisions For Independent Review

Propose extending the existing ToolCall/ToolResult variants, not a new reader:
optional bounded exact `call_id` and call-only `name`, with explicit descriptor /
detail availability. Names are structural tool identifiers, not descriptions or
argument-derived commands. Result-side name stays unknown unless a separately
approved scope-qualified request/result linkage proves it; no cross-node lookup
is proposed in this first increment. Coverage is NOT implied by a call-ID match.

Proposed name qualification: nonempty ASCII tool identifier, at most 256 bytes,
no whitespace/control characters; preserve bytes, reject rather than truncate.
Call IDs reuse the existing <=256-byte nonempty structural-ID policy. Review must
approve whether this name policy is sufficient public authority before code.

No generic readable argument/result payload is currently source-qualified. Expose
honest detail-unavailable instead of an unusable fabricated retrieval target. A
future narrow reader belongs with #290; this task will not add a competing API.
Safe shell command labels remain the execution resource's authority, not parsed
function arguments. Name/call metadata must share existing bounded per-node/item
and response accounting; exact additional byte accounting and availability enum
will be fixed in the adopted table before implementation, not left unbounded.

Public proposal is intentionally not final: independent S6 design and leader
approval are required before editing payload projection, protocol or schema.
Acceptance must use actual saved-producer synthetic fixtures, not handwritten DTO
only: zero exec, shell-backed, retained/pruned/compacted and bounded spill pages.
Negative tests must assert secret nonserialization and no positional/prose joins.
No DB migration/schema40, paid provider request, installed binary, private DB,
RPC or browser operation is needed. #297 attribution and #274 remain separate.

## Search And Evidence Limits

Documented hybrid project RAG returned no hits and semantic_not_ready /
index_incompatible. No reindex or embedding request was started. This proposal
uses current source and issue 298, not prior-agent conclusions as authority.
No Rust/heavy job, implementation test or runtime success is claimed here.

## Fixed Proposed Wire And Budget Table (S6 60c68ede)

Adopted by leader after S6 60c68ede conditions; no payload code at this checkpoint.
These fields extend existing ToolCall / ToolResult only. Canonical identities and
verified execution_ids retain their existing qualifications. No result name join.

| Typed field | Values / meaning | Source |
| --- | --- | --- |
| descriptor.call_id | optional exact string, omitted unless state available | this item's own typed call_id only |
| descriptor.call_id_state | available / missing / invalid / field_budget | closed ToolFieldAvailability enum |
| descriptor.name | optional exact string, ToolCall only; ToolResult always omitted | this call's own typed name only |
| descriptor.name_state | available / missing / invalid / field_budget / not_applicable | not_applicable on ToolResult, never inferred |
| detail_availability | execution_linked / unavailable | existing verified execution_ids nonempty / otherwise; not proof output is retained |

ToolFieldAvailability is a closed enum. `missing` means absent or null;
`invalid` means wrong type, empty, or (name only) non-ASCII / whitespace / control;
`field_budget` means a string exceeds 256 UTF-8 bytes. Validate length first, then
syntax. `available` has the exact field present; other states omit it completely.
These are metadata states, never text_complete or answer-completeness claims.
A name syntactically qualifies only as an identifier. The leader must explicitly
adopt current saved typed name/call_id as public structural authority; ASCII is
NOT a secret-safety guarantee. No text from extras/arguments/output/opaque qualifies.

`descriptor` is present even with no available fields. This is an honest bounded
identifier descriptor, not a natural-language description, argument summary or
result body. `detail_availability = execution_linked` only identifies existing
execution resource routing by actual ID; retention/access remains that resource's
responsibility. Zero-exec tools explicitly report unavailable; no reader is added.

| Budget boundary | Rule / omission |
| --- | --- |
| Each tool field | <=256 UTF-8 bytes, all-or-nothing; never publish a truncated ID/name |
| Each tool item | at most 512 new string bytes (call ID + call-only name), below existing 16 KiB leaf ceiling; fixed closed states only |
| Each source node | existing 1 MiB source-byte limit and actual source qualification remain; source_budget yields existing unavailable projection, no descriptor recovery |
| Response | sum both available field byte lengths in the SAME existing 4 MiB text/reasoning ledger; no independent metadata budget or free extra fields |
| Response exhaustion | preserve existing atomic node projection and null/deferred suffix with response_budget; publish no partial IDs or partial admitted node |
| Spill metadata | existing #274 8-source / 256-byte ID / 16 KiB / truncated rules unchanged; no new spill traversal or coverage inference |

Duplicate/foreign call IDs retain separate node/pointer identity and do not infer
name, result ownership or execution associations. No additional global lookup,
neighbor query, body hydration or generic detail read is authorized. Exact item
coordinates are kept even when all descriptor fields are unavailable.

Implementation will update effective-spec before payload source, add one formal
case helper for field qualification and reuse it in the existing tool branch;
extend exhaustive response accounting and generated schema/golden. Actual-producer
nine-row tests cover explicit field states, source/response budgets, Unicode IDs,
field-overbudget atomic omission, duplicates/foreign scope and secret exclusions.
S6 conditional design acceptance is not leader payload-authority approval and
not runtime adoption. Leader explicitly adopted the saved typed name/call-ID authority; implementation may proceed under this table.

## Implementation Checkpoint And Actual Evidence

The adopted wire/budget table is implemented in history/items + protocol; generated
schema is produced only by `cargo run -- protocol schema`. Spill projection admits
only copied exact function_call/function_call_output items; all other spill content
has no new readable-text authority. #274 association loader/paging is unchanged.

| Planned case | Concrete actual-producer / existing-boundary control |
| --- | --- |
| generic_call_zero_exec | history_window_items::generic_call_zero_exec |
| generic_result_zero_exec | history_window_items::generic_result_zero_exec |
| shell_backed_preserves_links | history_window_items::actual_projection_bounds_execution_links_and_preserves_source_identity (new descriptor/detail assertions) |
| invalid_descriptor_unavailable | history_window_items::invalid_descriptor_unavailable |
| compacted_call_descriptor | history_window_items::compacted_call_descriptor plus compaction::tests::spill_placeholder_avoids_repeated_arguments_and_reduces_synthetic_growth (actual spill_payload) |
| pruned_budget_no_hydration | history_window_items::actual_pruned_over_budget_and_oversized_leaf_producers (descriptor-absent assertions) |
| paged_spill_coverage_not_authority | history_relations::summary_and_spill_source_pages_preserve_ordinals_and_recorded_witness_after_tombstone plus unchanged history::spill_tools::tests::spill_tool_sources_bound_output_and_keep_missing_execution_unknown |
| conflicting_calls_not_merged | history_window_items::conflicting_calls_not_merged |
| descriptor_secret_exclusion | history_window_items::descriptor_secret_exclusion_and_exact_unicode_id_budget |

Additional ledger oracle: descriptor_both_fields_share_text_byte_ledger;
descriptor_bytes_share_response_budget_atomic_node_suffix proves both fields cost
512 bytes per tool and keep the existing atomic response-budget deferred suffix.
Actual response text, reasoning and descriptor fields share one exhaustive match.

First check PASS / unit14 PASS / window15 PASS + 1 FAIL demonstrated the existing
spill legacy-only gap. Original /tmp/core298-first-*.log retained. Corrected source
1407616f: check PASS / unit14 PASS / window17 PASS / relations5 PASS / schema golden1
PASS (b7461c3e). Its compaction filter matched ZERO tests, so no compaction runtime
claim is made from that invocation; exact named producer test is run separately.
No suite-wide, paid-provider, private DB, installed or Studio claim is implied.


Exact producer follow-up: actual compaction named control passed 1 test in
/tmp/core298-exact-compaction.log. The additional spill filter matched zero in
both integration and lib because the proposed test name did not exist; neither
zero-test run is counted. The actual existing spill_tools bounded-source control
is identified directly from source and run by its exact name. No test assertion,
production boundary or source qualification was weakened to obtain this result.
