# Protocol next (#381)

## Bounded history observations

`history.window` supplies `execution_count_exact` with its bounded direct-link
count. Window excerpts are admitted under a source budget before loading JSON;
`payload_chars` may be null even for retained sources. See
[Protocol](../protocol.md#bounded-history-window-and-body-reads-381).
