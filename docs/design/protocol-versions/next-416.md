# Protocol next (#416)

## Record pages and work observations

Owner record lists echo the effective `limit` and typed `filters` alongside
`items` and any `next_cursor`. Literal selectors stay private in the echo.
Work `status` puts `limit` at top level, drops `filters.cursor` and
`filters.memory`, and hashes only the normalized selection and saved page facts.
Thread summaries emit explicit reply-count exactness and rename
`awaiting_replies_exact` to `awaiting_replies_count_exact`. History list/window
share excerpt markers and preserve existing counts with explicit exactness.
Command names, request selectors and cursor formats are unchanged.
