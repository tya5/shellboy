# Protocol next (#399)

- `message.send.max_model_requests`: optional positive 32-bit request ceiling
  for a new Run. An active Run makes the send fail with `invalid_state` and
  `details.reason=model_request_ceiling_requires_new_run`.
- `run.get.model_requests` reports `{ceiling,used}` for limited Runs.
  `run.get.stop_reason` is `{kind:"model_request_ceiling_reached",requests_used:N}`
  when the next request is refused by the ceiling. Both fields are optional.
- Schedule command text can use the existing message operation. No schedule
  wire change. Persistent counters use migration `next_run_request_ceiling`.
