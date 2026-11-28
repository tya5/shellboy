# Experimental Thread context commands

Adds `context.add`, `context.list`, `context.get`, `context.set`, and
`context.remove`. The generated protocol schema advertises experimental operations
in `operation_stability`; unlisted operations have no experimental marker.
`context.list` returns live pages in registration order. It uses the shared list
limits, may return a shorter page to fit the response, and returns a Thread-bound
`next_cursor` until the end. Restart paging after a definition change.

The owner stores definitions per Thread, in insertion order, in
`context_command`. The conversation request owns each supervised process until
cleanup; cancellation prevents attempt admission. Native launch refuses expired
commands before spawn (`timed_out`, `started: false`). The serialized tail reserves
its outcome metadata first, then divides the remaining excerpt budget equally
across definitions. If the metadata cannot fit, a fixed skipped fact replaces
the results and no commands run. The tail is never a history node.
Saved attempt inspection reads the admitted item, hash and zero-based position.
