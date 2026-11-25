# Foreground shell decision points (#423)

## Non-goals

- No sender urgency or priority, Core-chosen expectation, repeating timer, automatic backgrounding, process timeout, cancellation on input, or change to response-generation interruption.
- No output, message, timing, state, or diagnostic bundle in the early tool result. Use existing reads; no new queue, history, timer ledger, or DB migration for decision points.
- No new ability to adopt another Thread's process or run a command twice. No change to process ownership, ordinary `wait`, or Run completion rules.
- No implementation or client pin change in this design revision. The guide examples below are proposed text, not current tool syntax.

## Event table

A foreground wait blocks the model that launched it, not the owner. Only the launching model decides what to do at a new decision point; the human Background/Stop controls retain their existing authority. The event is a boundary at which the model can reconsider, not a verdict about the process.

| Event | Exact wait armed | No foreground wait armed (including model generation, decision in progress, and background work) |
|---|---|---|
| New committed input addressed to the waiting Thread (human, another Thread, or a schedule) | Return `additional_input` once for this arm with the exact execution ID. Deliver eligible ordinary input at the normal next request boundary, in the existing order. The model can keep waiting, explicitly background, or stop. | No foreground result and no interruption of the in-flight response. Apply normal delivery/admission rules. |
| New input that is held (for example `complete` Revisit) | New arrival can wake the wait, but does not release the hold or promote the input. If no ordinary input is delivered at the boundary, inspect the current execution and any already-known message ID; do not infer the contents or bypass the hold. | It stays held until its existing eligibility boundary; no automatic resume. |
| The model's expected interval expires while the execution is still live | Return `expectation_exceeded` once for this arm, with the same ID. No restart, stop, or background. Choose again after reading current facts. | No timer or notification. |
| Exact shell completes | Return the existing completion result, including exit and output evidence. Later calls follow their existing prerequisite rules. | Existing saved lifecycle, output, and completion notice rules still apply. A later read or wait observes the terminal result. |
| Human Background on the exact execution | Existing `human_backgrounded` release and remaining-call cancellation. The process continues; the action **does** release its foreground binding. | If a decision is pending for that execution, the human can still explicitly background it; no second tool result. Otherwise retain existing exact-target acknowledgement/refusal. |
| Turn interruption, Run stop, owner stop | Existing `human_turn_interrupted`, stop, and shutdown paths win when already committed; no new interruption behavior. | Existing behavior; a stale decision cannot resume or retarget a stopped/replaced Turn. |

A notification is a hint, not proof of input. Arm against the exact owner, Thread, Run, Turn generation, execution, and current tool call. Register before checking committed state; use the existing admission/commit boundary and keyed wake discipline, not a sender's intent or a periodic scan. On launch include eligible pending ordinary input not yet exposed to the model, including input committed during spawn. On re-arm do not repeatedly wake for the same held or already observed arrival. Coalesce a burst into one return per arm; preserve normal message delivery and receipt identity independently. The captured baseline and later commit signal must not lose a new arrival in the registration/re-arm window. Cancellations, duplicate operation replays, refused sends and cross-Thread notifications do not create a fresh input event. If several causes are ready, prefer authoritative stop/completion and the already accepted human Background over a nonterminal decision; prefer newly committed input over the expectation. Return only one result. A process that exits immediately after a nonterminal result is found terminal on readback.

This costs one extra model call when ordinary input arrives during the wait and the model chooses to keep waiting. Coalesce only as above; do not add sender priority to avoid that cost.

## Tool contract and choices

An early decision returns exactly these two fields in the model-facing shell tool result:

```json
{"reason":"additional_input","execution_id":"EXACT_EXECUTION_ID"}
```

The timer decision reason is `expectation_exceeded`. The ID names the execution that was nonterminal at the decision boundary, not a promise it is still running when inspected. There is no `status`, output, input ID, elapsed time, or last-output timestamp in this variant. The existing human-override and terminal variants remain distinguishable and unchanged. The tool-group coordinator cancels later unexecuted calls on a decision point; that is internal control, not a `cancel_remaining_calls` key in the new result. Give later calls distinct saved prerequisite reasons for input vs expectation (do not report them as failed processes or human actions). Existing stored `additional_input` results carrying `status: running` retain their historical meaning; readers must not reinterpret them as the new two-field decision.

Proposed tool arguments: on a foreground launch, optional `expected_duration_ms`, a positive, checked duration measured against a monotonic clock from arming the wait (not a guessed startup or execution duration). Omission means no timer. Do not accept this argument for a background launch. After a decision, the same shell tool accepts an exact `wait_execution_id` instead of `command`, with optional `expected_duration_ms` for the *new wait interval*; launch-only arguments are invalid in this form. This is a timed observation/resumption of the same foreground binding, not a new shell command or a new public RPC. The existing generic `shellboy wait` remains an observation command for background work, not this re-arm path.

Returning a decision pauses the wait without marking the execution explicitly backgrounded. The existing foreground owner retains at most one resumable identity per driver; do not leave its original wait future or timer running. Short shell calls used for `shell get`, `shell read`, or `message get` must not erase this identity just by starting or completing. If a different foreground execution itself reaches a decision, the newer decision supersedes the older re-arm identity; the older process stays owned and can still be inspected or stopped by ID. Keep-waiting is accepted only for the exact still-eligible binding of the launching model in the same live Turn generation. A terminal exact execution returns saved completion; human-backgrounded, stopped, superseded, or foreign nonterminal bindings are refused, with no fallback to latest. The model may instead explicitly use `shell background` or `shell stop` for that ID. Background from a pending decision clears the foreground binding without stopping the process; a stopped or replaced binding cannot affect a different execution. Ending the Turn retires the decision binding and preserves existing Run ownership and cleanup; it does not record a fabricated Background action. No decision clock survives owner restart.

## Delivery and existing release reasons

- `human_backgrounded` remains the explicit human release, with the same target and subsequent-call semantics. The new nonterminal decisions do not impersonate it. `human_turn_interrupted` remains a distinct existing intervention.
- `additional_input` moves from sender-selected immediate release to the ordinary committed-input decision above. Keep the stable reason name but distinguish old and new result shapes; keep historical delivery and result records intact.
- Remove `--deliver now` and the corresponding send/ask input variant and TUI immediate-send choice in the input-decision unit. Do not alias it to `next` or carry a sender-only wake path. `next` is still normal boundary delivery, and `complete` keeps Revisit semantics: a wake is not early delivery. Preserve historical readback and any already-queued records without reassigning, losing, or replaying them. Before removal, check pre-upgrade outstanding `now` receipts and same-operation replay against persisted argument hashes; either establish a lossless upgrade path without accepting new `now` input, or keep removal out of the candidate until those witnesses pass. Never edit historical receipts to make the new input enum decode.

## Optional client facts (separate unit)

Existing `shell get` gives state, `started_at`, `last_output_at`, optional `elapsed_ms`, and terminal evidence; use `shell read` for retained output, not the decision result. `message get` gives a known message's delivery status, assignment, and saved `context.hold_reason` where present. `message list --awaiting` is a reply-duty list, **not** an inbox of ordinary queued sends. Use admitted input at the next request boundary to identify ordinary new messages; use `message get` for known IDs and `message list --awaiting --owed-by-thread THREAD` for owed duties. No general discovery guarantee is made for held ordinary input whose ID is unknown. Do not claim a live wait reason from a saved hold reason, an elapsed time, a generic Turn `waiting`, or a launch-time background flag.

Studio #159 needs reliable present-tense foreground and pending-message attribution; current reads do not establish the exact binding, distinguish a paused decision from explicit background, or prove which active phase blocks a particular message. If Studio adoption still needs it, independently expose in existing scoped reads `waiting_for: {kind: "foreground_shell", execution_id: "..."}` or `{kind: "response_generation"}` only when proven for that exact message/Turn, and an execution disposition distinguishing foreground wait, pending decision and background for known live bindings. Omit when unknown or inapplicable; absence is not background. Capture both from matching authoritative owner facts, fence by exact identities, and prefer omission under race over guessing or a new persisted mirror. This optional read unit is not required for model decision timing.

## Agent guide deliverable

Each implementation unit ships the corresponding text in `docs/guides/agent-guide.md` and a cross-link from `docs/guides/shell.md`. Add only examples whose syntax and behaviors that unit actually ships; rework the sender and Revisit sections when `now` is removed. Update the agent guide's quick lookup table. Proposed guide text:

**Pick a checkpoint, not a timeout.** Estimate from this command, not a universal number. For tests use recent duration of the same suite and scope; for builds account for a cold cache and linking; for installs account for download and unpack stages and use noninteractive options where intended; for a migration use known volume or past runs, and avoid stopping it without checking rollback/recovery implications. Omit the estimate when you cannot make a useful one. A server normally has no natural finish: launch in the background when appropriate and wait for readiness, not exit. For a foreground server's startup inspection choose an expectation for readiness, not its lifetime.

**Read before acting.** Retain the returned work ID. First run `shellboy shell get --execution EXECUTION_ID` to see state and time facts; if it already ended, inspect exit code, reason, and output completeness. If input woke you, consider what just arrived at the model boundary; check `shellboy message get --message MESSAGE_ID` for a known message and use the duties list only for reply-expected input. Read `shellboy shell read --execution EXECUTION_ID --limit-bytes 65536` when progress or failure evidence matters; keep its cursor for later reads. Check new instructions before making a consequential decision. Missing or old `last_output_at` alone does not prove a hang.

| What you know | Choice |
|---|---|
| The next step needs this result and the work should continue | Wait on the same exact execution, optionally with a fresh estimate. |
| The work may continue independently and you have other useful work | Explicitly background the execution; retain its ID and follow it later. |
| The user authorizes skipping this work, or inspection establishes stopping is appropriate and safe | Stop the exact execution, then confirm actual terminal state; acknowledgement is not completion. |

After Background, retain the **work** execution ID, not the ID of a helper shell running a CLI read. Use `shell get`, bounded `shell read` with its cursor, and `shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"EXECUTION_ID"}]' --timeout-ms 30000` when useful; check saved exit status and output after any completion hint. Do not stop merely because input arrived, poll in a tight loop, relaunch to resume waiting, or assume ending any wait means success.

### Worked transcripts for the agent guide

IDs and stated observations below are illustrative; substitute returned IDs and actual evidence.

```text
Model -> shell: {"command":"cargo test --locked --lib","description":"Run the library tests","expected_duration_ms":90000}
Tool -> Model: {"reason":"additional_input","execution_id":"TEST_EXEC"}
New input: "Routine report: the docs review is done." [REPORT_MSG]
Model -> CLI: shellboy shell get --execution TEST_EXEC
Model -> CLI: shellboy message get --message REPORT_MSG
Model: Still running; the next step depends on the test result. The report does not change it.
Model -> shell: {"wait_execution_id":"TEST_EXEC","expected_duration_ms":60000}
```

```text
Tool -> Model: {"reason":"additional_input","execution_id":"OPTIONAL_EXEC"}
New input from the person: "Skip that optional check." [SKIP_MSG]
Model -> CLI: shellboy shell get --execution OPTIONAL_EXEC
Model -> CLI: shellboy message get --message SKIP_MSG
Model: This is the running optional check, not the active migration. Stop it only.
Model -> CLI: shellboy shell stop --execution OPTIONAL_EXEC
Model -> CLI: shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"OPTIONAL_EXEC"}]' --timeout-ms 30000
Model -> CLI: shellboy shell get --execution OPTIONAL_EXEC
Model: Saved state is stopped; report the check as skipped, not passed.
```

```text
Model -> shell: {"command":"cargo build --locked","description":"Build the project","expected_duration_ms":120000}
Tool -> Model: {"reason":"expectation_exceeded","execution_id":"BUILD_EXEC"}
Model -> CLI: shellboy shell get --execution BUILD_EXEC
Model -> CLI: shellboy shell read --execution BUILD_EXEC --limit-bytes 65536
Model: It is linking and output has advanced. The build can proceed while I check docs.
Model -> CLI: shellboy shell background --execution BUILD_EXEC
Later -> CLI: shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"BUILD_EXEC"}]' --timeout-ms 30000
Later -> CLI: shellboy shell get --execution BUILD_EXEC
Later -> CLI: shellboy shell read --execution BUILD_EXEC --cursor BUILD_OUTPUT_CURSOR
Model: Verify exit code and output before reporting the build result.
```

```text
Model -> shell: {"command":"./dev-server","description":"Start the development server","expected_duration_ms":10000}
Tool -> Model: {"reason":"expectation_exceeded","execution_id":"SERVER_EXEC"}
Model -> CLI: shellboy shell get --execution SERVER_EXEC
Model -> CLI: shellboy shell read --execution SERVER_EXEC --limit-bytes 65536
Model: The server reports listening; verify readiness through its documented health check.
Model -> CLI: shellboy shell background --execution SERVER_EXEC
Model: Retain SERVER_EXEC to inspect it later and stop it when no longer needed. In future start this kind of server with background:true and check readiness, not exit.
```

## Migration and adoption

1. **Model-timed checkpoint and exact resumption.** Land the optional expectation, two-field expectation result, re-arm, existing cleanup/override interaction, and guide material for builds, tests, and servers that uses only this unit. Test no timer by default, one return per arm, no auto-background or stop, explicit new interval, short read helpers followed by resumption, exact stale target refusal, exit and human/owner stop races, and cancellation of remaining sequential calls. Existing ordinary input timing (including `now`) stays until unit 2; keep its legacy result format in this unit. No DB migration.
2. **New-input checkpoint and remove sender-immediate delivery together.** Extend the same boundary to committed input, keeping hold/admission semantics; remove new `now` from CLI/wire/TUI, coordinate Studio against the landed contract, and retain historical evidence. Update guide examples for routine reports and skip requests, criteria for not stopping, and the sender/Revisit documentation. Test both race orders around wait registration and re-arm, coalescing and duplicate/held input, different senders, existing `complete` and response-generation rules, human override, and a migration/replay witness over previously accepted data. Do not ship removal ahead of replacement. Apply protocol numbering at landing to any public wire change; do not renumber for this design.
3. **Optional Studio #159 facts.** Ship only with a demonstrated remaining client need and bounded authoritative data. Add client and guide descriptions of what the facts do and do not establish, with omission, replacement-owner, and exact-binding witnesses. No dependency from model timing back to this read unit.

**TUI:** Keep exact-ID Background/Stop and existing interruption separate; present input/expectation as a model decision, not process failure or human Background. Remove the immediate-send gesture and label when unit 2 lands; keep `next` and Revisit. Update shell continuation help against the actual shipped syntax and test selected/stale targets.

**Studio:** Preserve launch and observation IDs and exact human Background/Stop. Recognize new reasons without inferring exit or automatically resuming. Remove sender-now controls at protocol adoption; do not replace them with urgency. Adopt optional #159 facts only if that separate unit lands. Neither this document nor the Core design authorizes a Studio pin bump.
