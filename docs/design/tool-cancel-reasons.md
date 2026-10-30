# Structured cancellation of later shell calls

Issue #56 changes the explanation of existing serial tool-group cancellation.
It does not change which calls run, their ordering, foreground release, stop
precedence, persistence, or replay. Time-based foreground backgrounding remains
explicitly deferred. No configuration, table, state, event, worker, queue,
public operation, or dependency is added.

Each unexecuted cancelled call saves the ordinary function_call_output result
with `status: "cancelled"`, `executed: false`, a stable machine-readable `reason`,
and a separate human-readable `message`. Cancellation caused by an earlier call
also saves `prerequisite`, an object containing that call's `call_id` and its
known `execution_id`, `state` (the observed result status), `exit_code`,
`output_complete`, and `wait_release_outcome`. Unknown facts are omitted; no OS
liveness query or later snapshot is fabricated. In particular `state: "running"`
means observed running when its foreground wait was released, not a promise that
the process remains alive when another consumer reads the history.

| Reason | Meaning |
| --- | --- |
| `previous_call_failed` | The prerequisite failed or stopped, or shell admission/execution returned a non-storage error. |
| `previous_output_incomplete` | The prerequisite's saved output is incomplete. |
| `previous_wait_released_by_new_input` | New accepted input released the prerequisite's foreground wait; that shell was still running. |
| `previous_wait_released_by_human_background` | A human released the prerequisite's foreground wait; that shell was still running. |
| `previous_wait_released` | The shell's saved CLI wait-release outcome halted the group; preserve that outcome separately. |
| `tool_group_validation_failed` | Preflight rejected another call before any OS side effect. |
| `owner_shutdown_before_call` | Owner shutdown closed new starts before this call. |
| `run_stopped_before_call` | Run cancellation was observed before this call. |
| `human_turn_interrupted` | A human interrupted this Turn; unstarted calls remain unexecuted, while a foreground result with observed running state retains its continuing execution ID. |
| `previous_turn_interrupted` | The prerequisite foreground wait was released by human Turn interruption. |

The two Turn-interruption reasons are added by #52. Their UI labels distinguish
human Turn interruption from full Run stop; the saved result status still
determines whether this call ran or a prerequisite continued in the background.

Classification follows the existing halt decision. Explicit foreground-release
reasons precede output/terminal classification; otherwise incomplete output
precedes other failures. Owner shutdown and Run cancellation still override an
earlier local reason before each call. Preflight's own invalid calls keep their
existing error result; other cancelled calls receive the structured explanation.
Storage errors and admission OperationConflict still propagate rather than
manufacturing results or completing a pre-existing call.

The prerequisite result itself is unchanged, including `additional_input` and
`human_backgrounded`. Completed results are persisted through the existing
save_tool_result transaction and replayed without rewriting historical results.
Frontends project reason/message/prerequisite from the saved results, retaining
raw details. The existing group-completion Notice still reports only its current
foreground-release and cancellation-count facts.

Models should inspect continuing execution IDs with shell get/read/wait rather
than duplicate a command whose foreground wait was released. Failed calls need
inspection of their retained output before a retry; incomplete output is not a
fabricated nonzero exit. Synthetic temporary-store/loopback tests must verify
both saved results and absence of a shell_execution for the cancelled call.
