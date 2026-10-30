# Terminal program status (OSC 7501)

Adopt rev0.4 of https://www.superlogical.com/rex/docs/build/program-status
for issue54 / decision68 D-011. This is generic terminal metadata, not a receiver
integration. Default on; off emits nothing, including no shutdown clear/query.
Use existing base64, DB worker, bounded frontend read admission and update queue.
No new worker, process, timer, poller, table or dependency is introduced.

The existing saved-fact `activity::classify` is the only activity classifier.
Map all active phases to working, Recovery to blocked, Failed to error,
Stopped/Idle to idle. Completed is done only for completion after this UI session
started that is neither currently selected nor acknowledged by that UI. Existing
historical completions start idle: no new unseen work is invented on restart.
Viewed latest-completion identities are ephemeral, pruned when deleted or replaced,
and never persisted. The frontend keeps one seen entry per visited completion,
not every historical Run. UI acknowledges only a snapshot for its selected Thread.

A single coalesced read uses the existing finite read pool after saved-work or
selection changes, not a new periodic task. It streams all live Thread metadata
for exact aggregate counts without collecting history. Retain only the highest-
priority 32 nonidle children; root counts include overflow. Root precedence:
blocked > error > working > done > idle. Auth kind uses typed saved configuration/
credential errors or HTTP401/403, never text matching. No progress is guessed.
Titles use the existing display-title projection for retained children only.

Root has app=shellboy and count summary. Children use thread/compact UUID (hash
fallback for internal synthetic non-UUID ids). Each report replaces its full
record. Clean free text of controls/format/bidi chars, truncate UTF-8 safely to
192 title / 2048 msg bytes, standard-base64 encode; whole sequence <=4096 bytes.
Diff final encoded content, clear disappeared/idle/evicted children before new
records, and send id-less clear on normal exit. No heartbeat or feature query.
Model/tool output is read from saved sanitized presentation, never forwarded as
terminal protocol bytes. Tests must keep that boundary and frame separation.

Physical receiver qualification is separate from writer/PTY mocks. Unknown OSC
receivers should ignore reports; tmux may discard them. No product-specific
passthrough, agent command, environment detection or receiver process is added.
