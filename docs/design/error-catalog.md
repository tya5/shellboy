# External Shell Error Catalogue (#72)

## Placement And Authority

An optional Python-standard-library tool, `scripts/shellboy-errors.py`, owns a
rebuildable SQLite file `DATA/derived/errors.sqlite3`. Nothing in Rust Core,
owner startup, UI, protocol, conversation schema/migrations or #41 G changes.
The tool is loaded only when explicitly invoked. It connects to the explicit
`DATA/shellboy.db` using SQLite URI `mode=ro`, query_only and a bounded snapshot;
no owner API, provider, embedding or LLM is invoked. Raw source text is untrusted
input, never executable instructions or automatically installed guidance.

## Commands And Bounds

`python3 scripts/shellboy-errors.py --data-dir DATA build [--incremental]`
materializes bounded shell facts (including active rows for step ordering) for
at most10,000 executions. An exceeded cap
fails without publishing a partial catalogue. Each command is read up to8192
characters from at most16 KiB of request JSON (overlong commands are omitted,
not silently cut into a safe-looking preview); stderr decodes at most8 chunks/
4096 bytes per failure, with one bounded lookahead chunk. Identity fields
are capped at128 characters. Output pages cap at100 rows, examples at3 per
signature; snippets cap at240 characters, normalized commands at512. Builds use
one read transaction and an atomic replacement of only the derived file. The
source DB, its schema and canonical output are never changed. Existing active
WAL readers/writers are supported by normal read-only SQLite snapshot semantics.

The incremental option still checks the bounded current source set. It compares
sanitized facts, skips an unchanged snapshot and atomically replaces changed
snapshots, including absent/deleted facts;
it is not a rowid-only watermark (which would miss in-place terminal transitions,
GC and deletion). It recomputes pairs after refresh. Thus its read cost is bounded
by the same10,000 executions, not claimed proportional only to appended rows.
Deleting the derived file followed by build produces the same factual records.
Derived file format/version/options are checked; rebuild is explicit on mismatch.

`similar --execution ID` returns ranked signature matches and observed successful
commands, with exact execution/thread/Turn/group/call/result-node IDs. Missing
result nodes remain null; no node or similar name is guessed. `top [--since RFC3339]`
counts only failures since the cutoff: occurrences, distinct Threads, first/last
observation, paired fraction, mean steps and step distribution. Counts are over
the built snapshot, not a claim about all retained history. `similar` compares
saved shape fields, not semantic/LLM similarity. No text recommends an action.

## Signatures And Pairing

The deterministic signature consists of executable basename, first non-option
subcommand, exit code, terminal state/end reason, normalized first error line,
and cwd kind (repo root/relative, home, absolute/unknown). Paths, numbers, hex,
temporary directories, timestamps and UUIDs normalize to typed placeholders.
Categories: missing command/PATH, permission, network, syntax/arguments, timeout,
other. Unknown/missing/truncated metadata remains explicit and is not success.

A recovery candidate is a later terminal success in the same Thread AND Turn,
within N subsequent shell starts (default5, maximum20, including intervening
running shells). It must start after failure completion, with either matching
executable and similar normalized command, or identical cwd and a higher command
similarity. Select the nearest eligible success. This is an observed temporal
association, not proof of causation, a validated fix or permission to rerun it.
Overlapping background executions, another Turn or missing completion timestamp
cannot establish a pair. Equal timestamps are conservatively excluded.

## Privacy And Failure Semantics

Before storage/output: remove URL credentials/query values, environment assignment
values, secret-option values, bearer/basic credentials, private-key blocks,
common token/key formats, home paths and long opaque strings. Strip terminal
controls. Store only sanitized bounded command facts and normalized error lines,
never raw stdout/stderr or original argument JSON. Redaction is heuristic, not a
secret detector guarantee: do not publish a catalogue without human inspection.
Sanitization precedes truncation; missing oversized input is labelled. IDs remain
exact references but must satisfy finite opaque-ID validation. Unavailable source
or invalid/mismatched derived format fails closed with a credential-free error.
Source path and SQLite sidecars must never alias output; no symlink output path
or arbitrary output-file override is allowed. Failure leaves the previous derived
snapshot intact and cleans the tool's own temporary file.

Tests use current-schema synthetic temporary data only. They cover grouping,
causality/Turn constraints, counts, provenance, canaries, malformed/oversized text,
read-only source hashes, rebuild equivalence, incremental transitions/deletion,
limits and query plans. An opt-in synthetic10,000-execution measurement records
full/incremental durations without timing gates. All test watchdogs use60 seconds.

SQLite transaction scopes do not own connection lifetime. Explicitly close every
cursor and connection before replacing/deleting the derived file; retain the
original error if staging cleanup also fails, with the cleanup failure chained.
Never use collection, delay or retry to release handles. Synthetic fixtures obey
the same lifetime rule before temporary-directory cleanup.
