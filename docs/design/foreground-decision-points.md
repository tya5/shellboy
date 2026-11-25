# Explicit foreground shell release (#423)

## Non-goals

- No automatic wake on input, sender urgency, Core timer, expectation argument, or paused/resumable foreground state. Background is an actual release of the foreground wait, not a pause.
- No change to response-generation interruption, Run ownership, Stop permissions, or owner/endpoint admission. No new caller-Thread authorization rule for Background.
- No bundled input, output, or diagnostics in a new wake envelope. Read existing facts. No message-level `waiting_for` projection in this reset.
- No new queue, persisted wait mirror, or DB migration presumed. This revision is design only, with no client pin or behavior change.

## What wakes a model

The intervening party decides whether to release a foreground wait. The launching model decides what to do after release. Background leaves the exact execution running under its original owner; it does not detach it from the Run, stop it, or start it again.

| Event | Exact foreground wait exists | No exact foreground wait exists |
|---|---|---|
| Ordinary send or ask | Queues input for the next eligible boundary. Does not itself wake the model. | Normal delivery. No interruption of response generation. |
| Revisit (`complete`) | Keeps its current eligibility rules; Background does not promote it to ordinary input. | Existing hold and delivery rules. |
| Person chooses Background in TUI or Studio, or another Thread calls `shell background --execution ID` | Releases the wait with a typed reason and that execution ID. The process keeps running; existing cancellation of later unexecuted calls in the tool group stays. | Refuse the exact target as no longer foreground; never match a different execution. A receipt replay can report its original outcome. |
| Foreground execution ends | Existing terminal result with completion evidence. | Existing saved state and completion notices; there is no foreground tool to wake. |
| Model's `shellboy wait --timeout-ms` expires while observing a background execution | Not a foreground release or intervention. | The observation returns; the background shell keeps running. Read state before making another decision. |
| Turn interruption, Run stop, owner shutdown | Existing interruption and cleanup rules. Background does not override a committed stop or terminal state. | Existing behavior. |

### Message plus explicit Background

Send first using default delivery, confirm acceptance, then find the recipient's current foreground execution and call Background with that exact ID. Do not background on a failed send or while its acceptance is unknown. These are separate operations, not a transaction: the recipient can receive the input naturally before Background; the observed execution can end or be replaced before the call. If Background refuses the target or its reply is lost, inspect the original execution and any known operation receipt. Do not automatically re-send the message, retry the wake, or background a replacement. Message acceptance is not delivery, and Background acknowledgement is not process completion.

Any Thread may request Background on another Thread's foreground shell **on the connected owner**. There is no same-Thread, sender-role, or ownership check for this action. The current `ShellBackground` dispatch in `src/control/runtime.rs` resolves the execution's owning Thread and Run from the exact ID, rather than the caller's Thread, and `src/control/live.rs` matches that execution against the live driver slot. Keep those target-validity checks, not caller-based authorization. Verify the complete admitted path with a real Thread origin; if any upstream caller-Thread refusal exists, remove it for Background only. This does not grant access to a different owner or change other shell mutations.

### Model-facing release result

Reuse the existing release envelope. Preserve `human_backgrounded` for known person actions. Use the neutral `shell_backgrounded` for explicit commands not known to be person actions, including Thread-origin calls; do not infer human authority from an unlabelled caller. The reason says *why the wait returned*, not why the intervening party acted. Normal input delivery carries their message, if any.

```json
{"status":"running","execution_id":"EXACT_EXECUTION_ID","reason":"shell_backgrounded","cancel_remaining_calls":true}
```

`running` describes the release, not a promise the execution will still be live on readback. Treat both Background reasons as the same release for remaining-tool prerequisite reporting, without attributing a Thread-origin action to a person. Keep `human_turn_interrupted` distinct. Retain `additional_input` in historical records; it remains the legacy outcome while `now` is accepted, and is not emitted for the new explicit send-plus-Background flow.

## Find the target by a typed execution fact

Add `wait_disposition` to execution entries in existing `shell get`, `shell list`, and scoped `status` shell observations. Proposed values: `foreground` means the execution is the current model foreground wait for its exact live Turn; `background` means the execution is live and known to be running without a foreground wait, whether launched that way or explicitly released. Omit for terminal executions and when live attribution cannot be established. Absence is not `background`. Do not infer it from launch flags, timestamps, generic Turn `waiting`, the last listed shell, or a client-side join.

Derive this from the owner-side live foreground identity and execution facts already used to validate release. A stale or closed slot is not foreground; once a release is authoritative, do not publish that slot as still waiting. Scope the projection to the exact owner, execution, Run and Turn, and only the bounded executions already selected for the read. If a transition races readback, omit when evidence is inconsistent. Keep existing page shape and continuation; no full scan or new filter is needed. Clients can look for a `foreground` entry under the exact Thread and check its Turn; on a partial walk or ambiguous result, do not guess. The mutation rechecks identity independently of the observational fact.

## Retire immediate delivery

Remove `now` from new send/ask wire input, CLI options/help, and the TUI immediate-send gesture. No alias to `next`, hidden fallback, or implicit wake from ordinary input. Studio should replace any combined control with ordinary Send and separately explicit Background against the selected exact execution. Preserve default `next` and Revisit `complete` unchanged.

Historical `now` inputs, delivery metadata, receipts and `additional_input` results stay readable and exportable. Before removing the runtime path, prove that already accepted outstanding input reaches its existing legitimate boundary and that persisted operation replay and argument identity remain lossless. Historical decode/replay is not permission to admit a fresh `now`. Do not rewrite receipts or silently convert accepted requests. Hold removal if these checks cannot be met.

## Agent guide deliverable

Ship matching text in `docs/guides/agent-guide.md` and a cross-link in `docs/guides/shell.md` with each implementation unit. Update quick lookup, intervention and delivery guidance; remove the old `--deliver now` advice only when removal lands. The following is proposed guide text and examples. The typed read field and new release reason are not yet shipped.

**Intervene only when you need to release the wait.** Routine reports need only Send. For an instruction that cannot wait, send first; after acceptance, read the recipient's current foreground execution from their exact Thread. Background it explicitly. Do not interrupt a model response or stop a shell merely to deliver input.

```text
Intervening model -> shellboy message send --thread RECIPIENT --text 'Skip the optional test and continue with the review.'
Observed: input accepted.
Intervening model -> shellboy shell list --thread RECIPIENT --state running
Observed: TEST_EXEC is foreground for RECIPIENT's active Turn.
Intervening model -> shellboy shell background --execution TEST_EXEC
Woken model: the shell tool returned shell_backgrounded for TEST_EXEC; the new instruction arrives at its ordinary boundary.
Woken model -> shellboy shell get --execution TEST_EXEC
Woken model: this is the optional test, still running; the instruction authorizes skipping it.
Woken model -> shellboy shell stop --execution TEST_EXEC
Woken model: confirm terminal state with wait and shell get before reporting the check as skipped.
```

**Read, then decide.** Start with `shellboy shell get --execution ID` for current identity, phase, and terminal evidence. If a new instruction arrived, read it before acting and use `shellboy message get --message ID` when its ID is known. Then use `shellboy shell read --execution ID --limit-bytes 65536` for output if progress matters; retain its continuation. Do not treat lack of recent output as proof of a hang. Continue to use the work's execution ID, not the ID of a short shell running a read or wait command.

| Evidence and dependency | Choice |
|---|---|
| A routine report or wake changed nothing, and the next step needs the test result | Observe the same already-backgrounded execution with a bounded `shellboy wait`. Do not relaunch or try to make it foreground again. |
| A build is still progressing and you have independent work | Leave it running, do the independent work, and return to the same ID later. |
| The instruction authorizes skipping this exact command and stopping is appropriate | Stop the exact ID and confirm actual terminal outcome. |

**For unknown durations, start in the background and bound the observation.** Use `background: true` on the shell tool. For the returned ID, use:

```sh
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"WORK_EXEC"}]' --timeout-ms 30000
shellboy shell get --execution WORK_EXEC
shellboy shell read --execution WORK_EXEC --limit-bytes 65536
```

Choose the wait interval for when it is useful to reconsider, not to impose a process deadline. On observation timeout, inspect, wait again if dependent, or do independent work. After an end event confirm the saved exit code and output completeness. No tight polling or blind restart. Cast the guide examples as observation transcripts, with the same commands and the selected work ID throughout:

| Worked case | Launch, observation, and decision |
|---|---|
| Tests | Launch `cargo test --locked --lib` with `background: true`. Wait on `TEST_EXEC` for 30 seconds; a routine report arrives while the test runs. Read `TEST_EXEC`; it is progressing and the next step depends on the result. Wait again on `TEST_EXEC`, then read saved exit status. |
| Build | Launch `cargo build --locked` in the background. A 60-second wait on `BUILD_EXEC` times out. `shell get` and bounded `shell read` show linking. Review docs meanwhile, then wait on the same ID and inspect the terminal result. |
| Install | Launch an approved noninteractive install in a disposable environment in the background. First wait on `INSTALL_EXEC` times out; the output shows download progress. Let it continue, resume observation with the same ID, and verify the exit and expected installed state. Do not run a second install. |
| Migration | Launch an approved migration against a backed-up disposable copy in the background. The wait on `MIGRATE_EXEC` times out; read progress and check recovery procedure before considering Stop. If it is still progressing, wait again, then verify the saved result and the resulting schema/data. |
| Server | Launch `./dev-server` in the background. Observe `SERVER_EXEC` with a short bounded wait if checking for early exit; timeout means it did not end during observation, not that it is ready. Inspect output, verify readiness via the documented health check, retain the ID, and stop it explicitly when finished. |

## Independently landable units

1. **Typed execution readback.** Publish the owner-derived execution fact in get/list/status without changing send or release behavior. Test foreground, launch-background, post-release, ended, stale identity and omission under ambiguity, with bounded list/status reads. Guide: discover exact foreground targets without guessing and add the existing background-and-wait examples. Studio #159 may adopt the execution fact independently; do not claim message-level attribution.
2. **Explicit cross-Thread release.** Verify the existing operation with a genuine Thread origin and remove a caller-based restriction only if found. Preserve owner admission and exact-target validation; report neutral explicit-command release without mislabeling person actions. Test recipient-only wake, original process still running, later calls skipped, next-boundary input, stale Turn/execution refusal, and unchanged interruption and human behavior. Guide: send, find, Background, inspect, decide. Keep `now` working until removal. TUI and Studio preserve their explicit, exact-target person control; a client must not fabricate a human reason if provenance is unknown.
3. **Remove `now`.** Once the replacement is available, remove new `now` sends/asks across CLI, wire and TUI, and remove its special wake path when historical compatibility permits. Test accepted older receipts, readback/export and replay, new-input rejection, ordinary input not waking, and unchanged Revisit and interruption. Guide: replace the old advice, explicitly describe non-atomic send and Background. Studio coordinates removal at protocol adoption, not a new urgency control. Number public wire changes at landing; this design does not authorize a pin.
