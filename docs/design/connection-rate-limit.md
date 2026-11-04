# Connection-Wide Rate Feedback (#224)

## Status: Proposed / Phase 1 Only

2026-11-05. Measurement and design draft, **not an adopted contract or an
implementation**. No runtime/config/schema changes accompany this document.
Current behavior remains [model HTTP](model-http.md), including #76 and #188;
summary adoption/cache rules remain [compaction](compaction.md). #215 parallel
map is a future consumer, not enabled by this draft. Owner/reviewer approval and
implementation acceptance below are required before changing behavior.

## Measurement And Limits

Owner-authorized, single read-only `mode=ro` SQLite connection, `VACUUM INTO`
private temporary copy; all DB reads used that copy. Free space first:
64,403,771,392 bytes; source 3,847,847,936 bytes; copy 3,779,506,176 bytes;
47.076 seconds to copy. Copy and private directory deleted after aggregation.
No live provider calls, credentials, raw messages, raw error bodies or log
publication. These aggregates are published with authorization in
[#224 measurement](https://github.com/tya5/shellboy/issues/224#issuecomment-6644711837).

Snapshot: 65,716 attempts, UTC start timestamps 2026-10-26 13:29:42 through
2026-11-04 22:57:03; 2,123 non-null `error_json` records. Count structured
`error.status` / top-level fallback, using saved attempt `profile_id`, not
current Thread settings. No message-substring classification.

| UTC attempt-start day | Connection | Recorded 429 | Recorded 5xx | Structured usage_limit |
|---|---|---:|---:|---:|
| 2026-10-26 | litellm | 0 | 1 | 0 |
| 2026-10-27 | litellm | 0 | 27 | 0 |
| 2026-10-28 | litellm | 0 | 19 | 0 |
| 2026-10-29 | litellm | 0 | 0 | 0 |
| 2026-10-30 | litellm | 0 | 1 | 0 |
| 2026-10-31 | litellm | 18 | 5 | 0 |
| 2026-11-01 | litellm | 27 | 6 | 0 |
| 2026-11-02 | litellm | 39 | 8 | 0 |
| 2026-11-03 | litellm | 0 | 7 | 0 |
| 2026-11-04 | litellm | 0 | 5 | 0 |
| 2026-11-01, 02, 03 (each) | litellm-claude | 0 | 0 | 0 |
| Total | all saved profiles | **84** | **79** | **0** |

All recorded 5xx were 500; another 9 saved errors were 400. 1,951 error records
have no saved status; 1,798 have no `kind` in the selected error object/fallback.
Non-null `error_json` is not synonymous with a failed provider call: proactive
compaction also writes metadata there (`src/control/driver/proactive.rs:610`).
The untyped records must not be relabeled lost errors or throttles.
Counts are **lower bounds of recorded HTTP facts**, not a complete provider-side
incident count or proof of zero usage limits. No second copy was made to extend
this measurement. Days/profiles absent from the table have no error row, not
necessarily no requests. UTC grouping can differ from the owner's JST day.

**Retry-After values are unknown.** Among the 84 429 records, 76 contain a planned
`automatic_retry`: selected delays 1/2/4/8/16/32 seconds each 10 times, 60 seconds
16 times; 8 contain no planned retry. Of 79 500s, 72 contain planned retries:
1s x39, 2s x8, 4s x5, 8s x4, 16s x5, 32s x4, 60s x7. These selected delays
cannot distinguish header-driven waits from backoff; they are **not measured
Retry-After values**, actual sleeps, POST counts or provider latency. The
inspected relevant error objects contain no Retry-After/reset field.

Summary ledgers: `app_meta` half-open range `summary.usage.` to
`summary.usage/`; 792 records and 3,708 counted requests:

| UTC ledger-start day | Connection | Records | Requests | Map | Reduce |
|---|---|---:|---:|---:|---:|
| 2026-11-01 | litellm | 36 | 182 | 146 | 36 |
| 2026-11-02 | litellm | 166 | 794 | 586 | 208 |
| 2026-11-02 | litellm-claude | 1 | 1 | 1 | 0 |
| 2026-11-03 | litellm | 485 | 1,824 | 1,347 | 477 |
| 2026-11-04 | litellm | 104 | 907 | 755 | 152 |

These are ledger-start dates, not per-request dates. Counting begins before the
client call, so local rejections/interruption can be included. **Summary
429/5xx/usage_limit counts and headers are unobservable, not zero.**

Owner logs independently inspected later: rotated log 26,671 JSON records,
2026-10-26 13:29:34 to 2026-11-04 11:42:49 UTC; current log 7,703 records,
2026-11-04 11:42:51 to 23:14:06 UTC. Respectively 321 and 2 `model_failure`
events: provider 169, incomplete 86, transport 51, timeout 14, http 3. They lack
HTTP status, Retry-After and reset fields; no independent daily/connection
429/5xx/quota count is possible. Attempt IDs can correlate but must not double
count the same DB observation. Digits in UUIDs/timings are not status records.
Logs are bounded/rotated, not a complete durable provider audit.

## Current Behavior And Recording Gaps

References refer to inspected base `291f298f03`; later changes may move lines.

- `src/control/driver/retry.rs:17`: `classify_retry` recognizes 429/all 5xx,
  checks error kind, terminal response and partial-output opt-out. A valid
  Retry-After above 600 seconds defers rather than retrying early. Budget and
  exponential delay are per Driver/Turn/generation (`:208`, `:229`): retry count
  resets only after durable success or changed Turn/generation. Defaults are
  eight additional attempts, backoff 1/2/4/8/16/32/60/60 seconds.
- `src/control/driver/retry.rs:267`: #188 declines explicit usage-limit retries
  when reset exceeds the remaining planned retry schedule. A matching
  driver-authorized retry may bypass the existing reset guard. No prose-based
  quota inference or guessed reset is allowed. `:414` releases the Thread
  boundary lock and sleeps cancelably on its own slot: another Thread is not
  delayed by an ordinary 429 without a recognized reset.
- **Existing shared exception:** `src/model/http.rs:77` holds the #76 timed
  usage-limit fact in an `Arc<Mutex<...>>` shared by client clones;
  `src/model/http/response.rs:121` locally rejects generation while active;
  `:162` remembers recognized facts. It is a shared rejection guard, not a
  general asynchronous admission wait. It covers generation including
  summaries, not embeddings/catalogs. Owner restart discards it. It must not be
  described as absent or silently replaced by a bypassable generic cooldown.
- `src/model/http/response.rs:338` reads status and `:342` parses Retry-After
  seconds/HTTP date; `:347` recognizes long (>600s) 429 header cooldown without
  waiting for the body. `src/model/http/body.rs:80` preserves error HTTP status,
  including a body-read timeout (`:93`). Thus some 429s **are recorded**.
- `src/control/driver/retry.rs:378` projects error/partial/raw output and selected
  delay into the durable attempt body, **not `failure.facts.retry_after`**.
  `src/model.rs:66` has status but no Retry-After field. This is where header
  attribution disappears even though status may survive. There is a specific
  exception: `src/control/driver/overflow.rs:126` saves parsed long-wait delay
  as `automatic_retry_deferred.retry_after_ms`; structured usage-limit facts
  may also preserve reset time. Neither field appeared in the measured 429
  records. Short/ordinary header attribution still cannot be reconstructed.
- `src/compaction.rs:677` consumes a map budget slot and starts a usage record,
  then calls the shared client once (`:679`). Failure updates only token usage
  (`:686`); context overflow can split a segment, otherwise `:703` returns an
  `ApiError` string. No ordinary transient retry. Reduce similarly uses its
  existing request fence (`src/compaction/bounded.rs:25`, `:32`). Completed
  cached parts remain reusable; source adoption stays atomic.
- `src/compaction/usage.rs:85` stores identities and token/request counters, not
  error status/header/reset. Summary failure facts are lost in that projection;
  the failure string is not a durable per-request structured status ledger.
- `src/diagnostics.rs:363` / `:382` reduces ModelError to an allowlisted reason
  and attempt ID; no status/header/reset survives. `usage_limit` falls through
  to Unknown. This explains why searching owner logs did not find a reliable
  429 observation; it does **not** establish that no throttling occurred.

## Proposed Admission State And Scope

Keep retry permission in the existing callers; the transport still sends once
per invocation, never performs hidden retries. Add bounded admission feedback
to the existing owner-scoped shared clients (`src/model/http/clients.rs:5`),
not a new worker, command queue, persistent table or background timer. Conversation
and compactor already share these clients (`src/control.rs:255`).

Proposed state per rate domain: monotonic `not_before`, feedback generation,
consecutive-throttle exponent, adaptive in-flight window, actual in-flight
count, one-probe flag, and fixed-size observation counters. Extend/compose with
the existing timed reset slot; do not create competing independent gates. A
short synchronous mutex protects updates only; **never hold it over await**.
Existing request futures sleep/select on cancellation and a change notification,
rechecking the generation/deadline when woken. No Thread/DB lock, output-drain
permit or HTTP receive deadline is held while waiting.

All outbound request paths on that connection must use admission immediately
before send: conversation generation, summary map/reduce, embedding, model
catalog and any future HTTP endpoint. The old explicit quota guard may retain
its generation-only semantics; the **generic 429 gate covers every endpoint**.
Already-in-flight requests are not cancelled. Late failures still extend the
shared gate; a late success cannot erase a newer throttle generation. Wait
expiry means eligible to compete, not permission to skip normal admission/CAS,
selection, capacity, credentials or stop checks. Revalidate after waiting.

Initial identity is the configured connection profile. To satisfy same-provider /
same-key aliases, propose sharing its rate-state reference for matching resolved
API origin/base path and effective authentication identity, constructed privately
at owner startup with a per-process salted fingerprint. Never store/log a key,
header, hash/fingerprint or provider URL in usage output. Do not merge unrelated
keys or origins; profile names remain the user-facing dimension. A matching
identity can conservatively share across models/endpoints even if provider quotas
are finer. Exact alias grouping needs confirmation against LiteLLM's quota scope;
if effective identity cannot be established safely, do not pretend profiles are
one domain. This is a review decision before implementation, not permission to
inspect/publish credentials or add configuration now.

## Proposed 429 Transition And Retry Contracts

1. Observe **every actual HTTP 429 at headers**, even if body read later fails,
   the request becomes stale, or the caller has no retry budget. Publish the
   shared deadline before returning the failure. Normalize a bounded header
   fact: absent / invalid / delta-seconds / HTTP-date, parsed duration/deadline.
   Do not retain arbitrary header text. Concurrent updates take the later
   deadline, never shorten another request's wait.
2. Valid Retry-After sets a floor. No header or invalid header uses connection
   exponential backoff starting at one second and capped by the existing
   connection delay setting; positive jitter may delay, never advance, a valid
   provider deadline. A zero/past header does not justify unlimited immediate
   resend: use a backoff floor and the single-probe rule. Track explicit reset
   separately; generic rate feedback does not guess subscription exhaustion.
3. At expiry, admit one probe on the existing request path; other senders remain
   gated until its result. On 429, extend wait and reduce the adaptive window;
   on non-429 completion, release probing with no fabricated success if the
   probe was cancelled/timed out. A failed/cancelled probe clears its ownership
   via RAII so another eligible future can probe; it does not clear the latest
   cooldown. No timer task launches requests autonomously.
4. Preserve per-caller retry classification, output boundaries, partial opt-out,
   receipt/CAS, duplicate-charge warning and exhausted/deferred recovery. Take
   `max(caller_backoff_deadline, shared_deadline)`, not additive double sleeps.
   Waiting itself is not a new HTTP attempt and consumes no request slot.
5. **#188 stays authoritative:** explicit reset beyond remaining scheduled retry
   budget goes to recovery immediately, not an hours-long suspended Run. The
   existing matching reset bypass never bypasses the generic shared 429 gate.
   Retry-After >600 seconds remains a deferred recovery/error, never truncated
   to 600 or automatically bypassed. New callers encountering such a gate
   return typed deferred information instead of waiting unboundedly. Ordinary
   shared waits have the existing 600-second automatic-wait ceiling; extensions
   beyond that per-admission envelope defer too. No auto-resume of recovery.
6. A 5xx keeps the existing retry policy but does not, by itself, freeze the
   entire connection or reduce its window: a server failure is not necessarily
   a rate signal. Save its facts too. Local quota rejection, cancellation,
   timeout-before-headers and missing status never increment HTTP429 counts.

Restart: deliberately ephemeral gate/window, like #76 today. Restart does not
resume interrupted Runs or summaries; new explicit work starts conservatively
with one probe, not a burst. Persisted observations are audit/usage only and
must not resurrect work. This can forget a provider deadline on restart; show
that limitation rather than claiming durable cooldown or deriving operational
state from old logs. If the owner requires cross-restart cooldown enforcement,
that is a separate adopted change (could use bounded existing app_meta, no new
table); not assumed in this draft.

## Proposed Summary Retry And Cache Reuse

Use the same pure transient classifier / header interpretation as conversation,
with summary-specific caller policy. Retry 429 and eligible 5xx only before a
complete accepted summary; never retry cancelled, malformed/refused final output
or permanent failure as though it were a rate signal. Keep context splitting
separate. Preserve receive timeout, stop/shutdown, and unchanged output budgets.

Every additional map POST consumes one existing `summary_map_max_requests` slot;
every additional reduce POST consumes one existing
`summary_reduce_max_requests` slot, **including failed POSTs**. No nested retry
loop can acquire a fresh budget; no automatic new compaction to reset it. Keep
four reduce rounds and current character/source limits. Reserve slots atomically
before dispatch if future parallel map is introduced. Caller waits consume no
slot; save `sent=false` for a gate-only rejection, distinct from the current
pre-client usage counter. Per-summary retry count/delay is bounded by remaining
request slots and the existing conversation retry policy, not a new setting.
The 600-second/explicit-reset deferral rules apply too.

After an eligible failure, retry only that exact uncached map segment or failed
reduce group. Reuse successful source-identified cached map parts after the
existing revision/model/prompt fences; do not resend or recharge them. Cache a
new success through the existing durable boundary before advancing. Reduce
intermediate successes may be reused within the same invocation; durable reduce
caching is not claimed if the existing cache does not support it. On exhausted
budget/defer/cancel retain original history and successful parts. Adoption is
still one atomic fenced operation; no partial spill or stale-summary adoption.
No replay eligibility or opaque/signed reasoning policy changes from #215.

## Proposed Fairness And Adaptive Parallel Map (#215)

Do not hard-code a map parallel count or create an independent summary executor.
All existing request futures acquire a connection admission lease; future map
launches are demand-driven by remaining uncached parts and available leases,
not a pre-spawned burst of every part. Begin window `W=1`. After a full window's
worth of successful requests in an unchanged feedback generation, increase W
by one (additive increase). A 429 sets `W=max(1, floor(W/2))`, closes admission
until deadline and requires one probe (multiplicative decrease). Do not punish
multiple concurrent 429s in the same feedback generation repeatedly; all may
extend the deadline. Late old-generation successes do not regrow W. No measured
success/429 means no evidence-based growth. Idle/restart resets W conservatively.

No fixed concurrency maximum is proposed: live demand, existing owner/resource
capacity, request budgets and in-flight leases are finite bounds; safe integer
saturation is not a throughput target. Shrinking below actual in-flight count
creates admission debt: let them finish without cancellation, grant no new
lease until debt is repaid. Track the window across conversation and summary
traffic, so a summary burst cannot ignore worker consumption. A W that keeps
growing despite queued requests/latency is not proof of safety; require future
measurement before enabling map parallelism in production.

For scheduling, propose one shared FIFO admission primitive (e.g. Tokio's fair
semaphore) inside the existing client, **not a new owner work queue/table**;
its cancellable waiter nodes belong to the already-live request futures. Each
worker or compaction may have only one pending admission acquisition at a time;
map may refill only after an acquisition completes, rather than enqueueing all
parts. Later summary requests therefore cannot overtake a waiting conversation;
conversely a continuous stream of new workers cannot overtake an already-waiting
summary. This deliberately chooses FIFO over strict worker priority (starvation
risk). Adaptive slot changes must preserve FIFO/cancellation and shrink debt;
prototype semaphore resizing under paused-clock tests before adopting it. If
that cannot be implemented without an unbounded side queue, return to review
rather than add one silently. Awaiters exist only for bounded existing work,
not durable queued jobs. At W=1, long requests may still delay other callers;
no latency guarantee or preemption is implied.

## Proposed Observability And Usage Get

Record bounded typed observations at headers/send completion, separately from
retry decisions. Attribute actual HTTP call identity, UTC observation day, saved
profile and request kind (conversation/map/reduce/embedding/catalog). Shared
aliases retain per-profile counts; a domain total must deduplicate actual calls.
Persist through existing attempt error/usage and summary usage app_meta records,
not a new table. Propose new fixed-schema connection/day counters under the
existing `app_meta` for endpoints without attempt records; such counters do not
exist today. Define retention with existing GC
before implementation, and never add a record per wait wakeup.

Expose additive `usage get` fields: observed 429/5xx, structured usage-limit
provider facts versus local rejections, missing-status observations, retries
planned/sent, gate deferrals, valid/absent/invalid Retry-After counts, bounded
numeric retry-delay/header min/max or fixed buckets, current shared deadline
and W, and accumulated actual waiter milliseconds. Distinguish sum of caller
wait times from union of connection-cooldown wall time. Runtime fields are
owner-session observations; durable counters report coverage/version/unknowns.
Do not turn legacy missing fields into zero. Existing token unknown accounting
and totals stay unchanged. Count actual HTTP429 once even if it triggers both
conversation recovery and summary failure; a local guard failure is not another
provider429. Usage metadata must remain bounded/indexable by connection/day,
not scan raw response bodies or all history for each refresh.

Diagnostics should add allowlisted status, Retry-After validity/parsed duration,
request kind, profile, applied shared wait and feedback generation to existing
bounded events; no raw error/header/key/body. Persist observed fact before
returning a summary failure, with interruption/DB-failure unknown coverage
explicit. Failed observation persistence must not cause a hidden resend.

## Unknown LiteLLM Facts And Review Decisions

The owner's measured 429s establish throttling at the client-facing connection,
not its root cause. Unknown: LiteLLM version/config; proxy retry and fallback
policies; whether Retry-After is emitted, stripped or rewritten; quotas by
virtual key, team, tenant, model group, deployment or upstream credential; whether
the two profile names share a real quota domain; requests-per-minute versus
tokens-per-minute versus concurrent-request limits; which upstreams caused the
429; other applications sharing the key; and whether catalog/embedding use a
separate quota. Client attempts do not reveal proxy-internal fan-out or charges.
No live probe, config/key inspection, provider changes or paid request is
authorized by this phase. Confirm metadata with the owner before choosing alias
grouping or claiming AIMD will discover the provider's optimal capacity.

Before implementation: approve generic all-endpoint scope, conservative alias
identity, FIFO discipline, ephemeral restart behavior and additive usage fields.
No adopted #76/#188 rule or #215 caching contract is changed by this draft.

## Future Acceptance (Not Executed In Phase 1)

| Area | Required deterministic evidence |
|---|---|
| Shared gate | Two Threads + summary + embedding/catalog; same domain waits, different domain sends; alias isolation tests |
| Headers | Absent/invalid/zero/past/date/seconds/overflow; body stall after429; concurrent deadline extensions never shorten |
| Policy | >600s defer; #188 reset beyond remaining budget; matching bypass cannot skip generic gate; partial/terminal rules unchanged |
| Lifecycle | Cancel/stop/shutdown during wait, stale revision, cancelled probe, late old success, no locks or leaked leases |
| Budgets/cache | Exact POST counts, failed POST slot consumption, cached-part no-resend, four rounds, original-path atomicity |
| Fairness/AIMD | Sustained workers and summary demand, FIFO no overtaking, no pre-enqueued burst, window growth/shrink/debt, no fixed cap |
| Observability | Actual429 vs local reject, no double-count, day/profile dimensions, header unknowns, legacy coverage, redaction/GC bounds |
| Restart | No automatic resurrection; ephemeral deadline/window reset and first probe explicitly verified |

Phase 1 CI can check this document's branch but cannot validate an unimplemented
scheduler, provider quality, throughput, native hardware or rate-limit recovery.
