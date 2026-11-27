# Protocol next (#462)

## Named services and local endpoints

- `service.add` accepts optional `name`. `service.set` with `service_id` and `name` changes it, or clears it with `""`. Nonempty names match `[a-z0-9-]{1,64}` and are unique within their Thread (`invalid_argument`, reason `service_name_conflict`). Existing definitions are unnamed. `service.list` and exact-ID `service.get` return `name` or null without endpoint credentials.
- Authenticated local `service.endpoint` takes an exact `service_id` and returns `{port, token}` only for its current running execution. `target_unavailable/service_not_found` distinguishes missing definitions from `invalid_state/service_not_running` and `invalid_state/service_endpoint_unavailable`. The read creates no receipt or history. See [Thread-owned shell definitions](../thread-owned-shells.md).

## Storage

`next_service_name` adds optional `schedule.name` for services and a partial unique index on `(thread_id, name)`. Endpoint files are per execution and are not database state.
