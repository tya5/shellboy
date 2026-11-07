# Thread Prune, Rewind and Branches (Study)

Status: **P1 (prune) adopted by the user on 2026-11-06 with two simplifications:
spills are included (one rule for summaries and spills, nested included), and there
is no `--older-than` and no byte estimate (`reclaimable_bytes: null`). Implemented as
#235; see [GC](gc.md#explicit-prune-of-replacement-originals-235). The prune section
below is the study; where it excludes spills or reports `candidate_payload_bytes`
only as a fact, the adopted behavior above wins. R1 (rewind) is adopted by the user (2026-11-06) in a simpler form: branches are
Threads. No `thread_branch` tables and no migration; the cut-off part becomes an
ordinary fork child and there is no in-place forward (#236; see
[History](history.md#rewind-236)). The R1 storage design below is superseded; R2
(switch/forward) stays a study, not adopted. B1 (branch view, option d) is adopted
by the user (2026-11-06) as branches-are-Threads: no `thread_branch` tables and no
migration; the tree is read from fork relations (#237; see
[History](history.md#branches-237)). Differences from §4: the panel and the
`thread branches` CLI list Threads of one family instead of `thread_branch` rows;
switching opens the branch Thread; the panel keys are Enter (open), F (fork),
P (prune form), N (more), Esc; F8 holds an entry and there is also `/branches` (no
F11/F12); cursor and bounds are those of History. Compare stays out of scope.** Original status line: It covers three user directions from 2026-11-06:
explicit prune of compacted originals, rewind in place, and a branch view in
the TUI. For the branch view the user chose option (d): an inline marker plus a
branch panel. Evidence comes from reading the code at `origin/main` `7cb82d3923`.
Nothing was run. Claims that no existing test covers are marked *unverified*.
Line numbers are approximate.

## 0. Summary

| Idea | Recommendation | New storage |
|---|---|---|
| 1 Prune | Add a `pruned` value to `replacement.state` (the column has no CHECK, so no migration). The existing history GC treats a pruned replacement as no longer protecting its sources, and the prune enqueues a normal history `gc_job`. A replacement shared with another live Thread is skipped and reported. | none |
| 2 Rewind | Move the head back with the existing `select_path` and `validate_group_boundary`. The cut tail is saved as a branch record, so the rewind can be undone, and the same command moves forward again. | one migration: `thread_branch` and `thread_branch_node` |
| 3 Branches | Option (d): an inline `↳ 2/3 ⋯` marker on the page rows already loaded, plus a panel that lists only divergence points and tips. Every panel action maps to one CLI command. | none beyond idea 2 |

One fact changes the framing. **`/edit` does not create an in-thread branch
today.** `accept_ordered_edit` (`src/control/db.rs` ~995) calls
`history::fork_thread` to make a new Thread, drops the edited request from that
fork's path with `select_path`, and sends the new text there. The schema has no
branch concept. The only off-path nodes inside a live Thread today are:

- compaction originals;
- unadopted or incomplete attempts (`incomplete_response` audit nodes).

All of these are protected because their Turn belongs to the Thread.
"In-thread branches" therefore start with rewind. The `/edit` forks of today
appear in the panel as fork children.

## 1. What exists today (verified in code)

### History storage
- `thread_path(thread_id, ordinal, node_id)` stores the exact selected path. It has `UNIQUE(thread_id,node_id)` and index `thread_path_node(node_id,thread_id)`.
- `thread.cursor_node_id` is the head. `selection_revision` is used for compare-and-swap (CAS).
- `edge(src,dst)` is a DAG with a cycle trigger. `history.md` forbids inferring a path by walking the DAG backwards from the cursor. Compaction also adds edges: a suffix node ends up with two incoming edges, one from the summary and one from the last original. So a walk in either direction is ambiguous.

### `select_path` (`src/history.rs` ~177)
1. CAS on the expected selection revision.
2. Checks that nodes exist and do not repeat, and inserts any missing edges.
3. Rewrites the whole path: it deletes all of the Thread's `thread_path` rows and inserts new ones. Cost is O(path length).
4. Updates the cursor, both revisions and `root_revision`.

`edit_invalidates_kept_reasoning` (~253) returns `None` when the new path is a prefix of the old one. So truncating the path does not restart reasoning. A new path that has output-carrying nodes after the edit point does restart reasoning after that point (#182).

### `fork_thread` (`src/history.rs` ~400–480)
- The fork node must be on the source path. `validate_group_boundary` rejects a prefix that still has unmatched tool calls.
- The fork copies the path prefix (O(prefix) rows), cwd, AGENTS, profile, model, effort and reasoning epoch, and the `reasoning.keep.*` set.
- It copies `thread_replacement` rows only for outputs on the fork path, and `attachment_selection` rows only for sources on the fork path.
- It bumps `root_revision`.

### GC roots (`src/history/reachability.rs` `ROOT_PREDICATE`)
A node is a root if any of these holds:
1. It is on a live Thread's `thread_path`.
2. **Its `turn_id` belongs to a live Thread.**
3. A live receipt or reply references it.
4. An accepted or held `attachment_delivery` references it.
5. A `runtime_pin` references it.
6. It is the output of a replacement that a live Thread holds in `thread_replacement`.

Provenance (`src/gc/history.rs` `provenance_step` ~180) expands every replacement whose output node is marked into its `replacement_source` rows. This happens **for any `state`**. The destructive revalidation uses a "consumers" CTE (`src/gc/history.rs` ~299, sweep ~275, `reachability::searchable`) that walks `replacement_source` upward, also for any state. Sweep tombstones by setting `payload_json` to NULL with the fixed reason `thread_deleted_unreachable` (`tombstone_node`). Index vectors are deleted in the same transaction.

### Search scope (`src/history/search_scope.rs`)
The path plus the sources of `state='completed'` replacements, expanded recursively.

### Replacements
Production code writes only `state='completed'` (`src/compaction.rs` ~1436). The output node kind is `summary` or `spill`. Restore re-inserts the originals at the marker and keeps the `thread_replacement` row ("the audit record and GC root stay in `thread_replacement`", `compaction.md`). A spill placeholder tells the model `retrieve: shellboy history get --node <original_node_id>` (`src/compaction.rs` ~2037).

### Attachments (`src/attachments/selection.rs` ~59)
An `active` selection whose source node leaves the path is still listed as active. Only `context_replacement` exclusions off the path count as detached. `/edit` deletes the edited node's selection in its fork explicitly.

### Thread deletion
- It enqueues `gc_job(kind='history', cursor_json={"deleted_thread_ids":…})` (`src/control/db.rs` ~2608). The cursor field is informational.
- `thread delete-preview` returns `reclaimable_bytes=null` on purpose, so that it does not re-implement GC reachability (`deletion-impact.md`).

### TUI vocabulary
- Row marks are listed in `src/presentation/rows.rs` `RowKind::spec`: `›` `↳` `↺` `↻` `▸` `▹` `⏵` `◌` `✓` `✗` `⊘` `⁂` `⬦` `>` `◉`.
- `src/ui/text_width.rs` holds the UI glyphs (`◉ › $ ↳ ⋯ ⌘ ⏻ ⌕ ⟡`) and tree pieces (`branch()` = `├─`/`└─`, `vertical()` = `│`, `arrow()` = `→`).
- When the terminal does not draw these glyphs as single-width, the same helpers fall back to ASCII: `+-` `\-` `|` `>` `.`. `controls()` renders `<>` as `←→`.
- F1–F10 are bound. F11 and F12 have no app binding. History focus does not use ←/→.

## 2. Idea 1: prune compacted originals

### What protects an original today
Take an original `O` in live Thread `T` that summary `S` (replacement `R`) has replaced:

| Holder | Rule | Removed by prune? |
|---|---|---|
| T's Turn owns `O` | root rule 2 | **must be**, for O as a source of R |
| `S` on T's path → provenance from R → `O` | `provenance_step` and the consumers CTE | **must be** |
| `thread_replacement(T,R)` → `S` → provenance | root rule 6 | same as above |
| Fork F holds `O` on its path (forked before compaction) | root rule 1 | no; O survives |
| Fork F forked after compaction (holds `S` and a copied `thread_replacement(F,R)`) | rules 1 and 6, then provenance | no; R is shared, so it is skipped |
| Receipt, attachment delivery or runtime pin | rules 3–5 | no |

So the hold is not just `replacement_source`. **Turn ownership alone keeps every
original forever** while T is live. That is also why forking T and deleting it
would not work: the fork copies `thread_replacement`, provenance still holds,
and T's identity is lost.

### Minimal change
1. **State.** `thread prune` sets `replacement.state='pruned'` on the chosen replacements in one transaction.
   - A replacement qualifies only if it is completed, is held by T, has its output on T's path, and is held by no other live Thread. "Held" means a `thread_replacement` row, or the output on that Thread's path.
   - The prune also covers the nested closure: replacements whose output is a source of a replacement being pruned, under the same conditions.
   - It then bumps `root_revision` and enqueues `gc_job(kind='history', cursor_json={"pruned_thread_id":T,"replacements":n})`.
2. **GC rule.** Four SQL sites skip `state='pruned'`: `provenance_step`'s replacement lookup, the consumers CTE in `gc/history.rs` (twice), and `reachability::searchable`. Root rule 2 gains one `NOT EXISTS`: Turn ownership does not protect a node that is a source of a pruned replacement held by that same Thread. Every other holder still works, because path, provenance from unpruned replacements, receipts, attachments and pins are all separate rules.
3. **Reason.** `tombstone_node` records `pruned_unreachable` when the node is a source of a pruned replacement, using one indexed `EXISTS`. Otherwise it keeps `thread_deleted_unreachable`.

The rest is reused unchanged: batches, budgets, resume, pin revalidation, index-vector deletion, the `SummaryCache` sweep (map parts whose sources are now tombstoned are collected) and storage status. `search_scope` already filters on `completed`, so pruned sources drop out of Thread scope. No table, worker or queue is added.

### Command
```text
shellboy thread prune --thread T --before NODE [--dry-run]
```
- `NODE` must be on T's path.
- The prune selects replacements whose output is at an ordinal below `NODE`'s ordinal. To prune all of them, pass the head.
- Spill replacements (output kind `spill`) are **excluded** (see the open questions).
- It runs while a Run is live. Pins and per-node revalidation already make the destructive step safe, so the model can prune its own Thread.
- It refuses while a `context.edit` (restore or summary) for T is queued or running. This reuses the existing per-Thread edit ordering.

Result and dry-run fields are all facts from bounded indexed queries:
- `replacements_pruned`
- `replacements_shared_kept` (held by another live Thread)
- `source_nodes` (in the closure)
- `source_nodes_on_other_live_paths` (will survive)
- `branch_anchor_skipped` (see §5)
- `candidate_payload_bytes`: the sum of `octet_length(payload_json)` of candidate sources. This is a fact, not a promise of how much will be reclaimed.
- `reclaimable_bytes: null` with a reason, as in delete-preview
- `gc_job_id`
- `affected_markers`: up to 50 marker IDs and excerpts, plus a count

The closure is bounded at 65,536 nodes, the restore provenance budget. If it is larger, the prune refuses and suggests an earlier `--before`.

### What is lost, and stated as such
- `compact --restore` for pruned markers. It is refused with `invalid_state` and reason `pruned`, and `compact list` shows the state.
- Rewind or fork into the range. It was already impossible without a restore.
- Raw search hits for the originals. Their vectors are deleted.
- `history get` returns `tombstone_reason=pruned_unreachable`.
- Summary-part cache entries for the range, and therefore cheap re-summarization.
- The summary and omission texts are immutable, so restore hints inside them become stale. A follow-up `history get` shows the tombstone.

What is kept: summary nodes, node IDs, edges, `replacement_source` rows (provenance as IDs), shell raw output (it has its own retention), and usage records.

### Simplest version
| Element | Cost | Decision |
|---|---|---|
| `--older-than DURATION` | A time-to-node mapping and a second selector. It also invites a scheduled or implicit use. | **Drop.** `history list` shows timestamps, and the TUI picks a node. |
| `--before NODE` | One ordinal lookup | Keep. It is the only selector, and the head means "all". |
| `--dry-run` | Same code as the real run, without the writes | Keep. Prune is the only irreversible step. |
| Byte estimate | Re-implements reachability | Drop it. Report `candidate_payload_bytes` as a fact and `reclaimable_bytes: null`. |
| Shared-kept counts | Two indexed EXISTS per candidate, within the bounded closure | Keep. This is how the "nothing shared is deleted" promise becomes visible. |
| Spills | — | Excluded in v1. No flag. |
| Export before prune | — | Not built in. `history get` and `readback` already exist. |
| A separate prune worker | — | None. The existing GC job does the work. |

Is prune just the existing delete/GC path? The GC half is entirely the existing
path. The only new part is releasing the hold: one state value and one
`NOT EXISTS`.

## 3. Idea 2: rewind in place

### Where the cut tail goes
The design forbids inferring paths from the DAG, so an undoable rewind has to
store the tail it cuts exactly. There are two candidates:

- **B1, zero schema.** Rewind runs `fork_thread` at the head and then truncates T. The tail lives on as an ordinary fork Thread.
  - Costs: each rewind copies the whole prefix (O(path)) and adds a Thread to the list.
  - Moving forward in place would need an "adopt another Thread's path" operation that copies that Thread's `thread_replacement`, attachment and reasoning state, which may have diverged.
  - It also merges in-thread branches into forks, against the chosen option (d).
- **B2, recommended.** One migration adds two tables:
  ```sql
  CREATE TABLE thread_branch(id TEXT PRIMARY KEY NOT NULL, thread_id TEXT NOT NULL REFERENCES thread(id),
   anchor_node_id TEXT NOT NULL REFERENCES node(id), state TEXT NOT NULL CHECK(state IN ('live','pruned')),
   created_at TEXT NOT NULL, operation_id TEXT NOT NULL);
  CREATE INDEX thread_branch_anchor ON thread_branch(thread_id,anchor_node_id);
  CREATE TABLE thread_branch_node(branch_id TEXT NOT NULL REFERENCES thread_branch(id), ordinal INTEGER NOT NULL CHECK(ordinal>=0),
   node_id TEXT NOT NULL REFERENCES node(id), PRIMARY KEY(branch_id,ordinal));
  CREATE INDEX thread_branch_node_id ON thread_branch_node(node_id);
  ```
  Only the tail after the anchor is stored, at O(tail) cost. Thread identity, `thread_replacement` and attachments stay with T.
  - **Owner:** the Thread.
  - **Bound:** a tail is at most one path. Listing is paged.
  - **Ending:** Thread deletion leaves the rows. They stop being roots and are removed by GC's existing release step for deleted owners.
  - **Invariant:** every recorded node is either on T's path or in exactly one live branch tail, because a switch moves rows and never copies them.

### Semantics
```text
shellboy thread rewind --thread T --node NODE
```
1. **Refusal.** If T has a live or stopping Run, the command refuses. It uses the same live/stopping check as `/edit`, with no automatic stop. Held shells or child Runs keep the Run live, so the refusal covers them (`control/driver.rs` ~559, *unverified for every held-work kind*).
2. **`NODE` on T's path.** `validate_group_boundary` runs on the prefix. The tail after `NODE` is saved as a branch anchored at `NODE`. Then `select_path(T, path[..=NODE])` runs; truncation causes no reasoning restart. All of this happens in one transaction with a CAS on `selection_revision`.
3. **`NODE` in a live branch B's tail (forward or switch).**
   - The command resolves B's full path: the anchor's position on the current path, or recursively in its parent branch (bounded depth 64).
   - It saves the current tail after the divergence point as a new branch, moves B's rows up to `NODE` into the path, and keeps B's rows after `NODE` as a branch anchored at `NODE`.
   - `select_path` here needs a flag that skips `edit_invalidates_kept_reasoning`. The restored outputs were produced on exactly this prefix (#182 risk, see §7).
4. **Attachments.** Active selections whose source moves off the path become `excluded` with a new reason `branch`. Switching back reactivates only `branch`-excluded rows whose source returns to the path. *Unverified that this does not conflict with `context_replacement` handling. Needs a test.*
5. **What the result reports** (bounded facts only):
   - the new head and `saved_branch_id`;
   - the cut node and request counts;
   - shell executions in T's Turns created after the target node: a count plus up to 20 rows with ID, command excerpt, state and `started_at`;
   - child Threads whose first Run started after the target: a count plus up to 20;
   - the undo command: `thread rewind --thread T --node <old head>`.

Nothing outside the conversation is undone:
- **Files and shell effects** stay as they are. The next request gets a new context row only when cwd differs (#183). Child Threads and background shells are kept and listed.
- **Range limit.** Rewinding into a summarized range needs `compact --restore` first, because `NODE` must be on the path. After a prune that is impossible.

### GC impact
Branch tails are mostly owned by T's Turns, but not always. A rewind to before
T's fork point puts the parent's prefix nodes into the tail. If the parent is
later deleted, the tail is their only holder. `ROOT_PREDICATE` and
`DELETED_OWNER` therefore gain a clause: the node is in a `live` branch of a
live Thread. Pruning a branch (§4) sets `state='pruned'`. Root rule 2 then also
skips nodes in that Thread's pruned branches, in the same way as pruned
replacements.

### Simplest version
| Element | Cost | Decision |
|---|---|---|
| In-place truncation | Existing `select_path` and group validation | Keep. It is the core. |
| Saving the tail (undo) | The only new storage | Keep. Without it, rewind loses the path (B1 is the zero-schema fallback). |
| Forward and switch | Same command, same tables. Only the reasoning flag and the attachment reactivation are new. | Keep, with one verb: "move head to NODE", wherever NODE is. |
| `--dry-run` | A second code path | **Drop.** Rewind is reversible, so the result itself is the preview, with an undo line. |
| Shell and child listing | Two bounded queries | Keep as counts plus up to 20 rows. It is transparency, not a gate. |
| Live-work refusal | One existing query | Keep. Neither `/rewind` nor `/edit` stops a Run automatically. |
| A separate `/edit` in place | — | Not in v1. Rewind followed by a normal send is equivalent. Whether `/edit` should switch is a question for the user. |

Is rewind just `/edit` machinery? Yes. It is `/edit` minus `fork_thread`, minus
the send, plus saving the tail. Should rewind and prune be one command? No: one
is reversible and the other is not. They share target addressing (`--node`,
branch IDs) and the `--branch` form of prune.

## 4. Idea 3: branch view, option (d)

There were four candidates:
- (a) Pure git-graph lanes spend width on narrow terminals and repeat the whole history.
- (b) A request-only outline also repeats unchanged history.
- (c) An inline-only marker cannot show the whole tree.
- (d) combines (c) for local switching with a panel that shows only divergence points and tips. **The user adopted (d).**

### Inline marker
- **Position.** One muted row directly below the anchor node's last row, only when the anchor has at least one live branch.
  - Ja: `↳ 2/3 ⋯ 「mockで別案」 · ←→ 表示 · Enter 切替`
  - En: `↳ 2/3 ⋯ "try with mocks" · ←→ view · Enter switch`
  - `2/3` is the position of the current continuation. The ordering is by the `created_at` of each alternative's first node.
- **Keys.** In history focus with the marker row selected, ←/→ change which alternative's excerpt the row shows. They never switch on their own. Enter opens a confirmation popup with the switch facts, then calls `thread rewind`.
  - ←/→ are unused in history focus today.
  - Narrow help uses `controls()` (`<>` becomes `←→`).
- **Query.** It rides on the existing page enrich, like the #230 summary row: one query per 50-row page.
  ```sql
  SELECT anchor_node_id, count(*) FROM thread_branch
  WHERE thread_id=? AND state='live' AND anchor_node_id IN (<page ids>)
  GROUP BY anchor_node_id
  ```
  The excerpts are fetched only for the selected marker. On failure the page stays plain.

### Branch panel
Opened with `/branches` and an F8 entry "会話 | 分岐" / "Conversation | Branches". The F-key is an open question. It lists divergence points and tips only.

Wide layout (≥ 80 columns):
```text
◉ 分岐 | 設計検討 | 分岐点 2 | 分岐 3 | fork 1
├─ 「テストを直して」                                   10:42
│  ├─›現在   「CIを通して」            11:05 | 38件 | 12 KB
│  └─  分岐  「mockで別案」            10:58 | 21件 |  8 KB
├─ 「READMEを更新」                                     11:20
│  ├─  現在  「表を足して」            11:24 |  6件 |  2 KB
│  └─  分岐  「英語版も」              11:31 |  4件 | ⬦ 要約範囲内・復元が必要
└─ ↳ 「別Thread title」 fork · 「テストを直して」から      (dim)
```
- In English: `Branches`, `current`, `branch`, `n items`, `⬦ inside summary — restore first`, `pruned`, `fork from "…"`.
- Nested branches indent with `│  `.
- With ASCII fallback the same layout uses `+-` `\-` `|` `>`.

Narrow layout (< 60 columns): each row is only the tree prefix, the role word and an excerpt. Time, size and state move to the fixed detail area below, as in the Thread browser.

Keys:
- ↑↓ select.
- Enter on a tip switches (confirm popup, then `thread rewind --node TIP`). On a fork, Enter opens that Thread (`o` does the same).
- `f` forks from the tip (`thread fork --node TIP`, stage 4).
- `p` prunes the branch (dry-run facts, then confirm, then `thread prune --branch B`).
- Esc closes. F1 shows help.
- Compare is a later stage: read a tip with `history list --branch B`.

Queries, each bounded:
| Query | Bound and source |
|---|---|
| Branches | Paged, 64 per page, ordered by the anchor's ordinal on the current path (`thread_path` UNIQUE lookup). Nested branches are grouped under their parent through `thread_branch_node_id`. |
| Size | Exact count up to 65,536; above that, shown as "≥65,536". |
| Bytes | Sum of `octet_length` (`gc_bytes_node`) when the tail is ≤ 4,096 nodes; otherwise `不明` / `unknown`. |
| Last request excerpt | Scan back ≤ 256 tail rows; otherwise unknown. |
| Time | The tip node's `created_at`. |
| State | `live`, `pruned`, `anchor off path` (needs restore), or `anchor tombstoned`. |
| Forks | Children via `thread_parent`, up to 32, dimmed. The shared prefix end comes from a binary search on `(thread_id, ordinal)` equality, O(log N) point lookups. If either path was compacted after the fork, the label says "shared prefix now ends at …" instead of naming a fork node that was not recorded. |

No new glyphs: the panel uses `◉ ├─ └─ │ › ↳ ⬦ ⋯`. "Pruned" is a word, not a mark.

### CLI equivalent for the model
`shellboy thread branches --thread T [--limit 50] [--cursor TOKEN]` returns the same rows as structured JSON. Excerpts are up to 512 characters, and every unknown is null with a reason. The cursor is bound to `selection_revision`.

## 5. Interactions

- **Prune and branches.** A replacement whose source closure contains a live branch anchor is skipped and counted (`branch_anchor_skipped`). Removing the branch first is the user's choice. Branch tails are never prune candidates by `--before`; they are released only with `--branch`.
- **Prune and rewind.** A pruned range cannot be restored, so no node in it can become a rewind target.
- **Compaction and branches.** Compaction of the current path over an anchor detaches the branch. The panel shows "restore first", and `compact --restore` re-attaches it. Compaction is not blocked.
- **Fork.** A fork copies only the current path and the replacements on it. Branches are not copied (they are T-local). Fork from a branch tip comes in stage 4.
- **Search.** `search_scope` covers the path plus completed sources only, so cut branch tails leave Thread-scoped search unless the scope CTE adds live branch nodes (open question 6). Pruned sources leave both the scope and the index.
- **Delete.** `thread delete-preview` gains `branch_nodes` and `pruned_replacements` counts.
- **Reasoning.** Truncation never restarts reasoning. A switch needs the skip flag.
- **Cache.** After truncation the replay is a prefix of the old one, but no cache-hit claim is made.

## 6. CLI summary
```text
shellboy thread rewind   --thread T --node NODE
shellboy thread branches --thread T [--limit 50] [--cursor TOKEN]
shellboy thread prune    --thread T (--before NODE | --branch B) [--dry-run]
shellboy thread fork     --thread T --node NODE        # stage 4: NODE may be in a branch tail
shellboy history list    --thread T --branch B         # stage 4: read/compare
```
- Wire names: `thread.rewind`, `thread.branches`, `thread.prune`.
- All use idempotent operation receipts, like the other commands.
- Rewind still refuses while any target Thread has a live Run. #259 removes only the separate queued-input refusal for definitely unadmitted messages; unknown path admission remains a safety stop.

## 7. Risks

- **Reasoning on switch.** The #182 invariant (reasoning strictly before the earliest edited position) must hold when a stored tail is re-selected. The skip flag is correct only if the tail's outputs were produced on exactly that prefix and epoch. The prefix is guaranteed by the anchor. The epoch must be compared, and if it differs the switch restarts reasoning.
- **GC rule change.** Four SQL sites plus root rule 2 are edited. Each needs a regression covering: shared fork kept, nested summaries, pins, interrupted batches.
- **Attachment selection on rewind.** The current rule would still send active attachments whose source is off the path.
- **The full-path rewrite in `select_path` is O(path)** for each rewind or switch, which is the same cost as compaction today. The cost needs to be measured on long threads before anyone optimizes it.
- **Stale restore hints** inside immutable summaries after a prune.
- **Branch count growth.** Each rewind adds one branch. Paging bounds the reads but not the count. That is deliberate: there is no automatic cleanup.

## 8. Staged plan

1. **P1: prune** (`--before`, `--dry-run`, spills excluded, state `pruned`, GC rule, typed restore refusal). It needs no schema and gives the storage value on its own.
2. **R1: rewind and branch storage.** Migration, `thread rewind` truncate only, the result facts, the undo line, the GC root clause, and `/rewind` in the TUI (target taken like `/edit`: the selected request or the last one; the head becomes the node before it).
3. **R2: switch and forward** through the same command, with the reasoning flag and attachment reactivation. Add `thread branches` and the inline marker.
4. **B1: branch panel**, fork dimming and jump, `thread prune --branch`, fork from a branch tip, `history list --branch` (compare).

Each stage is useful alone. P1 does not depend on R1. If R1 lands later, P1 gains the anchor-skip check.

## 9. Open questions for the user
1. Include spills in prune? They hold the largest bytes, but their placeholder tells the model to `history get` the original. Option: `--include-spills` later.
2. Should prune offer an export? Default: no, use `history get` and `readback`.
3. Is the B2 migration acceptable, or is B1 (rewind saves the tail as a fork Thread; no forward in place) preferred for zero schema?
4. Should `/edit` stay "edit and fork", or become rewind in place followed by a send (which then creates an in-thread branch)?
5. Which F-key opens the panel? F1–F10 are bound. F11 and F12 are free in the app but commonly captured by terminals or the OS. Alternatively, use only `/branches` and the F8 entry.
6. Should cut branch tails stay in Thread search scope, or leave it as compacted originals' scope rules would suggest?
7. Command name: `thread rewind` also moves forward. Keep the name, or use `thread switch`?
8. A model asking to rewind its own Thread: refuse (v1), or queue the rewind until the Run ends?
