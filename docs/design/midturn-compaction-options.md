# Mid-Turn Summarization: Options for Decision

Status: **A adopted; B deferred pending real-world evidence.** C and D are not
adopted. No new mid-Turn summarization or configuration is authorized. The
existing spill placeholder is simplified separately: it no longer duplicates
call arguments, which remain on the call item; it retains the original result
reference and reports the UTF-8 byte length of the original serialized tool-result output.
Prior placeholders did not carry a size; already-saved spills are not rewritten.

The user chose the simplest policy: B adds a replacement operation and its
reasoning replay contract cannot be established with mocks. The only real case
so far is one W3 conversation from before the #153 fix. The user will check
post-0.6.0 worker conversations twice daily, tracking reported `input_tokens`
within a Turn and what remains after spill. Reconsider B only if a real example
continues growing substantially beyond the threshold. Synthetic bytes are not
provider token or cost measurements.

## Evidence And Problem

On the selected replay path, spill replaces eligible tool **results**, not the
assistant messages, opaque reasoning, tool-call arguments, or all short results.
Earlier per-result placeholders also contained copied arguments; new spills omit
that duplicate. Existing saved spills are not rewritten. The
current automatic summary protects the current Turn and the most recent
completed Turn. Consequently, spill plus ordinary summary need not bound the
input of a single ongoing Turn. Explicit overflow recovery is a separate,
wider existing exception: it may include the current user request. It is not
proof that proactive mid-Turn replacement is safe.

In the synthetic 401-attempt loopback described in the author's [#153
comment](https://github.com/tya5/shellboy/issues/153#issuecomment-6565773561),
the compact-JSON `input` grew from 1,054,893 bytes at request 301 to 1,401,843
bytes at request 401, even after the eligible older outputs had been spilled.
The +346,950 bytes comprised assistant items +35,900, reasoning items +60,300,
call items +110,725, placeholders +139,625, and array structure +400. This is
a synthetic byte accounting, **not** provider tokens, cost, the complete HTTP
body, or a reproduction of any private conversation. The mock's usage values
were deliberately fabricated to exercise the trigger; the mock did not verify
provider reasoning signatures. Its first harness had a wrong expectation of a
second summary request; a separate read-only synthetic DB check confirmed the
reported two partial adoptions and one summary request. Neither this experiment
nor these options establishes real-provider acceptance or native Windows behavior.

Sources at the researched revision `21b83fbf854d2065450e07c1ebb5553925beb0e1`:
`src/compaction/policy.rs`, `src/compaction/range.rs`, `src/compaction.rs`,
`src/control/replay.rs`, `docs/design/compaction.md`,
`docs/design/replay-cache.md`, and `docs/design/model-http.md`. Current main's
`docs/design/compaction.md` sections on #125 and #153 are the adopted baseline;
the other options in this document remain unadopted possibilities.

## Non-Negotiable Trigger And Boundary

- Use the existing #125 rule: only the saved provider-reported `input_tokens`
  for the latest completed attempt on the current connection/model, at or above
  the existing `auto_at_tokens` threshold, can trigger automatic work. Missing,
  unknown, stale, or post-replacement-pending usage cannot trigger. No byte,
  character, output-token, generated-reasoning-token, elapsed-time, or heuristic
  estimate substitutes for it. The existing per-connection override and zero
  opt-out remain unchanged; **add no configuration item**.
- The existing #153 non-adoption/partial-spill retry gate uses fresh reported
  growth of `ceil(auto_at_tokens / 10)` and remains in force. A successful
  adoption requires a fresh post-replacement measurement before another
  automatic attempt. Define one coordinated attempt per boundary, not separate
  ordinary and mid-Turn paid loops. How ordinary prefix summary and the proposed
  candidate share a retry baseline is an implementation decision that must be
  specified and tested before adoption; it must not weaken #153's gate.
- The snapshot is taken only at a safe next-request boundary with the Thread
  guard, no in-flight model request, and complete tool groups. Do not split a
  multi-call assistant response, a call/result pair, parallel siblings, or an
  outstanding call. Protect pending work, unconsumed messages/receipts,
  attachments, system/developer instructions and the most recent working
  suffix. A complete group is structurally eligible, not proof its facts can
  safely be summarized. Never execute a saved tool again during replay.
- Before adopting any asynchronous result, revalidate the exact path/selection,
  range and group fences, generation, active Turn, connection/model, reasoning
  epoch, pending/protected state and cancellation/stop state. Atomically adopt
  only the validated replacement and provenance. A race, provider error,
  rejection, cancellation or storage failure must not install a partial summary;
  previously committed #153 spills remain subject to their existing separate
  adoption/error semantics. Retain original nodes for history, restore and fork.
- Opaque reasoning must never be parsed, truncated, invented, re-signed or
  assumed portable. Epoch filtering today removes items whose epoch differs; it
  does not prove that the remaining provider sequence is valid, or that a model
  can continue coherently after omission. If an adapter's documented contract
  and tests cannot establish an admissible resulting sequence, skip the candidate
  and preserve the original replay path. A mock alone cannot establish that
  contract. Any epoch change is a proposal, not a given consequence of summary.

## Simplicity And Reuse Comparison

Counts here are design estimates, not a completed implementation inventory.
"Concepts" means additional distinct behavioral rules; "state" distinguishes
logical state from new persisted fields or tables, which are **not** decided.
Module counts are candidate areas rather than a promise or an exact patch size.
All four options continue to use existing #125 trigger and #153 retry policy.

| Option | Additional concepts and state | Estimated affected modules | Reuse and limits |
| --- | --- | --- | --- |
| A: do nothing | 0 concepts; 0 new state | 0 | Reuse spill, ordinary summary, retry and explicit overflow unchanged. No mid-Turn automatic reduction. |
| B: middle segment | At least 1 concept (mid-Turn replacement range), plus fenced candidate/provenance state; new durable state: unknown | Roughly 3-5 areas: policy/range, compaction adoption, replay, plus possible storage/inspection | Reuse whole-group validator, summary generation, existing spill and retry; a middle replacement and provider reasoning compatibility are not established by existing prefix or overflow paths. |
| C: wider prefix | At least 1 concept (automatic release of current-Turn prefix); snapshot/provenance state may be reusable; new durable state: unknown | Roughly 2-5 areas: policy, compaction/adoption, replay, possibly storage/inspection | Reuse prefix summary, spill and group fences; overflow is precedent for broader scope, not a safe proactive policy. Preserving/reinserting the user message adds a separate ordering concept. |
| D: new continuation | At least 2 concepts (continuation boundary and reasoning transition), plus lifecycle/provenance/epoch state; number of new durable fields/tables: unknown | At least 4 areas: driver/Turn lifecycle, compaction, replay, persistence; possibly provider adapters and UI | Reuse spill, existing summary and retry triggers where compatible. Neither ordinary summary nor overflow currently proves a new continuation or epoch preserves semantics. |

Option A is the simplest by construction. B's relatively narrow semantic scope
does not imply fewer modules than C. These rough counts must be replaced with an
actual design and dependency inventory before authorization to implement; do
not add state merely to meet a number. Where a range is skipped, A remains the
behavioral fallback rather than a second autonomous compaction mechanism.

## Compared Options

### A. Keep Current Automatic Policy (Baseline)

Keep #153 spill and ordinary prefix summary; do not automatically summarize the
current Turn. Continue the existing explicit-overflow path when applicable.
The group and reasoning replay boundaries remain as today; no new mid-Turn
reasoning omission is introduced. This does not prevent a single long Turn from
growing beyond the provider's context and does not promise overflow recovery
will succeed. The latest group and protected content remain unchanged by normal
spill. Existing explicit overflow's broader scope is a distinct risk, not a
new guarantee.

Implementation: none for this proposal. Cost: no additional mid-Turn summary
request or mid-Turn-summary cache disruption; current spill, ordinary summaries,
and overflow can still cost requests and change a provider prefix. Existing
cache hits are not guaranteed. This is the lowest implementation risk but leaves
the reported residual growth unsolved.

Operationally, the user can divide a long task into smaller completed Turns and
provide an explicit handoff of facts, paths and remaining work. A new Turn in
the same Thread still includes history, and ordinary summary also protects the
latest completed Turn; splitting alone does **not** immediately reduce context
or guarantee a lower token count. The user can instead start an independent new
Thread with `/new` and deliberately carry forward the necessary facts and file
references, subject to the loss and verification risks of that handoff. Manual
`/compact` follows its own safety boundaries and may not reduce the current
Turn; explicit `/compact-max` is a more aggressive, separately documented
operation, not a guaranteed or lossless fix. After an interruption, inspect
saved tool results and side effects before retrying: ending or retrying a Turn
must not blindly repeat a possibly executed command. Missing provider usage
still cannot trigger automatic reduction, and none of these actions gives a
fixed safe maximum or guarantees avoidance of context overflow.

### B. Replace An Old Middle Segment, Preserve Current User Request (Deferred)

After the normal trigger and safe boundary, choose an **old contiguous segment
inside the current Turn** ending at a complete safe group boundary; keep the
current user request, applicable instructions and a nonempty recent working
suffix verbatim. Replace that segment in the selected replay path with a summary
that records durable task facts, observed failures and side effects, exact paths
and identifiers needed to continue, unresolved work and provenance. Do not
re-run tools. Summarization is lossy even if all groups are complete: the model
can omit or distort facts, and neither prompt instructions nor schema alone prove
fidelity. Preserve originals for inspection and manual recovery.

Call/result closure must include every call and corresponding result from each
covered assistant response, including parallel calls; no retained result may
reference a removed call, or vice versa. Keep the current user request and recent
suffix outside the replaced segment. The resulting sequence must additionally be
valid under the active adapter's documented reasoning/replay contract. If the
covered and retained reasoning cannot be separated without breaking that
contract, **skip**, rather than guess, splice an opaque item, or silently switch
epoch. For example, retained reasoning may be dependent on the removed context
or output even when call/result pairing is structurally sound. Provider-specific
policy and live validation are open prerequisites, not implied support.

This is a new middle-range summary/adoption operation, not simply the existing
prefix selector. It needs explicit source provenance, bounded range selection,
summary inputs, independent shrink/adoption validation, atomic fenced adoption,
and replay/readback/fork tests. Existing machinery may be reusable, but exact
scope and storage/schema needs are not yet established. A summary's smaller
serialized projection is only a local shrink check, not a token estimate or a
guarantee that the next request costs fewer tokens. Require a bounded summary
response and reject a non-shrinking candidate; preserve original on failure.

Cost: each attempted summary can incur one or more provider summary requests,
depending on existing segmentation, reuse and failure; retries add more under
the existing measured gate. Each adopted middle replacement changes the request
prefix from its insertion point onward. At most the unchanged prefix before that
point is a candidate for provider cache reuse; actual matching, minimum cache
sizes, eviction, billing, and hits are provider-dependent. There may be one
cache-prefix discontinuity per adoption, not necessarily one paid cache miss or
one miss per subsequent request; repeated adoptions can cause repeated cache
loss. Local replay cache invalidation/reconstruction is a separate correctness
requirement, not evidence of a provider cache hit or miss.

### C. Extend Prefix Summary Into The Current Turn

Extend the existing prefix-style replacement to cover old complete current-Turn
groups, potentially including the current user request, while protecting the
latest group and other hard fences. A variant could reinsert the original user
request outside the summary, but that is not equivalent to the current prefix
operation: it must preserve ordering, provenance and non-duplication, and needs
its own design. Structural group closure and adapter reasoning compatibility
are mandatory just as in B; the existing overflow path permits a broader prefix
but is not a safety proof for automatic use.

Risk: a lossy summary can alter or omit the user's live instruction itself,
attachments, relevant prior context, side effects or exact identifiers; moving
or duplicating the request may change semantics. Source originals and safe
atomic rollback remain necessary. Reasoning in or after the prefix may depend on
the original prompt and cannot be assumed valid after replacement. A conservative
adapter may skip, leaving growth unresolved. This approach may reuse existing
prefix plumbing, but proof of correct provenance, preserved instructions,
reasoning behavior and boundary concurrency still makes it a substantial change.

Cost: paid summary attempts and their segmentation/retries as above. Since the
replacement begins earlier than B, it potentially changes a longer provider
prefix and can reduce the reusable prefix more often or by more bytes, but neither
a precise hit rate nor actual provider cost follows from position alone. Each
adoption can produce a new prefix discontinuity; subsequent matching and costs
remain provider-dependent.

### D. Explicit Continuation / New Reasoning Epoch

At a safe boundary, summarize completed work, retain the original user request
and current constraints, and start a new continuation whose replay deliberately
omits incompatible earlier reasoning according to a newly specified adapter
contract. Preserve complete call/result groups or remove each covered group as a
whole; do not carry an orphan result into the continuation or execute the calls
again. Treat a new epoch or continuation boundary as a substantive behavioral
change, not a mechanism that proves an opaque reasoning sequence is valid.
Never carry old signed reasoning across the boundary by assumption.

Risk: lost hidden working state, different model decisions, tool side effects
being repeated by the model, lost active instructions or pending inputs, and
broken attachment/fork/history semantics. It may need new persisted provenance,
continuation lifecycle and recovery rules; changes to schema, migrations or
public behavior cannot be assumed unnecessary. It is the largest change and
requires explicit product and provider-specific decisions before implementation.

Cost: summary request(s), possible additional continuation/model request, and a
changed provider prefix at each transition. This can invalidate reuse from the
first changed position and may reduce cache reuse more than B; it does not prove
a full cache miss or predict a number of billed tokens. Frequency follows actual
eligible reported usage and successful boundary decisions, not an invented timer
or an assumed per-N-attempt schedule.

## Decision And Future Acceptance

A is adopted; B is deferred under the real-data condition above. C and D are
not adopted. The following are safeguards for any separately authorized future
design, not permission to implement one. No option guarantees bounded context,
zero information loss, a provider cache hit, or successful overflow recovery.

Before any future implementation, acceptance must cover synthetic multiple and
parallel calls; orphan/missing outputs and incomplete groups; protected latest
groups, instructions, attachments and pending follow-ups; non-shrinking and
failed summaries; cancellation, storage failure and late-result fencing;
selection/fork/profile/epoch changes; readback/provenance and no tool re-execution;
unsupported reasoning contracts and the fail-closed path; absent/stale usage,
threshold/opt-out and #153 retry-gate behavior. Measure actual provider-reported
input and cache usage when available, separately from local serialized bytes and
summary-request usage. Provider tests require separate authorization and cannot
be inferred from mocks. Test exact cache-prefix differences and local replay
invalidation without asserting a provider hit or fixed monetary saving.
