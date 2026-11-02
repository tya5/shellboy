# Mid-Turn Middle-Segment Summary (#181, option B)

Status: **Proposed, awaiting leader review. Not adopted for implementation.**
This memo makes the prerequisites of option B in
[midturn-compaction-options.md](midturn-compaction-options.md) concrete. It does
not change any adopted contract until approved. Baseline: [compaction](compaction.md)
(#125 trigger, #153 retry gate, M2/M11 boundaries, #73/D-017 summary bounds),
[replay cache](replay-cache.md) and [model HTTP](model-http.md) dialects. No
code was changed; sources were read at `origin/main` 777c100d.

## Why reconsider now

The deferral condition ("a real example continues growing substantially beyond
the threshold") was met by a read-only measurement of a copy of the user's DB on
2026-11-03 (provider-reported values; not reproduced here from private data):

- With proactive compaction on, 59% of luna, 12% of terra and 17% of sol
  requests reported more than 272K input tokens; some Threads reached 888-921K.
- Proactive compaction cannot shrink a long Turn: on the next request the input
  was still at or above 200K in 83% (luna) and 31% (terra) of cases (#180).
- Cost of the above-272K requests, computed at the short-context rate as a lower
  bound: sol $847, terra $331.
- The first request after a compaction had a cache hit ratio of 32% (terra) and
  6% (sol).

The user approved pursuing B on 2026-11-03 (#181). These numbers motivate the
change; they are not a prediction of B's savings.

## Summary of the proposal

At the existing #125 boundary, after the existing spill, Shellboy may replace
**one old contiguous run of whole groups inside the active Turn** with one
summary node, using the existing explicit middle-range summary adoption
(`summarize_recorded` + `SummaryAdoption{start,count}`). The current request,
every later user instruction, and the recent working suffix stay verbatim. The
candidate is skipped, never guessed, when the active adapter cannot replay the
retained reasoning after the replacement. No table, column, migration, worker,
queue, event, timer or configuration item is added.

## 1. Segment selection (structural)

Inputs are the existing summary `Snapshot` (selected path split into groups by
`source_groups`, which already keeps a group's first call through its last
result, including parallel calls and interleaved audit sources, as one group).

Fence groups (never inside the segment):

1. any group that is not complete (`Group.complete == false`), i.e. an open,
   running or partially answered call of any sibling;
2. any group containing a protected node (accepted/held receipt message,
   current-Run revisit event), as `range::group_is_safe` already defines;
3. any group containing a user-role node of the current Turn: the request group
   `R = policy::request_boundary(current_turn)` and any user message delivered
   later in the same Turn;
4. the latest complete tool group `L` (as in `policy::safe_prefix`).

**Recent working suffix** = `L` and every group after it. It is defined by
structure only (the latest complete tool group onward), not by tokens,
characters or a group count. It is nonempty whenever a candidate exists because
`L` must exist after the segment.

**Candidate** = the oldest maximal run of consecutive non-fence groups with
index strictly between `R` and `L`, that contains at least one tool group or
spill marker. If there is no current Turn, no `R`, no `L`, or no such run, B has
no target. Prefix groups before `R` (earlier Turns) are never part of B; they
remain the ordinary summary's scope.

Closure follows from whole groups: a covered assistant response takes all its
calls and all their results; no retained result references a covered call and
vice versa. `range::validate(.., RangeMode::Summary)` is run on the candidate
as today, so the existing validator remains the single authority.

A previous B summary node is an ordinary non-tool group without `turn_id`; it
can be covered again by a later candidate (nested summary, already exercised by
`nested_summary_and_spills_preserve_current_turn_boundary`). A run made only of
that node has no tool/spill group and is therefore not a target, which also
prevents re-summarizing a summary alone.

## 2. What stays verbatim

- System prompt, tools, Turn instruction snapshot and every group before `R`
  (prior Turns, prior summaries).
- The current request group `R`, every later current-Turn user message group,
  protected receipts/revisit events, and their attachments (attachment
  exclusion applies only to covered sources, as for every replacement).
- The recent working suffix (`L` onward), byte-identical in replay: reasoning
  items with their IDs and `encrypted_content`, opaque blocks, call IDs.
- Off-path queued messages (unchanged #52 behaviour).

## 3. Summary request

Reuse `summarize_recorded` unchanged in its bounds:

- Input: the covered groups rendered by the existing `render` (current-epoch
  reasoning summaries only, `encrypted_content` scrubbed, opaque blocks never),
  split into whole-group map segments of at most 64,000 Unicode characters; an
  indivisible group above that is the existing `InvalidState` rejection.
- Request limits: `summary_map_max_requests` (default 128, preflighted before
  any HTTP), `summary_reduce_max_requests` (default 16), at most four reduce
  rounds. Context-limit bisection depth 8 as today.
- Output limit: `min(summary_max_chars, summary_max_percent of source)`, floored
  at 2,000 characters (defaults 20,000 and 5%). Over-limit output is rejected,
  never truncated.
- Rejected: empty text, typed refusal, over-limit output, and any candidate
  whose summary node projection is not smaller than the covered source
  projection (existing local shrink check; not a token claim).
- Prompt and cache keys: existing `SUMMARY_PROMPT` and `summary_key`, so cached
  map parts are reused. Usage is recorded by #133 unchanged.
- The adopted node's `item` text is prefixed with one fixed English line, so the
  model does not repeat covered work: "Shellboy context notice: summary of
  earlier work in the active Turn replacing N source nodes (FIRST through LAST).
  Tools it describes were already executed and their side effects remain;
  inspect current state before repeating an operation. Originals: shellboy
  history get --node <source_node_id>." One out-of-band field
  `summary_scope: "active_turn"` is added to the payload (not sent).

On any failure the selected path and originals are untouched; summary parts
already saved remain ordinary cache records.

## 4. Reasoning replay contract per adapter

Let **retained reasoning** be the items replay would send from groups after the
replacement point (indices `>= segment.end`) that are current-epoch `reasoning`
items or current-epoch `provider_opaque` items of the connection's dialect
(items of other epochs or dialects are already dropped by `replay::convert` and
the encoders). Covered reasoning disappears together with its whole assistant
response; nothing is spliced, truncated or re-signed.

If retained reasoning is empty, the result is a plain structural edit on every
adapter (same as existing summary/omit markers) and B may proceed. Otherwise:

| Adapter (`api`) | Retained reasoning present | Source |
| --- | --- | --- |
| `chat_completions` | Never sent: the Chat encoder skips `reasoning` and all opaque items; vendor `reasoning_content` is unused. **Supported.** | model-http.md #64 |
| `responses` | **Skip** until the paid validation below passes and is recorded. Documented: reasoning items should be passed back, and when truncating, "all items between the last user message and your function call output" should be passed "untouched"; reasoning items irrelevant to the functions are ignored. Not documented: whether retained encrypted reasoning after a removed same-Turn segment is accepted or used. A developer-role summary is not a user message, so B does not satisfy the documented guidance literally. | [OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning) (already cited by replay-cache.md M8), read 2026-11-03 |
| `anthropic_messages` | **Skip.** Documented: within a tool-use turn thinking blocks must be passed back complete and unmodified; on Claude 5.1+ every `thinking`/`redacted_thinking` signature is checked against everything sent before it (system, tools, earlier messages); editing or deleting an earlier message, or removing a thinking block from the middle, invalidates every later block; keep-tail client compaction is documented to fail this check; the unset default is a 400 for accounts created on or after 2026-08-31. Shellboy has no model catalog and sends no `block_binding`, so it cannot select a passing case. Unknown opaque kinds are skipped as unknown. | [Thinking](https://platform.claude.com/docs/en/build-with-claude/thinking), [Preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking), read 2026-11-03 |

Implementation: one pure function next to the dialects, e.g.
`dialect::midturn_replay(api, retained) -> Result<(), SkipReason>`, with the
Responses arm as a single constant flipped only in the commit that records a
passing validation. It is not a setting. The predicate reads only snapshot
payloads, which the adoption fence already hashes, plus the connection `api`,
whose change advances the profile revision/epoch and fails the fence.

Skip is local: no summary request is sent and the existing path continues (see
section 7). The skip reason is recorded in the attempt record.

## 5. Storage and provenance

The existing machinery already supports a middle range: explicit
`compact --summary --from --to` adopts with `SummaryAdoption{start, count}`,
rebuilding the selected path as prefix + summary node + suffix, and
`explicit_middle_summary_retains_prefix_suffix_and_exact_provenance` covers it.
`store_replacement` writes the summary node, `replacement`, ordered
`replacement_source` rows with revision/hash, `thread_replacement`, and the
attachment exclusions in the same transaction.

Turn boundaries survive: the request node keeps its `turn_id`, so
`request_boundary`/`turn_boundary` find it directly; the NULL-turn summary node
after it is handled by the M11 reverse lookup only when needed. GC already
protects originals through `replacement_source`.

Minimal change: none to schema. **No migration.** Only the payload gains the
out-of-band `summary_scope` field and the item text prefix (section 3); the
attempt record gains a `midturn` object (section 7). `compact --restore` works
only for omission markers today and is unchanged for summaries; originals stay
readable through history get/details and fork.

## 6. Atomic, fenced adoption

Reuse `SummaryAdoption::adopt_tx` in one DB transaction, in the existing order:
cancellation check, `validate` (Thread selection revision, profile revision,
reasoning epoch, live Run/Turn generation fences, every source revision and
payload hash including the suffix), replacement/provenance/attachment
exclusion, `select_path` CAS, commit. Any race, cancellation, provider error,
rejection or storage failure leaves no partial summary. A spill committed
earlier in the same attempt keeps its existing #153 semantics.

Tests (mock/synthetic only):

- policy table: candidate after `R`, ending before `L`; parallel calls inside a
  covered group; incomplete group, held receipt, revisit event and a mid-Turn
  user message as fences; nested summary only; no current Turn; no `L`.
- adapter predicate: Chat with reasoning present; Responses with current-epoch
  reasoning in the suffix (skip) and only in covered groups or older epochs
  (proceed); Anthropic `thinking`, `redacted_thinking` and unknown kind in the
  suffix (skip), only covered (proceed), other dialect (ignored).
- Compactor with the loopback summary proxy: path becomes `[.., R, S, L..]`,
  exact ordered `replacement_source`, originals unchanged; non-shrinking,
  over-limit, empty, refusal, cancellation, late selection change and storage
  failure keep the path; attachment exclusion only for covered sources.
- replay: post-adoption lease items equal prefix + one developer item + the
  suffix byte-identical (reasoning IDs, `encrypted_content`, call IDs); replay
  cache rebuilds; tool executor count unchanged (no re-execution); each
  dialect encodes the result (Anthropic tool_result-first merge, Chat tool
  messages after their call).
- readback: `compact list` shows the automatic reduction with the scope;
  history get returns the covered originals.
- fork: forking after adoption yields the same replay; provenance resolves in
  the child; GC keeps originals; forking from a covered original works.
- driver: in a synthetic long Turn with fabricated reported usage over the
  threshold, B adopts and the next request's sent input is smaller (local
  serialized characters, not tokens); stale usage does not re-trigger; #153
  gate and #180 progress rule hold.

## 7. Trigger and #180

Trigger is unchanged #125: the latest completed attempt's provider-reported
`input_tokens` on the current connection/model, not stale, at or above
`auto_at_tokens` (connection override and 0 opt-out unchanged). One coordinated
attempt per boundary in `driver/proactive.rs`:

1. spill as today;
2. if the B candidate exists and the adapter predicate passes, run B **instead
   of** the ordinary summary in this attempt; otherwise run the ordinary
   summary as today. One summary operation per attempt; record
   `midturn: {"scope":"active_turn"}` or `{"skipped":"no_target"|"adapter:<api>"}`
   inside the existing `automatic_trim_attempt` JSON.

Outcomes map to the existing `adopted`/`rejected`/`no_safe_reduction` and the
same `ProactiveBudget`, so the #153 growth gate (`ceil(auto_at_tokens/10)`) and
the post-adoption fresh-measurement rule apply unchanged. For #180, the
candidate predicate is itself a stored-fact progress signal: after an adoption
the run is `[S]` plus new groups, and it becomes a target again only when a
newer complete tool group exists before `L`. #180's "eligible content grew"
check should include "B candidate became non-empty" so a long Turn without new
groups never re-fires. Adapter skip sends nothing and is not a paid loop.
No configuration item is added.

## 8. Cache impact

Each adoption changes the request from the replacement point onward: one prefix
discontinuity per adoption, directly after the current request (plus any
same-attempt spill position, whichever is earlier). Everything before it
(system, tools, prior Turns and their summaries, the request) is unchanged and
remains a candidate for provider cache reuse; whether it hits is
provider-dependent and is measured, not assumed. The measured 32%/6% hit ratios
are after today's compactions, which can change the path near its start; B's
point is later, but no improvement is claimed until the validation records
`cached_tokens`.

Weighing: without B a long Turn sends the full grown input on every request
(59%/12%/17% of requests above 272K; at least $847 and $331 for sol and terra
above 272K at the lower-bound rate). With B, per adoption Shellboy pays the
summary requests (bounded by section 3, recorded by #133) and one uncached
rewrite of the summary plus suffix, after which requests grow from a smaller
base until the threshold triggers again. The number of adoptions in a Turn is
bounded by measured growth: each requires a fresh reported input at or above
the threshold. Repeated adoptions repeat the discontinuity and nest summaries
(lossy); this is the accepted trade-off.

## 9. Real-provider validation plan (paid; needs the user's approval)

Separate from mock tests; a mock success is never reported as provider success.
Run with a temporary data directory, never the user's DB, on a validation
branch whose Responses constant is enabled. Use the existing per-connection
`auto_at_tokens` override and `summary_map_max_requests = 4` as test
parameters (existing settings). One scripted Turn: the model runs a fixed list
of harmless read-only shell commands on synthetic files (including one
parallel-call step) until B adopts, then two more tool rounds and a final
answer.

Bounds per provider: at most 20 conversation requests and 4 summary requests
per run, at most 2 runs (second only after a transient failure); stop at the
first rejection.

| Provider | Case | Pass criteria |
| --- | --- | --- |
| Responses, the user's model behind the measured data (one model; a second only if the user asks) | B adopted with retained reasoning | every post-adoption request succeeds with no invalid-input error; first post-adoption reported `input_tokens` is lower than the triggering request's; model continues and finishes the Turn; no saved tool re-executed; `cached_tokens` recorded (observation, not pass/fail); whether the model re-issued a covered command recorded |
| Anthropic Messages | thinking disabled via `extra_body` | same as above with no retained thinking |
| Anthropic Messages | default thinking on a 5.x model | B skipped, zero summary requests from B (no paid B traffic beyond the conversation itself) |
| Chat Completions | only if the user has such a connection | same as Responses row |

Failure keeps the Responses constant at skip. Record environment, model,
dates, request counts, reported usage and outcome in
[acceptance](../validation/acceptance.md).

## 10. Implementation size (estimate)

| File | Change | Estimate |
| --- | --- | --- |
| `src/compaction/policy.rs` | `midturn_range` + table tests | ~40 + ~100 test lines |
| `src/model/dialect.rs` | `midturn_replay` predicate + tests | ~35 + ~80 |
| `src/compaction.rs` | `summarize_midturn` entry, prefix line, `summary_scope` | ~45 |
| `src/control/driver/proactive.rs` | choose B or ordinary, record `midturn` | ~25 |
| `src/readback/compact.rs` | expose scope in metrics | ~3 |
| tests (compaction module / `overflow_tests`-style driver test) | adoption, replay, fork, readback, driver | ~350 |
| docs | compaction.md section, status, acceptance | n/a |

About 150 production lines and 500 test lines; no schema, config, table,
worker or dependency.

## Open questions for review

1. Responses ships disabled until validation; until then only Chat (and
   Anthropic without thinking) benefit. Accept, or validate before merging?
2. Summary role: developer (existing shape, chosen) vs user, which would match
   OpenAI's "since the last user message" wording but may read as a new
   instruction. Not proposed without evidence.
3. B covers spill markers, so the summarizer sees placeholders, not original
   outputs (same as ordinary summary). Expanding originals would improve
   fidelity at higher summary cost.
4. Prefer B over the ordinary summary in an attempt (proposed) or run both
   (two adoptions, earlier discontinuity)?
5. Existing risk outside B: on Anthropic Claude 5.1+ with thinking, ordinary
   prefix summary, `compact --summary` middle ranges and #50 omission also edit
   messages before retained thinking blocks and may 400. Track as a separate
   issue; the same predicate could guard them.
