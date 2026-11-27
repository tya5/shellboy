# Shellboy Core v0.18.0

## Interface changes

- **Read saved artifacts (#445, #458):** `artifact read` returns one regular file from an exact artifact version as base64. A `file:` artifact reads the saved file itself; a `shellboy-app:` folder takes a relative `--path` within that folder. Remote and network-share paths are not supported. On Windows, the CLI saves local verbatim drive paths in ordinary drive-letter form so the resulting artifact URI can be read back.
- **Watch message delivery and shell placement (#423, #437):** `message get` can report why a message is held, including the exact execution ID when it is blocked by a foreground shell. Use `wait` with `message_delivered` to observe application. Running shell reads report `foreground`; moving a shell to the background changes that flag without stopping the shell or attributing the action to a particular person.
- **Send at a safe boundary (#423):** New sends and asks no longer accept `--deliver now`. Default delivery uses the next safe boundary; `--deliver complete` is still available. An ordinary send does not release a foreground shell wait. Use `shell background` on the exact execution if you want the Turn to continue. Saved metadata and exact replay of older operations keep their original meaning.
- **Keep reply duties open while asking back (#433, #434, #438):** An owing Thread that asks a follow-up can keep the original duty open as `awaiting_own_ask`. The saved blocking Ask identifies what it is waiting for. Saved history also shows the recorded sender of named Asks from Threads when available.
- **Observe exact `exec` outcomes (#441, #446):** Reply mode waits while the duty is open. Each observation uses one owner wait for the remaining deadline and reads its outcome from that wait's snapshot. Timeout or interruption exits with code 5 and does not stop the target. `recovery_wait` exits with code 4; JSON or schema mismatch after repairs exits with code 3.
- **Inspect work and recover safely (#311, #421, #431, #444, #452):** Start reading near the end of saved shell output with `shell read --tail`, then continue using its cursor. `operation get` has a typed receipt result; check it before resending uncertain input. A Run still stopping can report a saved stop-settlement failure in `run get` and in work `status`; an accepted stop is not proof of settlement. A shell list item without saved command text omits `command_characters`, and `run_id` can be null.
- **Schedule commands and notices (#449):** Supply the scheduled program as one explicit command. Recurring schedules can opt into `--notify skipped`: the owning Thread receives a notice when an occurrence is skipped because the previous execution is still active. Notices are off by default.

## Terminal UI

- **More room for the conversation:** Empty work and held-input sections no longer take up conversation rows. Markdown rendering retries for omitted content stay within the loaded history window (#432).

## Guides and reliability

- **Shell starts account for available descriptors (#450):** On Unix, admission checks live descriptor use, the cost of starting the shell, and headroom for owner IPC. A refused start reports `descriptor_budget` without launching the command or retrying it automatically. This applies to Run shells, services, and scheduled work.
- **Storage maintenance (#450, #451):** Unchanged instruction snapshots are shared, and unreferenced snapshots can be collected. Raw output can age out without an explicit collect, even below the size cap. Cleanup reconciles pins left by ended work and collects eligible older schedule/service executions without removing the latest result from the definition.
- **More reliable regression tests (#442, #447, #454):** Tests check saved outcomes and public reads instead of private bookkeeping, and use controlled observations for deadline and shutdown cases. These are test changes, not new runtime behavior.
- **Guides and examples (#438, #439, #456):** The README includes a terminal demo. The guides explain when to delegate with an Ask, how to reuse project procedures and existing skills or MCP servers, and how external IPC clients can recover an uncertain result.

## Upgrade notes

- **Backup and compatibility:** Back up the entire data directory before starting the new owner. Upgrade the owner, CLI, and RPC clients together. Database migrations 66 through 69 cover Run stop diagnostics, open reply duties, schedule notices, and instruction-snapshot retention.
- **Protocol: 36 -> 47.** Versions 37-40 cover delivery observation, the exact Background result, the operation receipt result, and the shell output tail. Versions 41-44 remove immediate delivery and add Run stop diagnostics, shell foreground placement, and the open ask-back state. Versions 45-47 update shell list fields, schedule notices, and artifact content reads.

| Surface | Before | After |
| --- | --- | --- |
| New Send and Ask delivery | `--deliver now` / `deliver: "now"` accepted | Not accepted; use default safe-boundary delivery or `complete` |
| Open reply duties | `pending` | `pending` or `awaiting_own_ask` |
| Shell list without saved command text | `command_characters: null` | `command_characters` omitted; do not interpret omission as zero |
| `schedule.set` | Description required | Supply description, `notify`, or both |
