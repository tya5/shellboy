# Mid-Turn Middle-Segment Summary (#181, option B)

Status: **Adopted and implemented (2026-11-03)** after review, the survey and the
GPT-route validation. **Superseded in part by #182 (2026-11-03):** the segment,
retained suffix, epoch rule and trigger order in sections 1-4 and 7 are replaced
by [compaction #181/#182](compaction.md#active-turn-middle-summary-181-reshaped-by-182)
(all tool rounds replaced, the latest round as text, no epoch advance, ordinary
summary first). #215 supersedes that scope: automatic mid-Turn summary chooses
oldest-first whole safe groups only until the remaining calibrated removal need
is met, keeping the latest complete tool group and the later suffix. Adoption
atomically advances the Thread reasoning epoch and clears the kept set, dropping
all earlier reasoning. The original request, kept user instructions, unreflected
input, incomplete groups and protected nodes remain fenced; the summary keeps
its User instructions to keep section. Record remaining need before and after.
The evidence and cache sections remain as history. Adopted contract summary: [compaction](compaction.md#active-turn-middle-summary-181).
Implementation differences from this memo: the readback `compact list` metric
for the scope is not added (the `midturn` object is in the saved attempt record),
and the function is named `active_turn_range`/`summarize_active_turn`.
This memo makes the prerequisites of option B in
[midturn-compaction-options.md](midturn-compaction-options.md) concrete. It does
not change any adopted contract until approved. Baseline: [compaction](compaction.md)
(#125 trigger, #153 retry gate, M2/M11 boundaries, #73/D-017 summary bounds),
[replay cache](replay-cache.md) and [model HTTP](model-http.md) dialects. No
code was changed; sources were read at `origin/main` 777c100d.

## Review decisions (2026-11-03)

1. Drop reasoning is the single default for every adapter: an adopted B
   replacement starts a new reasoning epoch, so no previously saved reasoning
   or opaque/thinking block is sent afterwards (close to option D). Keeping
   reasoning on Responses is a possible later improvement, out of scope. An
   adapter with no documented compliant shape skips (section 4).
2. The summary is a developer item; placeholders-only fidelity for spilled
   groups is accepted; B runs instead of the ordinary summary in an attempt.
3. Paid validation only through the user's GPT subscription route (`litellm`).
   No Claude or OpenRouter request; Claude is recorded as not validated
   (section 9).
4. Implementation was confirmed after the survey and landed on this branch.
5. Survey (2026-11-03): Codex, OpenCode and Goose compact mid-Turn and carry no
   reasoning across it. In Codex (OpenAI's client, `openai/codex` at 40cf1685),
   `codex-rs/core/src/session/turn.rs` (around L688-735) runs auto compaction
   with `CompactionPhase::MidTurn` and continues the Turn, and
   `codex-rs/core/src/compact.rs` (around L400-420) rebuilds the history from
   the user messages plus the summary. As a result Claude moves from "skip" to
   "supported by documentation, not live-validated".

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
adoption advances the Thread's reasoning epoch in the same transaction, so all
reasoning saved before it is no longer sent, on every adapter (section 4). No table, column, migration, worker,
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

## 3. Summary request (historical proposal; superseded by #303)

The former multi-part path below is retained as design history only. #303 removes
map/reduce and the percentage output cap; current behavior is the single-request
contract in `docs/design/compaction.md` and the supported absolute
`[compaction] summary_max_chars` owner default. This proposal is not current.

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

## 4. Reasoning: drop by new epoch, per-adapter compliance

**Rule (all adapters).** `SummaryAdoption::adopt_tx` for a B candidate also runs
`UPDATE thread SET reasoning_epoch = reasoning_epoch + 1` in the same
transaction (after `validate`, which still checks the old epoch). Existing
`replay::convert` sends `reasoning` items and `provider_opaque` items only from
nodes whose saved epoch equals the Thread epoch, and the replay cache rebuilds
on an epoch change. From the next request on, therefore:

- **Dropped:** every Responses `reasoning` item (summary and
  `encrypted_content`) and every `provider_opaque` item (Anthropic `thinking`,
  `redacted_thinking`, unknown kinds) saved before the adoption, in the retained
  suffix *and* in earlier groups and Turns. Summary material already excludes
  them.
- **Kept:** messages, `function_call`, `function_call_output`, spill
  placeholders, summaries, events and attachments, unchanged. Saved nodes are
  not modified; the dropped items remain in history and readback.
- **New:** responses after the adoption carry the new epoch and are replayed
  normally, so reasoning continuity resumes from that point.

The epoch is Thread-wide rather than positional because that reuses the
existing machinery without new replay logic and composes with model-change
epochs. The cost is that earlier-Turn reasoning is dropped too (see section 8).
No orphan can arise: every reasoning item goes, and no call or result is
removed by the drop.

**Per-adapter compliance after the drop.**

| Adapter (`api`) | Shape sent after adoption | Decision | Basis |
| --- | --- | --- | --- |
| `chat_completions` | Unchanged apart from the summary; the encoder never sends reasoning/opaque items. | Proceed. | model-http.md #64 |
| `responses` | No reasoning items at all; function calls and outputs paired. | Proceed. | [OpenAI reasoning guide](https://developers.openai.com/api/docs/guides/reasoning): passing reasoning items back is "highly recommend[ed]", not required. Validated through `litellm` on 2026-11-03 (section 9). The guide's "untouched since the last user message" advice is knowingly not met: that loss is the summary. |
| `anthropic_messages` | Summary is sent as user text; every thinking/redacted_thinking block saved before the adoption is omitted, including those of retained assistant messages. | **Proceed: supported by documentation, not live-validated** (no paid Claude run, by instruction). | [Preserved thinking](https://platform.claude.com/docs/en/build-with-claude/preserved-thinking): each block records the block before it; removing blocks from the start, the end or all of them is valid, and only removing a block from the middle while keeping later ones invalidates them. Kept thinking must be an unbroken run of the original blocks. Dropping everything is the "all" case, so no signature fails the Claude 5.1+ prefix check. Residual, documented but not observable without a model catalog: the tool-use rule ("when you return tool results, you must pass the thinking blocks ... back") and manual mode's "final assistant turn must start with a thinking block". The docs say mid-turn conflicts "degrade gracefully" (thinking is disabled for that request, no error); manual `type: "enabled"` is only reachable through user `extra_body` and is rejected on Claude 4.7+. |

No adapter skips on reasoning. The only skips are a missing candidate and the
existing failure paths. Should a live Claude failure appear later, the fallback
is a skip predicate on Anthropic `provider_opaque` items in the active Turn
(about 20 lines); it is not implemented now. #182 tracks the separate risk
that the *existing* edits keep thinking blocks after an edited prefix.

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

Minimal change: none to schema. **No migration.** The adoption transaction
additionally advances `thread.reasoning_epoch` (existing column), the payload
gains the out-of-band `summary_scope` field and the item text prefix (section
3), and the attempt record gains a `midturn` object (section 7). A fork copies
the parent's current epoch, as today. `compact --restore` works
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
- epoch drop: after adoption the Thread epoch is +1; the next lease contains no
  reasoning/opaque items from before the adoption (suffix and earlier Turns)
  and does contain those of a later response; a failed or late adoption leaves
  the epoch unchanged.
- encoders after the drop: Anthropic request contains no thinking blocks and
  keeps tool_result-first ordering; Responses contains no reasoning items; Chat
  unchanged.
- Compactor with the loopback summary proxy: path becomes `[.., R, S, L..]`,
  exact ordered `replacement_source`, originals unchanged; non-shrinking,
  over-limit, empty, refusal, cancellation, late selection change and storage
  failure keep the path; attachment exclusion only for covered sources.
- replay: post-adoption lease items equal the prefix and the suffix without
  reasoning/opaque items, plus one developer item; all other suffix items
  byte-identical (call IDs, outputs); replay cache rebuilds; tool executor count unchanged (no re-execution); each
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
2. if the B candidate exists, run B **instead
   of** the ordinary summary in this attempt (decided); otherwise run the ordinary
   summary as today. One summary operation per attempt; record
   `midturn: {"scope":"active_turn"}` or `{"skipped":"no_target"}`
   inside the existing `automatic_trim_attempt` JSON.

Outcomes map to the existing `adopted`/`rejected`/`no_safe_reduction` and the
same `ProactiveBudget`, so the #153 growth gate (`ceil(auto_at_tokens/10)`) and
the post-adoption fresh-measurement rule apply unchanged. For #180, the
candidate predicate is itself a stored-fact progress signal: after an adoption
the run is `[S]` plus new groups, and it becomes a target again only when a
newer complete tool group exists before `L`. #180's "eligible content grew"
check should include "B candidate became non-empty" so a long Turn without new
groups never re-fires.
No configuration item is added.

## 8. Cache impact

Each adoption changes the request at one point per adoption: the earliest of
the replacement point (directly after the current request), any same-attempt
spill, and, because of the epoch drop, the first reasoning item still replayed
from earlier groups. Whether dropping earlier-Turn reasoning changes the
provider's rendered prefix depends on the provider (OpenAI documents that some
models do not render earlier-turn reasoning); it is measured, not assumed.
Items before that point remain a candidate for provider cache reuse; whether it hits is
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

## 9. Real-provider validation (2026-11-03, approved, GPT subscription only)

Scope and approval: per the user's instruction, only the `litellm` connection
(ChatGPT backend through LiteLLM), model `gpt-5.6-luna` (the configured
default). No Claude, `litellm-claude`, `litellm-user` or OpenRouter request.
A fresh temp data dir held a config with only the connection's `base_url`
copied (no key exists for this connection), plus `auto_at_tokens = 0` and
`summary_map_max_requests = 4`. It was deleted afterwards; nothing was printed
or committed. Binary: this branch at 777c100d + docs (no code change), debug
build. B itself is not implemented, so the B shape was produced as follows:

1. Baseline: `shellboy exec` ran one Turn through the real Responses adapter,
   reading six synthetic files (`cat f1.txt` ... `f6.txt`, one call each) and
   answering their sum.
2. Summary: the existing explicit middle summary
   (`compact --summary --from --to`) over the groups B would cover, a real
   map + reduce request pair.
3. B shape: `thread request --attempt A --body` exported the exact wire body of
   the request after the latest complete tool group's result (`unknown: []`).
   A harness replaced the items after the user request up to the covered
   group's last result with one developer item (fixed notice + summary text),
   removed **every** `reasoning` item, checked call/result closure, and POSTed
   the body to the same endpoint. It then continued the task: it read the
   requested file locally for any `cat fN.txt` call and stopped at the answer.

| Run | Covered / retained | Reasoning dropped | Result |
| --- | --- | --- | --- |
| 1 (default effort) | f1-f4 covered (13 items incl. 4 event notices); f5 group retained; attempt 6 body | 1 (covered only; the retained group had none) | HTTP 200 `completed`; model called `cat f6.txt`, then answered `{"sum":3369}` (correct) |
| 2 (Thread effort `high`) | f1-f5 covered (15 items); f6 group retained; attempt 14 body | 2, including the **retained** f6 response's reasoning item | HTTP 200 `completed`; model answered `{"sum":3369}` (correct, using the summary's values) |

Both baselines also completed with the correct sum. Requests and
provider-reported usage (tokens), cost at the given luna prices
($0.10 input / $0.01 cached input / $0.50 output per 1M):

| Requests | Input | Cached | Output | Cost |
| --- | --- | --- | --- | --- |
| run 1 baseline, 7 conversation | 47,549 | 8,960 | 237 | $0.004067 |
| run 1 summary, 2 (map + reduce) | 10,527 | 2,816 | 1,280 | $0.001439 |
| run 1 B shape, 2 conversation | 8,962 | 0 | 77 | $0.000935 |
| run 2 baseline, 7 conversation | 47,754 | 10,752 | 256 | $0.003936 |
| run 2 summary, 2 (map + reduce) | 12,264 | 0 | 2,176 | $0.002314 |
| run 2 B shape, 1 conversation | 4,171 | 0 | 76 | $0.000455 |
| **total 19 (15 conversation + 4 summary)** | **131,227** | **22,528** | **4,102** | **$0.013146** |

Within bounds: 2 runs; 9 and 8 conversation requests, 2 summary requests per
run; no rejection. The subscription route's real billing is not per token; the
figure applies the given prices to the reported usage.

Conclusions and limits:

- Responses through this route **accepted** the drop-reasoning B shape and the
  model continued the task correctly, including when the retained response's
  own reasoning item was dropped. This is one route, one model, short synthetic
  Turns; it is not evidence for other Responses endpoints, models or long Turns.
- The B-shape input was smaller than the corresponding baseline request (run 1:
  3,999 vs 9,210; run 2: 4,171 vs 10,469), as expected from the shape. This is
  not a projection of savings on long real Turns.
- Cache was **not measured**: the harness sent only `content-type`, without
  Shellboy's session/routing headers, and its B-shape requests reported 0
  cached tokens, even for the second request with an identical prefix. Section
  8's cache question stays open until the implemented B runs through Shellboy
  itself.
- Not covered: Claude (not validated, by instruction), Chat Completions (no
  such connection), Shellboy's own adoption/epoch code (not implemented).


## 10. Implementation size (estimate)

| File | Change | Estimate |
| --- | --- | --- |
| `src/compaction/policy.rs` | `midturn_range` + table tests | ~40 + ~100 test lines |
| `src/compaction.rs`, `summary.rs` | B entry, prefix line, `summary_scope`, epoch bump in `adopt_tx` | ~50 |
| `src/control/driver/proactive.rs` | choose B or ordinary, record `midturn` | ~25 |
| `src/readback/compact.rs` | expose scope in metrics | ~3 |
| tests (compaction module / `overflow_tests`-style driver test) | adoption, replay, fork, readback, driver | ~350 |
| docs | compaction.md section, status, acceptance | n/a |

About 120 production lines and 450 test lines; no schema, config, table,
worker or dependency.

## Open questions

1. Implemented; 3-OS CI is recorded in acceptance.
2. Cache effect of an adoption (section 8) is unmeasured; measure with the
   implemented B through Shellboy's own requests (with its routing headers).
3. Thread-wide epoch drop also discards earlier-Turn reasoning. A positional
   drop would keep it but needs replay logic; revisit only if measured cache or
   quality loss warrants it.
4. Keeping reasoning on Responses (instead of dropping) is a later improvement
   that would need its own validation.
5. #182: existing prefix summary, middle summary, omission and spill edits on
   Claude 5.1+ with thinking.

## Owner amendment: automatic mid-Turn spill (#215)

After closed-Turn spill and summary fail to meet the calibrated target, the
automatic path may spill older complete active-Turn tool groups before trying
the active-Turn summary. The latest complete group, incomplete groups and
unreflected user inputs remain protected. Each stage measures the selected
replay anew and records outcome, skip reason and removed characters. Adopted
mid-Turn spills advance the Thread reasoning epoch and clear the kept set in
the adoption transaction, dropping all previously saved reasoning from replay.
This does not permit edits between a tool call and its result. Anthropic tool-use
thinking degradation is documented but has not been live-validated. Summary
material may include readable old-epoch reasoning, never encrypted or opaque
content; replay remains epoch-filtered.
