# Protocol next (#352)

`wait` and `events.wait` share a separate pool of 32 permits. Plain owner `status` exposes `ipc_admission` with the connection cap and per-class limits and refusal totals. `wait` results expose `admission_wait_ms` and `admission_refusals`; only CLI `wait` retries definite, unapplied admission refusals, retaining the original cursor and deadline. See [IPC admission](../protocol.md#ipc-admission).
