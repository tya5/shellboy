# Unreleased notes

## Conversation retries (#315)

Conversations now retry strictly recognized untyped Responses SSE errors with
code 429 or 500-599 received before `[DONE]`, even after partial output, using the
existing retry budget and backoff. Partial output is kept for inspection but is
not replayed, unfinished tool calls are not executed, and upstream charges can
be duplicated. The conversation-only SSE exception does not apply to summaries.

**Configuration change:** Remove `response_retry_partial` from every connection
before upgrading. Both `true` and `false` are now rejected as unknown keys, with
no alias. Otherwise eligible conversation and summary failures no longer stop
solely because output was received. `response_max_retries = 0` still disables
automatic retries for the connection.
