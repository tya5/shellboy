# Thread list state filter (#378)

`thread.list` accepts optional `state`: `idle`, `live`, `stopping`, `preparing`, `running`, `waiting`, `recovery_wait`, `completed`, `stopped`, or `failed`. It matches the existing row `state` before paging, not historical failures or the separate activity phase. Keep the same state when continuing with `next_cursor`. The filter requires updated order and cannot be combined with `thread_ids` or `awaiting`.
