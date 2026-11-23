# Protocol next (#381)

## Bounded history observations

`history.window` supplies `execution_count_exact` with its bounded direct-link
count. `history.get` may supply `body_unavailable_reason: "prune_indeterminate"`
when its prune check cannot prove readability within the budget. See
[Protocol](../protocol.md#bounded-history-window-and-body-reads-381).
