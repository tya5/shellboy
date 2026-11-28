# Experimental Thread context commands

Adds `context.add`, `context.list`, `context.get`, `context.set`, and
`context.remove`. The generated protocol schema advertises experimental operations
in `operation_stability`; unlisted operations have no experimental marker.

The owner stores at most 16 definitions per Thread, in insertion order, in
`context_command`. The conversation request owns each supervised process until
cleanup; cancellation prevents attempt admission. Stdout capture shares the
existing tool excerpt budget across definitions. The tail is never a history node.
Saved attempt inspection reads the admitted item, hash and zero-based position.
