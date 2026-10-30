# Failure-Driven Input Reduction (#50)

Status: adopted for the first small implementation. Supersedes #34's estimated
activation threshold, once-per-Turn reduction and first-resumption stop guard.

- Remove token estimation/calibration and the fixed 75k guideline from Core.
  Remove compact_hint_tokens rather than add an override mode. There is no
  pre-POST compaction. Context edits report projected characters, not tokens;
  thread get reports the last successful measured usage and its timestamps.
- Existing retry eligibility, cancellation, partial-output opt-out, Retry-After
  and configured consecutive retry limit (default eight resends) remain. First
  transport/header/5xx failure retries unchanged; from the second consecutive
  failure an eligible failure may maximally reduce input. Ordinary 429 and
  receiving-body idle timeouts do not trigger reduction.
- Explicit context-limit failures immediately use the same maximum reduction.
  A second explicit overflow may use the existing latest-spill/summary stage.
  These sends also consume the same retry budget. No safe candidate means retry
  unchanged, not a new charge outside the budget. Successfully adopted model
  responses reset the consecutive budget and overflow stages, allowing a long
  Turn to reduce again. There is no independent once-per-Turn trim allowance.
- Reuse existing protected ranges, provenance and atomic path adoption. Omitted
  sources remain available through restore. No new tables/workers/queues.
- Add /compact-max and compact --maximal to request that same API-free maximum
  reduction at a safe boundary, without node IDs. This does not resume a Run;
  explicit retry remains a separate, potentially chargeable decision.
- Recovery presents saved failure/retry facts, protected request/latest tool/
  attachment sizes and applicable actions, without ranking or inferring causes.
  Keep TUI wording/layout minimal for the upcoming #41/#58 redesign. Retain
  normal retry/edit/model/detach/restore/stop operations and their safety rules.

Validation uses temporary DBs, controlled clocks and loopback responses: first/
second failure, default retry limit, no candidates, repeated reductions in one
Turn, explicit overflow stages, opt-out, 429/body-idle, stop/late result fencing,
model switch on retry, factual projections and API-free manual reduction.
Actual provider-specific error encodings and physical terminals are unverified;
no paid API or real conversation is used. No OS/process/encoding behavior changes
are planned; if such changes become necessary, run manual Windows CI before push.

First-version presentation scope: recovery uses the existing short stage/reason
line and F8 factual error details, not a new dashboard or a new user-action
ledger. Existing operation receipts/history remain the record of manual actions.
The model-selection choice is listed when another configured connection is
known; inspecting other models on the same connection remains the normal model
selector's responsibility. Protected sizes are replay JSON character/byte facts,
not a tokenizer estimate or the size of unmeasured external file contents.
