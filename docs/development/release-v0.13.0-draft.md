# Shellboy v0.13.0

Per-Thread override of the model output limit. Protocol stays 5; the conversation
schema moves from 43 to 44 (migration 44, below).

## What changes for users

- `shellboy thread set --max-tokens N` sets a Thread's output limit; an empty string
  (`--max-tokens ''`) restores the connection's value. The TUI Thread settings form
  has the same field.
- Precedence: the Thread value, then the connection's `max_tokens`, then the existing
  default (32768 for `anthropic_messages`; Responses and Chat Completions send no
  limit when none is set). Only a positive integer is accepted; Shellboy adds no cap
  of its own. The value applies from the next model request, survives a connection
  change and is inherited by child Threads and forks.
- `thread get` and `thread set` report `max_tokens` (the Thread's own value or null)
  and `effective_max_tokens` (the value Shellboy sends).
- Summary requests keep the connection's `max_tokens`; their length is governed by
  `summary_max_chars`.
- A Thread value that clashes with the same native limit key in the target
  connection's `extra_body` is refused at `thread set` / connection switch with a
  message naming the key.
- The truncated-output error now points at both the Thread command and the
  connection setting; `thread request --attempt` reproduces the override.

## Migration 44

Adds the nullable `thread.max_tokens` column. No table is added and saved history is
not rewritten. As with every migration, an older binary cannot open a migrated
database: keep a backup of the data directory before the first start of this version.

## Qualification

Exact-SHA hosted CI on Ubuntu, macOS and Windows (all-OS), independent review, local
focused runs. Installation on a real data directory is a separate, explicitly
approved step (schema 43 to 44, backup first).
