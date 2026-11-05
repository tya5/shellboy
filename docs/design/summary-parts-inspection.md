# Summary Parts: Inspection and Reuse (Study for #232)

Status: **study, not adopted.** Nothing here changes a contract until the user decides; implementation waits for that decision. Evidence is from `origin/main` at `a65421c74b` (code reading, not a run). Claims not covered by an existing test are marked *unverified*.

## 1. What a summary part is today

### Record contents
One `app_meta` row, key `summary.part.<sha256>`, value = JSON of `CachedPart` (`src/compaction.rs` ~144):

| Field | Map part (`compaction.rs` ~1063) | Reduce part (`bounded.rs` ~172) |
|---|---|---|
| `text` | assistant text of the response | same |
| `sources` | ordered node IDs of the segment (full list; range is known) | **always `[]`** |
| `cache_key` | its own key | its own key |
| `raw_response` | full provider response, usage normalized (`usage::normalize_response`) | same |

Not recorded: creation time, Thread ID, stage (map/reduce, round, depth), prompt/format/adapter version, profile/model/effort, source revisions/hashes, or a reduce's child parts. These exist only inside the irreversible digest. Per-part usage is derivable from `raw_response.usage` (same `usage_of` reader as the ledger); model identity only as far as the provider echoes it.

### Keys
- Map: `summary_key(snapshot, sources)` (`compaction.rs` ~1448) = sha256 of `(FORMAT, PROMPT, ADAPTER|MARKER_ADAPTER, profile, model, effort, reasoning_epoch, [(node_id, revision, hash, attachments)…])`. **No Thread ID.**
- Reduce: `sha256("bounded-summary-v1", summary_key(snapshot, []), prompt, batch_text)` (`bounded.rs` ~106); the prompt includes size limit and round, the batch text includes child parts' `Shellboy source node IDs:` lines.
- The final summary node stores `summary_part_keys` = **map** keys only (`compaction.rs` ~1149). Reduce keys are referenced from nowhere. The `replacement` row stores `input_key = summary_key(all sources)` and `prompt_version`.

### Where parts are read
- Compaction map preflight and map loop; reduce loop. An exact key hit whose raw response re-validates (`CachedPart::reusable`) is reused with no request.
- `compact` preview computes keys (`compaction/preview.rs` ~720).
- TUI shows `summary_part_keys` as an opaque metadata field ("要約part参照"); `presentation/summary.rs` decodes the legacy `### Part N` body. No CLI/TUI reads a part body; `compact list` reports counts only.

The key has no Thread ID, and a fork **shares node rows and copies profile/model/effort/reasoning_epoch** (`history.rs` fork ~461). A fork summarizing the same shared prefix with the same settings therefore computes the same map keys and reuses the parent's parts (and vice versa); `segments` is a pure function of the group sequence from the range start, so leading segments align. *Unverified by test.* Unrelated Threads never share node IDs, so they never hit.

### GC
- Parts count toward logical history bytes (`gc_bytes_app_meta_history` partial index and quota trigger) and add no retention root (`gc.md` line 23).
- `Work::SummaryCache` (`gc.rs` ~457, `SUMMARY_PAGE` ~1046), signalled by history tombstones and Thread deletion, deletes a part when **any** `$.sources` ID is missing or tombstoned (tested: `tests/gc.rs` `summary_parts_count_toward_quota_and_explicitly_deleted_sources_invalidate_cache`).
- **Gap found:** reduce parts have `sources: []` (and malformed values default to `'{}'`), so `json_each` yields no rows and they are **never collected**. After a Thread is deleted and its nodes tombstoned, its derived reduce texts remain indefinitely, contradicting `gc.md` ("do not keep a deleted branch's derived cache forever"). This holds independently of #232.
- Map parts with all-live sources but no consuming summary (failed attempt, old model/epoch/prompt) persist while their sources live: bounded, counted, not a deletion leak.

## 2. Reuse scenarios

Needs: **ID** stable handle; **Range** source node IDs; **Body**; **Usage**; **Perm** survives deletion of the source conversation.

| # | Who | Does what with a part | Needs | Served today? | Needs a node? |
|---|---|---|---|---|---|
| S1 | Compaction, same Thread | Retry after failure/cancel/budget limit without re-paying finished segments | key match | **Yes** (tested) | No |
| S2 | Compaction in a fork/sibling sharing the prefix | Summarize the shared prefix without re-paying | key match | **Yes in code** while settings/epoch match; *unverified by test* | No |
| S3 | Compaction in an unrelated Thread with "the same" content | Cross-Thread reuse | identical node IDs | No; impossible (no ID sharing) | A node does not solve it either |
| S4 | User via TUI/CLI | See each map part, its range and cost, to understand the final summary | ID, Range, Body, Usage | **No reader**; keys shown but not followable; reduce parts lack Range and linkage | No |
| S5 | Model, same Thread | Read a finer-grained summary of an older span instead of the large raw source | ID, Range, Body | No (only summary node + raw sources via `history get`) | No |
| S6 | Model in another/child/forked Thread | Read another Thread's part as background | ID, Range, Body | No; fork inherits the summary node, parts unreachable | No (reader) |
| S7 | Search/RAG | `history search` hit on a part as a mid-grain span summary | indexed body + Range | No; `search_bodies` indexes `summary` nodes only | Yes (or a second index source, worse) |
| S8 | User/model after deleting the source conversation | Keep the paid summary as a standalone artifact | Perm | No, by design | Yes, plus a deletion-policy reversal |
| S9 | Accounting | Cost per part; savings by reuse | Usage | Partial: per-compaction `summary.usage.*`, created/reused counts; per-part usage in raw without reader | No |

Observations: money-saving reuse (S1, S2) already exists and depends only on exact keys, not visibility. S3 is unreachable by any option. S4/S5/S6/S9 need a reader, not a storage class. Only S7/S8 need nodes, and S8 reverses the deletion intent. Rolling compaction puts the previous summary node into the next prefix, so later map keys differ: reuse in practice comes from retries and forks.

## 3. Options

### A: keep cache, add read-only reader, record missing provenance
- Storage: no table/column/migration. New records gain JSON fields: reduce parts get `sources` (ordered union of inputs' sources) and `stage` (`map` / `reduce` + round), optionally `created_at`. The summary node's out-of-band payload gains `summary_reduce_keys` (replay sends only `item`). Old records are not rewritten; readers report absent fields as unknown.
- Reader: one bounded `app_meta` read by key. CLI e.g. `shellboy history get --node SUMMARY --parts` (or `compact parts --node SUMMARY [--key K]`), returning key, stage, source first/last/count, characters, usage from raw, `reusable`, and the body paged like `history get`. The TUI follows `summary_part_keys` through the existing inspection path; no new worker. The model uses the same CLI.
- Replay/fork: unchanged; forks reach parts through the inherited summary node.
- GC: rule unchanged. Reduce parts now carry `sources`, so the existing sweep collects them, closing the leak for new records. Map parts cannot vanish while their summary is live (sources protected via `replacement_source`); the reader still handles not-found (old, or overwritten by retry). Existing `[]` reduce parts: see open questions.
- RAG: not indexed (S7 not served).
- Cost: low (one read path, ~3 JSON fields, GC test). Serves S4/S5/S6/S9, keeps S1/S2, fixes the leak.

### B: promote parts to nodes (non-replayed kind `summary_part`)
- Storage: node row per part; provenance needs a `replacement` row (output = part, sources = its sources) or a new link. The cache key must be indirected (node ID ≠ digest; nodes are append-only, so retry overwrite becomes append).
- Replay: not on any `thread_path`, so never sent.
- GC: a node not on a path and not a replacement source is unreachable and **would be tombstoned at the next history GC**. Keeping it needs a `thread_replacement` root (then failed/unadopted attempt parts keep raw history alive, reversing "cache adds no root") or a new root rule "referenced by a reachable summary's keys" (JSON scan in the root stage).
- Fork: `thread_replacement` is copied only for outputs on the fork path, so parts would not follow; a new copy rule is needed.
- Semantics leak: `history relations --relation replacements`, the M11 reverse Turn-boundary walk (consumer edges count against its 65,536 budget), the search-scope CTE and attachment exclusion all read a completed replacement as "this source was replaced". Each would need a kind filter.
- Deletion: rooted under the summary, parts die with it (consistent). As independent roots (S8), deleting a conversation would no longer remove its summaries; a user decision.
- RAG: add the kind to `search_bodies`, which serves S7, at the cost of embedding work and near-duplicate hits (part, summary, sources) for one span.
- Cost: high. Conflicts with "reuse existing mechanisms" and core-admission scrutiny.

### C: simpler variants
- **C0 GC fix only:** record `sources` on reduce parts. Needed under every option.
- **C1:** A without the TUI (CLI `--parts` only).
- **C2:** no part search; the summary node (already indexed) and raw sources (in Thread scope via `replacement_source`) cover S7's span.

### Options vs scenarios

| Scenario | Today | C0 | C1 | A | B |
|---|---|---|---|---|---|
| S1 | yes | yes | yes | yes | yes (key indirection) |
| S2 | yes* | yes* | yes* | yes* | yes* only with fork copy rule |
| S3 | no | no | no | no | no |
| S4 | no | no | CLI | CLI+TUI | yes |
| S5 | no | no | yes | yes | yes |
| S6 | no | no | yes | yes | depends on fork rule |
| S7 | no | no | no | no | yes |
| S8 | no | no (correct) | no | no | only with policy reversal |
| S9 | partial | partial | yes | yes | yes |
| Reduce deletion leak | **leaks** | fixed | fixed | fixed | fixed via root rule |
| New table/event/worker | — | none | none | none | none strictly; new kind, root rule, reader filters |

`*` code reading only, no test.

## 4. Recommendation
**C0 now as a bug fix; A starting as C1 for #232; do not node-ize.**
1. The money-saving reuse (S1, S2) already works without visibility; publishing parts saves no further money. The value is inspection (S4–S6, S9), which a reader delivers.
2. Only S7/S8 need nodes. S7 is largely covered by the summary node and searchable raw sources; S8 contradicts the current deletion intent.
3. B touches replay, fork, GC roots, relations, Turn-boundary provenance and search scope, each a tested contract. A touches one JSON value and one read path.
4. C0 is required regardless: reduce parts currently retain deleted conversations' derived text forever.

Risks: old records lack the new fields, so show unknown and never reconstruct. Retry with the same key overwrites a part, so the reader shows the current record, not immutable history; state this. Bodies need `history get` paging and size limits. S2 needs a test before it is claimed.

## 5. Open questions for the user
1. Is S8 (paid summaries survive source deletion) wanted at all? If so, it is a deletion-policy decision before any storage design. This memo assumes not.
2. Is S7 worth a new node kind, given that the summary and raw sources are searchable?
3. Existing `sources: []` reduce parts: one-off cleanup (pre-1.0 debt), an orphan rule (needs `summary_reduce_keys`, so old ones remain unreachable), an explicit `storage collect` category, or leave them?
4. CLI shape: `history get --node SUMMARY --parts` or `compact parts`?
5. TUI in the first step, or CLI-only (C1) first?
