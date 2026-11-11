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
