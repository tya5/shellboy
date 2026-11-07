# Thread Organization: Re-parenting, Archive, Tags, Pin (Study)

Status: **study; stage 1 (archive, migration 32) and stage 2 (re-parent, no schema
change) adopted and implemented** (2026-11-06/07, issue #222). Stages 3-4 are not adopted: nothing in them changes the
adopted design until the user accepts the stage. Code references are to
`origin/main` at `cbd4c76edf`.

Scope: #222 (relax child-Thread restrictions, change a Thread's parent freely)
extended by the user on 2026-11-06 with grouping: folders/groups, pin, and
**archive** (hide from the list without deleting). The user rated archive as
high priority: at work Threads are mass-produced and finished ones must leave
the list without being deleted. The future Web UI (`shellboy-studio`, separate
project, see [events and external protocol](events-and-external-protocol.md))
wants drag-and-drop organization through data-layer operations, and the model
must be able to do the same through the `shellboy` CLI.

Context from the competitive study (local report, not in the repository): Devin
Desktop has folders/groups and child sessions; Claude Code's agent view groups
sessions by state (Pinned / Ready for review / Needs input / Working /
Completed); no surveyed product offers arbitrary re-parenting.

## Summary

- `parent_id` (`thread.parent_thread_id`) is **conversation hierarchy only**. All
  live-execution authority (stop, retry, recovery notification, Run completion,
  wait cycles) follows `run.owner_run_id`. The parent is read at exactly one
  authority point: when a model sends to a Thread **and a new Run starts**, the
  Run is owned by the caller's Run if the caller's Thread is the target's current
  parent. Ownership is then stored and never re-derived.
- Therefore `parent_id` can become mutable without touching any live Run.
  Re-parenting changes only (a) who gets ownership of the *next* Run started by a
  model send, and (b) read-time projections (tree, family, usage with children,
  delete with descendants, "from parent" labels). Every reader already recomputes
  from the current column and already tolerates cycles and missing parents.
- The only new invariant a mutable parent needs is **no cycles**, checked in the
  write transaction by an ancestor walk from the new parent.
- `parent_id` should **not** double as folders. A Thread has one parent; using it
  for filing would pull delegated children away from their delegator (losing
  ownership of their next Runs) and a "folder Thread" is a real Thread that a
  child's progress report (`thread get` → `parent_id` → `message send`) would wake
  and bill. Folders, pin and archive are separate, flat per-Thread attributes.
- **User decision 2026-11-06: no folders.** Parent tree for meaningful relations, tags for placement/cross-cutting groups, automatic grouping by cwd. Stage 3 below is Tags.
- Order (user decision): **Stage 1 archive**, Stage 2 re-parent, Stage 3 tags and cwd grouping.
  **User decision 2026-11-07: no pin.** Ownership transfer is not recommended.

## 1. What `parent_id` does today

| Use | Where | Read when | Effect if parent becomes mutable |
|---|---|---|---|
| Creation defaults: cwd, AGENTS reference, connection, model, effort, compaction overrides copied from the parent | `create_thread`, `src/control/db.rs:1609-1682` | once, at create | none; stays a creation-time copy |
| Fork / `/edit` / rewind child: parent = source Thread, settings and path prefix copied | `fork_thread`, `src/history.rs:469`; [history](history.md) ("fork's parent_thread_id is lineage and creates no Run ownership edge") | once | none |
| **Ownership grant**: model send starting a new Run on a Thread whose parent is the caller's Thread sets `owner_run_id` = caller Run | `assign_message`, `src/control/db.rs:1901-2021` | at each new Run start | the *next* Run follows the new parent; live Runs keep their stored owner |
| Refusal "child Run is owned by another live parent Run" (caller is the parent, target's live Run has another owner) | `src/control/db.rs:1926-1934` | at send | after re-parenting a busy child, the new parent gets this refusal until the old owner's Run ends; correct behavior |
| TUI Thread browser tree (▾/▸, ├─/└─, missing-parent marker, cycle marker) | `src/ui/threads.rs:2051-2120`; [thread browser](thread-browser.md) | each fetched page | already a projection of fetched rows; needs refetch of old and new parent |
| Browser lineage counts (bounded descendant walk, depth 8, 256 rows, cycle flag) | `src/frontend/lineage.rs:12-37`, `affected_tx` `:196-210` | read time | invalidation walks *upward from the changed IDs*; the move must report the old parent as changed too |
| Header / watch / status show parent id and title | `src/frontend/header.rs:97`, `src/frontend/watch_read.rs:22-35`, `src/status.rs:444` | read time | none |
| "From parent" source label on input | `src/frontend/message_metadata.rs:108-116,335` | read time, relative to the *current* parent | old messages from the previous parent lose the label; `provenance.sender` keeps the true Thread ID |
| Usage `--include-children` (descendants ≤200, `scope_complete`) | `src/usage_totals.rs:191-200` | read time; no stored aggregate | the subtree's usage moves with it |
| Delete `--descendants` (unbounded CTE in the delete transaction; guarded preview ≤256, depth 8, cycle → incomplete) | `src/control/db.rs:2643-2650`, `src/deletion.rs:48-52`, `src/deletion/observation.rs:12-33` | in the delete transaction | current tree is deleted; preview marker already turns a moved subtree into `preview_stale` |
| Rewind lists children started after the node | `src/history/rewind.rs:361` | read time | none |
| Branch family (#237): walk up ≤32 ancestors to the topmost live one, breadth-first ≤256; divergence by path bisection; cursor bound to a fingerprint of (Thread, parent) pairs | `src/history/branches.rs:140-200`; [history](history.md#branches-237) | read time | see §3; the cursor already expires when the tree changes |
| Compaction readback "inherited from parent" flag | `src/readback/compact.rs:120,268-275` | read time, relative to current parent | a re-parented fork's inherited summaries would show as its own (display only) |
| Cycle handling | readers stop on revisit (`lineage.rs`, `deletion/observation.rs`, `branches.rs` `seen`) | — | no write-side check exists because parents are never written after insert; one is needed |

Uses of **`owner_run_id`** (not `parent_id`), unchanged by re-parenting:

- stop: no authority check (#256); stop cascades over owned live Runs
  (`stop_run`, `src/control/db.rs`);
- retry: no authority check (#256); the generation CAS makes a race safe
  (`retry_run`, `src/control/db.rs`);
- recovery and completion notices go to the owner Run (`owner_event`,
  `src/control/state.rs:333-355`; `nudge_owner`, `src/control.rs:1086-1100`);
- the parent Run stays live while owning Runs/shells (`src/control/driver.rs:559`);
  stopping it stops owned descendants (`driver.rs:793`);
- model wait cycle check walks the owner chain (`src/control/wait.rs:375`);
- ownership cycles are rejected by triggers (`migrations/0001_initial.sql:57-70`).

**Answer to issue point 2.** Re-parenting can and should leave live Run ownership
untouched. Ownership decides notices and cascade, not permission: the Run that owns a live child Run keeps
recovery and child-end notices (and its stop cascade) until that Run ends; the new parent gains
ownership only of Runs it starts afterwards. "Moving a delegation mid-flight" is
then: re-parent, let the current Run finish (or stop it as its owner/human), and
the new parent's next send owns the next Run. Transferring `owner_run_id` of a
live Run is not recommended: it would have to move owned shells' supervision,
re-evaluate the old owner's completion and pending waits, and re-check the
owner-cycle triggers, for a case the sequence above already covers.

Documentation follow-up (any stage 2): the delegation guide no longer restricts retry or stop (#256); it says that
notices follow the *owning Run*, not `parent_id`.

## 2. Restrictions: needed for safety vs removable (issue point 6)

| Restriction | Protects | Verdict |
|---|---|---|
| Parent fixed at creation | nothing: every consumer reads the current value; ownership is snapshotted in `owner_run_id` | **remove** (stage 2) |
| Ownership granted only when caller Thread = target's parent at Run start | delegation authority is explicit; a model cannot take over a Thread it merely messages | keep; with mutable parents the grant is still an explicit act (someone set the parent) |
| One owner per live Run ("owned by another live parent Run") | single owner, single cleanup path | keep |
| Owner Run stays live while it owns work; stopping it stops owned descendants | no orphaned child Runs or shells ("no persistent daemons") | keep |
| ~~Retry only by the live direct owner or a human~~ | was: avoid two Runs deciding recovery for one child | **removed (#256)**: retry is a generation CAS (`resume_recovery_turn_tx` returns false when the Turn changed), so a race is safe. The user then also removed the stop check: any caller may stop or retry any Run (personal use; the check was not a security boundary) |
| Model message to a child in `recovery_wait` is held | an ordinary message must not silently act as a retry | keep |
| Delete refused while any selected Thread has a live/stopping Run | no deletion under running work | keep |
| Parent must be a live Thread | no dangling new edges | keep for `set --parent` too |
| Readers' bounds (depth 8, 256 members, 32 ancestors, 200 descendants) | bounded reads | keep; they are bounds that report incompleteness, not prohibitions |
| Who may call `thread set` / `thread delete` / `thread create --parent` | — | no caller check exists today (`set_thread`, `delete_thread` take no authority check beyond Origin for target defaulting); a model may already delete any stopped Thread. Re-parenting under the same rule adds no stronger capability, so **no caller restriction** is proposed |

Possible concern: a model could re-parent an independent Thread under itself and
then own that Thread's next Run. The consequence is that its own Run stays live
until that work ends and it can stop/retry it; it gains nothing over sending to
the Thread independently except obligations. Not a safety issue.

## 3. Re-parent design (stage 2)

**Operation.** Reuse `thread.set` with one optional field:

```
shellboy thread set --thread T --parent P     # attach / move under P
shellboy thread set --thread T --parent ''    # detach: independent Thread
```

`""` clears, as `--effort ''` already does (`set_thread`, `src/control/db.rs:1761`).
The field is `skip_serializing_if = "Option::is_none"`, like `title`, so existing
operation fingerprints stay unchanged. It takes the settings-free path (no
`profile_revision`/`reasoning_epoch` change, no settings-change event). It
records a context entry `thread_parent` with before/after, and reports `T`, the
old parent and the new parent in `changed_threads` so the TUI lineage cache and
the Web UI refresh both sides.

**Checks, in the write transaction (single DB worker, so concurrent moves
serialize):**

1. `T` exists and is live; `P` (if given) exists and is live; `P ≠ T`.
2. **Cycle**: walk ancestors from `P` by primary key; refuse with `invalid_state`
   ("would make T its own ancestor") if `T` appears. The walk is a simple chain
   because every write keeps the forest acyclic; bound it at 1024 steps and
   refuse with `input_limit` beyond that, so the write stays bounded. (Existing
   corrupt cycles, which readers already tolerate, also end the walk through the
   bound.) No trigger: the existing ownership triggers raise a generic SQL abort,
   and a typed error from the one write path is simpler. Depth itself is not
   limited; readers already report depth truncation.
3. **No refusal for live Runs or owned work.** Ownership is untouched (§1). The
   result returns the Thread's live Run and its `owner_run_id`, so the caller sees
   that a previous owner still controls the current Run.

**Inherited settings.** Keep creation-time copy only. Re-parenting does not copy
cwd/model/AGENTS from the new parent; use the same `thread set` call to change
them explicitly if wanted. Copying would silently change a Thread's model or
working directory on a drag-and-drop.

**updated_at.** Organization changes (parent, archive, folder, pin) do not bump
`updated_at`, so the list order (by `updated_at`) does not jump on a drag. They
bump `revision` like the title-only path (no compaction or adoption fence reads
`revision`; fences use `selection_revision`, `profile_revision`,
`reasoning_epoch`).

**Branch view.** A branch is "a child that shares a path prefix with its parent"
and the divergence is computed from paths at read time, so nothing is stored to
fix up:

- A fork child moved under another Thread leaves its old family. Under the new
  parent it is shown with a divergence if the paths share a prefix, otherwise
  `divergence_unknown = no_shared_prefix` (the existing state for plain
  `create --parent` children). Inline `↳` markers in the old parent's history
  disappear; that is the meaning of moving it.
- Detaching a fork makes it its own family root.
- The paging cursor fingerprint includes (Thread, parent) pairs, so a move during
  paging returns `cursor_expired` (already specified).
- The fork's shared nodes are unaffected (`thread_path` rows are per Thread).

**Usage.** `--include-children` is read-time, so totals follow the current tree.
Document: "children" means the current subtree, not the Threads created from it.

**Deletion.** `--descendants` deletes the current subtree. Moving a child out first
is a way to keep it. A guarded preview taken before a move fails with
`preview_stale` (existing marker check). Orphans whose parent was deleted can be
detached or re-attached (their `parent_id` still names the deleted Thread).

**Known display effects (accepted, documented):** the "from parent" input label
and the compaction "inherited" flag are relative to the current parent.

## 4. Grouping design

### Evaluation: is `parent_id` already a folder?

Option "folder Thread" (an empty Thread used only as a parent):

- Pros: no schema; tree, collapse, usage with children, delete with descendants
  all work today.
- Cons: a Thread has one parent, so filing a delegated child under a folder
  removes it from its delegator: the delegator no longer gets ownership of the
  child's next Run, and the child's "report to `parent_id`" convention (delegation
  guide) now messages the folder Thread, **starting a model Run there and spending
  tokens**. A folder Thread also needs a configured connection/model at create,
  shows as an idle conversation, and can be mistaken for one by the model. Making
  it safe needs a "container" kind that refuses sends: a new concept anyway.

Conclusion: keep `parent_id` for conversation hierarchy (delegation, branches)
and add filing as separate, flat per-Thread attributes. Each is a column, not a
table.

### Archive (stage 1, high priority)

Storage: `thread.archived_at TEXT` (NULL = not archived). Metadata only: no
history, node, attachment, GC root or `root_revision` change. Archive is not
delete and frees nothing; `thread delete` / `thread prune` / storage collect are
unchanged and archived Threads are ordinary live Threads to GC.

Semantics:

| Surface | Archived Thread |
|---|---|
| `thread list` (CLI, TUI, Web) | hidden by default; `--archived only` lists them, `--archived include` lists both |
| `thread get`, `history get/search`, `status`, `wait`, `usage`, `thread branches`, `message get` | unchanged: readable, searchable, counted. Items gain `archived_at` (branches: `archived: bool`) so viewers can mark them |
| Receiving messages | allowed; a model or human can still send |
| **New Run starts on it** | **clears `archived_at`** (auto-unarchive) and records a context entry `thread_unarchived {reason:"run_started"}`. Messages into a live Run and held `--deliver complete` reservations do not unarchive (a `--deliver complete` or reply on an idle Thread starts a Run and does); only the moment new work actually starts |
| Archiving a Thread with a live Run | allowed for an explicit single target (user's choice); list hides it, `status`/`watch` still show live work. Bulk calls skip Threads with live/stopping Runs and report them as `skipped` |
| Descendants | not implied. `--descendants` selects the current subtree with the existing bounded walk (≤256, reports `scope_complete`); in the TUI tree an archived parent with visible children shows the existing "parent not fetched" marker |

Why auto-unarchive on Run start: archive means "finished, out of my way"; new
work on the Thread means it is not finished, and hidden running work would be
surprising. It is a single deterministic rule in one place (the new-Run branch of
`assign_message`, `src/control/db.rs:2021`), not adaptive behavior. Alternative
(open question): never auto-unarchive.

Operation (one new command, both directions, explicit IDs only):

```
shellboy thread archive   --thread A [--thread B ...] [--descendants]
shellboy thread unarchive --thread A [--thread B ...] [--descendants]
```

`thread.archive {thread_ids: 1..=200, archived: bool, descendants: bool}`. The
result lists `archived`/`unarchived`, `already` (no-op), `skipped` (live/stopping
for bulk, or not found) per ID; idempotent under client operation receipts.

**Bulk archive by age/state/filter** is done by *selecting with list, then
archiving by IDs*: the model reads `thread list` JSON (it already has `state`,
`activity`, `updated_at`, `last_end_message`) and passes IDs; the Web UI
multi-selects. This keeps one bounded write path and makes every archive an
explicit ID set, with no filter-driven writes that could catch unexpected
Threads. Nothing archives automatically. A later opt-in "auto-archive finished
Threads older than N days" could be a setting, but is not proposed now.

List paging: the existing order (`updated_at DESC, id DESC`,
`src/control/db/thread_summary.rs:112-118`, index `threads_updated`) gains the
filter `archived_at IS NULL` (default) or `IS NOT NULL`. With many archived
Threads a residual filter would scan past them, so add two partial indexes:

```sql
-- 0032_thread_archive.sql
ALTER TABLE thread ADD COLUMN archived_at TEXT;
CREATE INDEX threads_visible_updated  ON thread(updated_at DESC,id) WHERE deleted_at IS NULL AND archived_at IS NULL;
CREATE INDEX threads_archived_updated ON thread(updated_at DESC,id) WHERE deleted_at IS NULL AND archived_at IS NOT NULL;
```

The page SQL must repeat the partial-index predicates literally; verify with
`EXPLAIN QUERY PLAN` on the actual statement (as #39 did). `include` keeps
`threads_updated`. The cursor scope string includes the archived mode so a cursor
cannot move between modes.

### Tags and cwd grouping (stage 3) — replaces folders (user decision 2026-11-06)

The user decided against folders: with free re-parenting, nesting is expressed by
the parent tree; flat placement and cross-cutting grouping are tags; project-style
grouping comes from the working directory automatically. Roles are separated so
each concept keeps one meaning:

- **Parent/child** (re-parent, stage 2): meaningful relations only — delegation,
  reports, run ownership of the next Runs, usage `--include-children`,
  `delete --descendants`, branch family. Not used merely to place a Thread.
- **Tags** (many per Thread): placement and cross-cutting views ("customer A",
  "bug hunt"). Needs one small table `thread_tag(thread_id, tag)` with primary key
  `(thread_id, tag)` and an index on `(tag, thread_id)`; tag text validated like
  `title` (one line, trimmed, ≤128 Unicode scalars, no control characters), at
  most 64 tags per Thread. Operations: `thread tag --thread T --add TAG… --remove
  TAG…` (also bulk by up to 200 IDs), `thread list --tag TAG` (repeatable =
  AND; a single OR form only if needed later), `thread tags [--limit --cursor]`
  for distinct tag names (paged ≤200, no unbounded member counts). Tags are not
  inherited at create/fork/rewind by default (placement is the user's choice);
  revisit if delegated trees should carry them. Deleting a Thread deletes its tag
  rows in the same transaction.
- **cwd grouping**: the list can group by the Thread's working directory (no
  storage; `thread list --cwd PATH` filter and a group-by option in the TUI/Web).

Bulk "archive everything with tag X" is `thread list --tag X` then
`thread archive` with those IDs (no filter-driven bulk writes).

### Pin (dropped by the user, 2026-11-07; kept as study record)

`thread.pinned_at TEXT`. Pinned Threads are listed separately, not re-sorted into
the main order (re-sorting would change the cursor key): `thread list --pinned`
returns pinned Threads (bounded page, by `pinned_at DESC`), and the TUI/Web UI
show that page above the normal list. Pin and archive are independent; an
archived Thread is hidden even if pinned (open question: unpin on archive).

### Migrations

One migration per stage, after 31: `0032_thread_archive`, then
`0033_thread_tags` (table `thread_tag`) and `0034_thread_pin` (or one 0033 for both if they land
together). Columns on `thread`, partial indexes as above, `storage/current.sql`
updated. No new table, worker, queue or event kind; changes publish the existing
`Changed` notification.

## 5. CLI, data layer and TUI

Data layer (shared by CLI, TUI and `shellboy-studio` through the existing local IPC):

| Web UI gesture | Operation |
|---|---|
| drag Thread onto Thread | `thread.set {thread_id, parent_id: P}` |
| drag Thread to the root area | `thread.set {thread_id, parent_id: ""}` |
| drag Thread(s) onto folder / new folder | `thread.set {folder}` or bulk by IDs |
| archive / unarchive selection | `thread.archive {thread_ids, archived}` |
| pin / unpin | `thread.set {pinned}` |
| show archived / folder / pinned | `thread.list {archived, folder, pinned}` |

All are ordinary durable operations with client operation IDs (retry-safe), all
emit `Changed` for every affected Thread (including old/new parents), and all are
available to the model unchanged: no extra restriction beyond §2.

`ThreadSummary`/`ThreadDetails` gain `archived_at`, `folder`, `pinned_at`.
CLI reference, Agent Guide and the delegation guide get the new commands and the
"authority follows `owner_run_id`" wording.

TUI (Thread browser, [thread browser](thread-browser.md)):

- Stage 1: an archive key and an "archived" view toggle in the browser; archive is
  reversible, so no confirmation dialog, but a status line with the unarchive
  hint. A "with children" toggle like the delete dialog. Archived rows in the
  archived view and in `/branches` are marked with a word label, not a new glyph
  (new glyphs need user approval). Bulk: multi-select is not required for stage
  1; the model and CLI cover bulk.
- Stage 2: a parent field in the existing F8 `thread set` form (keep / set /
  clear, like title); move-by-key in the browser is optional later.
- Stage 3: a folder filter in the browser (and grouping by folder heading if the
  user wants it); the tree is still projected from `parent_id` inside the
  filtered rows, with the existing missing-parent marker.
- Key bindings are the user's decision.

## 6. Staged plan

1. **Archive** (smallest valuable, highest user priority): migration 0032;
   `thread.archive` (IDs ≤200, `--descendants`, both directions); `thread list
   --archived exclude|include|only`; `archived_at` in summaries; auto-unarchive on
   new Run start; TUI archive key and view toggle; docs. Tests: EXPLAIN for each
   list mode, cursor per mode, bounds (200 IDs, 256 descendants), skipped live
   Threads, auto-unarchive only on Run start, GC unaffected.
2. **Re-parent**: `thread set --parent P|''`; live-parent and cycle checks with the
   1024-step bound; context record; changed old/new parent; F8 form field;
   guide wording. Tests: cycle (self, descendant, deep chain at the bound),
   re-parent with a live owned Run (old owner keeps notices and the stop cascade, new parent
   refused until the Run ends, then owns the next Run), branch view divergence
   after a move, usage and delete following the current tree, preview_stale.
3. **Tags and cwd grouping** (replaces the folder plan; see "Tags and cwd grouping" above):
   many tags per Thread, automatic grouping by cwd, TUI filter.
4. Done (#256): no stop/retry authority check at all.

Not recommended: ownership transfer of live Runs; folder Threads;
filter-driven bulk writes; automatic archiving. Dropped by the user: pin (2026-11-07).

## Stage 1 as implemented (archive, migration 32)

Decisions of the user (2026-11-06) and the resulting contract. This section is the
adopted design for stage 1; the sections above remain the study.

- Column `thread.archived_at TEXT` (NULL = not archived) and two partial indexes
  `threads_visible_updated` and `threads_archived_updated`, both on
  `(updated_at DESC, id DESC)` (the study wrote `id`; `id DESC` matches the
  `ORDER BY` so the page needs no sort). Archive is metadata only: no history,
  node, attachment, GC root or `root_revision` change and no storage freed.
- Wire: `thread.archive` and `thread.unarchive` (two commands with the same args
  `{thread_ids, descendants}` instead of one command with a flag, so each
  operation maps to one CLI path and one F8 form); `thread.list` gains
  `archived: exclude|include|only`; `ThreadSummary` and `ThreadDetails` gain
  `archived_at`. No `thread.organize`; no filter-driven bulk write.
- Selection: up to 200 explicit IDs (`input_limit` beyond); omitted IDs target the
  calling Thread like the other Thread commands. With `descendants` the bounded
  recursive walk over live Threads selects the current subtree; more than 256
  Threads in all is refused with `input_limit` and nothing changes (a write must
  not be silently partial). Unknown or deleted IDs are reported in `not_found`;
  none found is `thread_not_found`.
- A live Run does not block archiving (user decision; the Run continues and
  `status`/Watch still show it), so the study's "skipped" list does not exist.
- Writes set `archived_at` and bump `revision` only; `updated_at` is untouched so
  the list order does not move. Changes publish `changed_threads` for the changed
  Threads.
- Auto-unarchive: the single new-Run branch of `assign_message` clears
  `archived_at` and records a `thread_unarchived` context entry
  (`{reason:"run_started"}`). Only a message that actually starts a Run does: a message into an existing live Run, a held one, or a reply that is only queued does not. `--deliver complete` on an idle Thread is assigned `new_run` immediately, so it does unarchive; against a live Run it is held until the Run ends and does not.
- List cursors are bound to the archived mode through the scope string; a cursor
  from another mode is `cursor_expired`.
- TUI: Ctrl+A archives (with descendants, no confirmation: reversible) or, in the
  archived list, unarchives; Ctrl+G toggles the archived list. See
  [thread browser](thread-browser.md). Watch is unaffected.
- Not done: pin, tags (stages 3-4), automatic archiving, archive state
  in `/branches`, and the archived marker in the TUI header.

## Stage 2 as implemented (re-parent, no schema change)

Follows §3 with these concrete choices (decision D-222-2).

- Wire: `thread.set` gains `parent_id` (`ThreadSetArgs`, skipped on the wire when absent). A
  Thread ID moves the Thread under that Thread; the empty string detaches it. CLI
  `thread set --parent ID|''`; F8 `会話 | 設定` field `親会話` (empty = unchanged, `none` = no
  parent). The field combines with the other `thread set` fields in one transaction (the
  move runs first; any later failure rolls it back). No new command, table, worker or queue.
- Checks run in the write transaction on the single DB worker (`src/control/db/reparent.rs`):
  the Thread and the new parent exist and are not deleted (`thread_not_found`); a Thread cannot
  be its own parent and the new parent cannot be inside the Thread's own subtree
  (`invalid_state`); the ancestor walk from the new parent follows primary keys for at most
  1,024 steps and a longer chain, including a stored cycle above the parent, is `input_limit`.
  The walk passes through deleted ancestors too, so the check is conservative. Archived Threads
  can be parents. There is no caller restriction (§2). Setting the current parent again is a
  no-op: no revision bump, no context entry, nothing published.
- Effects: `parent_thread_id` and `revision` change; `updated_at` does not. A
  `thread_parent` context entry records `{before:{parent_id}, after:{parent_id}}` and is shown
  like a settings change (same glyph, no new one). Inherited settings are not copied.
- Live work: nothing is touched. `run.owner_run_id`, shells, waits and pending replies are
  unchanged; replies follow the receipt's recorded sender, and wait cycles follow the owner
  chain, neither reads `parent_id`. The result's `parent_change` reports the previous parent
  and the Thread's live Run with its owner so the caller sees who still controls it. The new
  parent is refused with "child Run is owned by another live parent Run" until that Run ends;
  its later send then owns the next Run. The old owner keeps the notices and the stop cascade (any caller may stop or retry a Run).
- Change hints: `changed_threads` is the moved Thread, the old parent and the new parent, in
  that order (deduplicated), so lineage caches and `events.wait` clients refresh both sides.
- Bounds: the walk (1,024 steps) is the only loop; one UPDATE and one context row per call;
  `thread.set` stays in the writer-bound exemption list because the call is O(1,024) at most.
- Not done: ownership transfer of live Runs (stop/retry have no authority check since #256, so a move changes only notices and the stop cascade), a TUI move key
  or drag gesture, moving several Threads in one call (use one call per Thread; the Web UI
  can repeat it).

## Risks

- **Hidden work**: an archived Thread with a live Run is hidden from the list;
  mitigated by bulk skip, `status`/`watch`, and auto-unarchive on new Runs.
- **Misread authority** after a move: the new parent sees "owned by another live
  parent Run" until the old Run ends. Mitigated by returning `owner_run_id` from
  `thread set` and by the guide wording.
- **Lineage cache staleness** in the TUI if the old parent is not reported as
  changed; covered by reporting both parents.
- **Deep trees** from moves exceed reader depth bounds (8) more often; readers
  already report truncation, and guarded delete-with-descendants then requires
  smaller selections.
- **Label drift**: "from parent" and "inherited summary" labels follow the
  current parent.
- **List cost**: a residual `archived_at` filter without the partial indexes would
  scan archived rows; the indexes are part of stage 1.

## Open questions for the user

1. Auto-unarchive when a new Run starts (recommended), or archived stays archived
   until explicitly unarchived?
2. TUI archive "with children": default on (reversible, mass-produced worker
   trees) or off (consistent with delete)? CLI stays explicit `--descendants`.
3. Archive a Thread with a live Run: allow for single explicit targets
   (recommended) or refuse always?
4. One bulk command for all organization attributes (`thread.organize`) or
   separate `thread archive` plus `thread set` per attribute (recommended for
   stage 1)?
5. Folders: flat single folder per Thread (recommended) enough, or do you want
   nesting or multiple tags?
6. Pin: needed at all, given Claude Code's "Pinned" group, or is archive plus
   folders enough? Should archive unpin?
7. Retry authority: decided (#256), no check for stop or retry.
8. Should organization changes leave `updated_at` untouched (recommended, list
   order stays stable on drag), unlike the current title change?
9. TUI keys for archive, archived view and folder filter.
