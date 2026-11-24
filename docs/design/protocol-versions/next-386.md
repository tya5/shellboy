# Protocol next (#386)

## Connection test and transport causes

`connection.test` accepts optional `post_model`, equivalent to `--post-model`.
Omission preserves the catalog-only GET. Opt-in adds `conversation`, the result
of one paid synthetic conversation POST using a fresh HTTP client and no retries.
The top-level result still describes only the catalog GET.

A transport failure can include `transport_cause`, a redacted object with `kind`
and optional CONNECT `status` (only 407 is preserved by the underlying
connector). Saved model attempt errors and connection tests use the same causes.
No new database state or migration.
