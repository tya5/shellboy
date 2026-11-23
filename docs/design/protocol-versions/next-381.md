# Bounded Thread list search and excerpts (#381)

## Bounded search, tags, cwd and excerpts

Ordinary updated-order `thread.list` uses one candidate seek for query, tags,
cwd, and state. The index selects the archive/cwd partition and seeks directly
past `(updated_at,id)` on continuation. A page examines up to 200 candidates
plus one lookahead; `next_cursor` means unexamined candidates remain, not that
another matching row is known. Follow it with the same filters even on an empty
page. Existing cursor scopes and watermark semantics are unchanged.

Tag intersection uses indexed membership probes per candidate. Search retains
its normalized substring and derived-title behavior, but rejected candidates
never read the summary, excerpt, or row observations. This does not itself
bound the existing title fallback for a Thread with no saved title.

Each `thread.list` row (including exact-ID and awaiting lists) looks for the
latest nonempty excerpt in the last 32 selected history nodes. When none is
found and an older node remains, `excerpt_complete: false` distinguishes an
unread prefix from an empty excerpt on an exhausted path. The flag is omitted
when true. The node budget is not a byte budget. No saved body changes.

Migration `next_thread_cwd_partitions` adds two partial cwd indexes. No new
state table or write-side excerpt projection is needed.
