# Explicit foreground shell release (#423)

## Non-goals

- No automatic wake on send, wait timeout, or hold detection. No priority flag, shell timeout or expectation argument, or paused/resumable foreground state.
- No Background by Thread, latest-execution lookup, or execution-wide foreground discovery API in this slice. The sender follows its own message and targets only its proven blocker.
- No change to response-generation interruption, process ownership, Stop permissions, or owner admission. No caller-Thread authorization rule for Background.
- No new wake envelope, queue, persisted blocker mirror, or DB migration assumed. This revision is design only.

## Track the exact message

Send or ask with default delivery and keep the **message ID** returned by acceptance, not the operation ID or a reply ID. Acceptance is not application. Core exposes two independent read paths: a typed explanation of a current hold and a bounded wait for this message to be applied.

### Typed hold fact

Add optional `held_by` to `message get` and existing read projections that expose the same message's current delivery (including Studio's pending rows). Keep existing status, saved context, and hints; `held_by` is structured current evidence, not a renamed historical reason or a parse of hint text. Do not expand list scope or attach this live field to historical send/ask receipts and exports.

```json
{"held_by":{"kind":"foreground_shell","execution_id":"EXACT_EXECUTION_ID"}}
```

Other variants include `response_generation` and existing verified hold kinds such as `run_completion` and `recovery_wait`; none of those carry an execution ID. Omit the field when not held. For a held message with incomplete or conflicting live evidence, report `{"kind":"unknown"}` rather than making an actionable claim. Do not issue a foreground-shell reason for a hold that Background cannot release, such as Revisit waiting on Run completion. Check what blocks **this message**, not whether its recipient happens to have a running shell. A message held only dynamically at its next eligible boundary can have a live blocker even if the saved receipt is still `accepted`; preserve the saved status and prove the current hold by eligibility and active phase, not by receipt status alone.

Obtain delivery eligibility and the blocker for the exact recipient, Run and Turn using the owner-side delivery and foreground state. Require a live current slot for that exact execution, not a launch flag, a generic `waiting` Turn, or merely an execution on the same Thread. Prefer omission for a message that has already applied, and an unknown reason for an unresolved hold, if facts race. Bound the work to the Turn's accepted inputs, the same rows the driver reads at a delivery boundary (see [#427](https://github.com/tya5/shellboy/issues/427)). The read is observational: subsequent application or shell completion may invalidate it immediately.

### Bounded wait for application

Add a `message_delivered` condition to existing `wait`:

```json
{"type":"message_delivered","message_id":"MESSAGE_ID"}
```

Satisfy only on durable application of this exact model-target message (`applied`), using Core's existing application boundary. This does not promise provider processing, a response, or a reply. Acceptance, staging, path/attachment evidence alone, or applying another message are not success. Already-applied model input matches immediately. A cancelled message or a message that becomes unavailable after registration never matches and produces `status: unavailable` if no condition matches. An unknown or unavailable target at initial validation returns `TargetUnavailable`; a known non-model target returns `InvalidArgument`. Preserve the existing ANY rule: an otherwise valid wait can return `status: satisfied` for another condition even when this message is cancelled or has become unavailable. Confirm application only if `matched` contains `message_delivered` with this exact message ID. Do not keep waiting for a known impossible target or return a hold hint in the wait. Reply observation remains `reply_received`.

Use the existing owner wait admission, deadlines, cancellation, and keyed read/subscribe/recheck discipline. Read the exact delivery identity using the existing ID lookup; register before the initial snapshot, publish after committed transitions relevant to this predicate (including cancellation and target loss), and recheck durable state rather than trusting a notification. No polling, full scan, new timer service, or sample-based delivery guess. Preserve the current wait envelope and distinguish a satisfied predicate from observation timeout and refusal; CLI exit success alone does not mean the message applied. Timeout has no effect on the input or the recipient.

## Exact intervention and release

| What the sender observes | Action |
|---|---|
| Accepted send, then `message_delivered` satisfied | Do not intervene. No reply is implied. |
| Wait timed out and `message get` proves `held_by.kind = foreground_shell` | Explicitly call `shell background --execution` for **only** the reported ID, if intervention is still wanted. |
| Response generation, Revisit, recovery, unknown, cancelled or no longer held | Do not background anything. Normal delivery, existing hold rules, or a terminal result apply. |
| Exact Background acknowledges `requested: true` | Only the exact request was accepted. The driver can still observe completion or Stop before consuming the release. The acknowledgement proves neither release nor application. Use the exact-message wait or read to confirm application. |
| Exact Background refuses or outcome is unknown | Inspect the original target and known receipt. Do not resend, automatically retry, or resolve to another execution. |

Send, wait, read and Background are separate operations. A message may apply naturally before the read, the named shell may finish, or the recipient may move on before Background. The operation revalidates the exact live target independently; it does not rebind a stale ID or guarantee message application. Any Thread on the connected owner may request Background for another Thread's shell, with no same-Thread, role, or ownership gate. Keep endpoint admission and exact execution, Run and live foreground checks. The current runtime dispatch resolves the target Thread and Run from the execution ID rather than the caller Thread; verify the full path with genuine Thread origin and remove any upstream Thread-based refusal for Background only. Person Background keeps the same interaction and uses the same neutral outcome. Foreground completion, interruption and shutdown keep their existing paths.

### Model-facing release result

Reuse the existing release envelope. Use the neutral `shell_backgrounded` for every explicit Background, including TUI person actions and Thread-origin calls. No person marker is accepted. Historical person-specific reasons remain saved as written. The reason says *why the wait returned*, not why the intervening party acted. Do not add the intervening Thread ID to the result: this is an execution action, not proof of who sent any particular message. Normal input delivery carries the message and its own sender provenance.

```json
{"status":"running","execution_id":"EXACT_EXECUTION_ID","reason":"shell_backgrounded","cancel_remaining_calls":true}
```

`running` describes the release, not a promise the execution will still be live on readback. For new results use `previous_wait_released_by_shell_background` for the remaining-tool prerequisite, without attributing the action to a person. Keep `human_turn_interrupted` distinct. Retain `additional_input` in historical records; it remains the legacy outcome while `now` is accepted, and is not emitted for the new explicit send-plus-Background flow.

## Retire immediate delivery

Remove `now` from new send/ask wire input, CLI options/help, and the TUI immediate-send gesture. No alias to `next`, hidden fallback, or implicit wake from ordinary input. Studio should use the message's typed fact for its pending row (#159), and offer ordinary Send and a separate explicit Background against the named execution; never auto-release on a timeout. Preserve default `next` and Revisit `complete` unchanged.

Historical `now` inputs, delivery metadata, receipts and `additional_input` results stay readable and exportable. Before removing the runtime path, prove that already accepted outstanding input reaches its existing legitimate boundary and that persisted operation replay and argument identity remain lossless. Historical decode/replay is not permission to admit a fresh `now`. Do not rewrite receipts or silently convert accepted requests. Hold removal if these checks cannot be met.

## Agent guide deliverable

Ship matching `docs/guides/agent-guide.md` text and a `docs/guides/shell.md` cross-link with each implementation unit. Update the CLI reference and quick lookup as features land; remove `--deliver now` advice when removal lands. The new field, predicate, and neutral release reason below are proposed syntax.

**Routine reports need only Send.** If timely application matters, track the message. Check `matched` for this exact message condition, not the top-level status or CLI exit code alone. On timeout, read `held_by`; intervene only if it names the exact foreground execution and intervention is still useful.

```text
Sender -> shellboy message send --thread RECIPIENT --text 'Skip the optional test and continue the review.'
Observed: accepted with message_id SKIP_MSG.
Sender -> shellboy wait --conditions-json '[{"type":"message_delivered","message_id":"SKIP_MSG"}]' --timeout-ms 30000
Observed: the wait timed out; application is not confirmed.
Sender -> shellboy message get --message SKIP_MSG
Observed: held_by is {"kind":"foreground_shell","execution_id":"TEST_EXEC"}.
Sender -> shellboy shell background --execution TEST_EXEC
Observed: requested: true. This acknowledges the request only.
In this example the driver consumes the release.
Woken model: shell_backgrounded for TEST_EXEC; the skip instruction arrives at the normal boundary.
Woken model -> shellboy shell get --execution TEST_EXEC
Woken model -> shellboy message get --message SKIP_MSG
Woken model: this is the running optional test; the instruction authorizes skipping it.
Woken model -> shellboy shell stop --execution TEST_EXEC
Woken model: confirm terminal state with wait and shell get, then report skipped, not passed.
```

**Read, then choose.** Use `shell get` for current identity and terminal facts, `message get` for a known message ID, and bounded `shell read` for progress. If the next step needs the result, observe the already-backgrounded execution with a bounded wait. If it does not, continue useful work and return to the same ID later. Stop only when appropriate for that exact work; neither input arrival nor a silent shell proves a hang. Keep the work ID, not the ID of a short observation shell. Do not poll tightly or relaunch to resume waiting.

**For unknown durations, start in the background and bound the observation.** Use `background: true` on the shell tool. For the returned ID, use:

```sh
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"WORK_EXEC"}]' --timeout-ms 30000
shellboy shell get --execution WORK_EXEC
shellboy shell read --execution WORK_EXEC --limit-bytes 65536
```

Choose the wait interval for when it is useful to reconsider, not to impose a process deadline. On observation timeout, inspect, wait again if dependent, or do independent work. After an end event confirm the saved exit code and output completeness. No tight polling or blind restart. The guide should turn each scenario below into an observation transcript, using the same work ID throughout:

| Worked case | Launch, observation, and decision |
|---|---|
| Tests | Launch `cargo test --locked --lib` with `background: true`. Wait on `TEST_EXEC` for 30 seconds; a routine report arrives while the test runs. Read `TEST_EXEC`; it is progressing and the next step depends on the result. Wait again on `TEST_EXEC`, then read saved exit status. |
| Build | Launch `cargo build --locked` in the background. A 60-second wait on `BUILD_EXEC` times out. `shell get` and bounded `shell read` show linking. Review docs meanwhile, then wait on the same ID and inspect the terminal result. |
| Install | Launch an approved noninteractive install in a disposable environment in the background. First wait on `INSTALL_EXEC` times out; the output shows download progress. Let it continue, resume observation with the same ID, and verify the exit and expected installed state. Do not run a second install. |
| Migration | Launch an approved migration against a backed-up disposable copy in the background. The wait on `MIGRATE_EXEC` times out; read progress and check recovery procedure before considering Stop. If it is still progressing, wait again, then verify the saved result and the resulting schema/data. |
| Server | Launch `./dev-server` in the background. Observe `SERVER_EXEC` with a short bounded wait if checking for early exit; timeout means it did not end during observation, not that it is ready. Inspect output, verify readiness via the documented health check, retain the ID, and stop it explicitly when finished. |

## Independently landable units

1. **Typed message readback.** Add `held_by` to the relevant live message reads without changing delivery or wake behavior. Witness exact foreground attribution, another running shell on the same Thread, generation, Revisit/recovery, application and cancellation, and stale or ambiguous live state. Studio #159 can adopt the typed pending-row line independently. Guide: interpret each variant and do not guess a target.
2. **Delivery observation.** Add `message_delivered` through existing wait admission. Witness already-applied and pending input, application across registration/commit races, unrelated input, timeout, cancellation and target loss, and no delivery side effects. Include a mixed wait where another condition matches while the cancelled message does not: the result is `satisfied`, but `matched` excludes the exact message condition. Guide: retain the message ID and use bounded wait without confusing application with replies.
3. **Explicit exact-ID intervention.** Verify cross-Thread Background, remove a caller-based refusal only if found, and use the neutral reason for all explicit Background actions. Witness only the named wait released, its process still running, later calls cancelled, stale IDs refused and normal input application. Keep `now` available in this unit. Guide: send, wait, read the held reason, Background only its ID, and inspect before deciding. Include the worked background-and-wait examples.
4. **Remove `now`.** Once the replacement is available, remove new `now` sends/asks from CLI/wire/TUI and remove the legacy wake path once outstanding delivery and replay witnesses pass. Preserve history and `complete`; no alias. Observe ordinary send not waking. Guide: remove old advice and retain the separate explicit steps. TUI and Studio retain exact-ID person Background; Studio adopts removal with the landed contract. Number public wire changes at landing; this design does not authorize a pin.
