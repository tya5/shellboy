# Search documents and history

[Guide index](README.md) · [Find a guide](docs.md) · [Conversation history](history-and-search.md)

`shellboy rag` is a search CLI that works without starting the conversation application. It does not generate answers to questions. Start with `shellboy docs search --query 'search'` to find operation guides. The bundled guides are English. Queries are not translated automatically.

The standalone RAG store uses schema 5. Readers and dry-run require the current schema and do not migrate it. The writer owns schema preparation; the general schema-version/ledger mechanism is separate from a promise to support older releases. The standalone store does not modify the conversation DB.

## Register documents

```bash
shellboy rag init
shellboy rag source add /absolute/path/to/notes --name notes
shellboy rag source list
shellboy rag index --source notes --dry-run
shellboy rag index --source notes
shellboy rag search --scope source:notes --query 'recovery'
```

`rag init` creates a missing store or upgrades an existing store through the isolated startup migration. It does not register materials, index documents, or call an embedding API. If a read-only command reports an older RAG schema, run `rag init` once, then repeat the read.

### Exclude source entries

```bash
shellboy rag source add /absolute/path/to/notes --name notes --exclude '*.db' --exclude build
shellboy rag index --source notes --dry-run
# Explicitly clear exclusions on an existing source
shellboy rag source add /absolute/path/to/notes --name notes --clear-excludes
```

Repeat `--exclude` for up to 64 patterns, each at most 1024 bytes. Patterns are saved on the source, so you need not supply them for each index operation. Reregistering the same name/root without exclusion options preserves them; a nonempty set replaces all patterns, and `--clear-excludes` clears them. Matching is case-sensitive on every OS: `*.LOG` does not match `a.log`, including on macOS and Windows. Matching uses slash-separated paths relative to the root. A pattern without a slash also matches basenames at each level. A matching descendant directory skips its subtree; the registered directory root itself is never excluded by a glob. An explicitly registered file can still be excluded. Reading gitignore files and negation rules are unsupported.

Descendant entries starting with a dot, such as `.git` and `.DS_Store`, and the current RAG DB, its `-wal`/`-shm`/`-journal` files, and writer lock are excluded by default. An explicitly registered hidden root itself is respected as an intentional selection. Symlinks are not followed. If a registered root is replaced by a symlink, it is reported as `source_unavailable`; all updates and removal detection for that source are skipped, retaining its previous searchable revisions as stale. Unsupported formats are excluded rather than failed. Inspect `skipped`, per-source `selection` counts for targets/excluded/unsupported/symlinks, the extension breakdown, and up to 20 examples. Each source is limited to 10,000 supported documents, 100,000 visited entries, and depth 64.

After a complete directory traversal, previously indexed documents now excluded or deleted leave the active searchable revision and count as `removed`. Original files are not deleted. A read/extraction failure preserves only that file’s old revision as `stale` (or records `failed` if it has no old revision); it does not block removal of other unseen files. Incomplete traversal, including an unavailable root or a traversal limit, skips **all file updates and removal detection** for that source. Provider-breaker deferred work also suppresses removal detection until you retry.

Windows document keys have used slash separators since v0.1.6. The first complete synchronization removes old backslash-key documents from the active index and registers the current slash keys with new document IDs. You do not need to recreate the database. Old keys and IDs do not identify the replacement documents for `inspect --id`; no legacy lookup or ID/revision preservation is provided. POSIX backslashes remain literal filename characters.

Keyword indexing/search is the default and needs no API connection. Registration and dry-run do not call an API. `rag --db PATH` selects only the standalone knowledge DB, not history, the owner, or the embedding configuration. Without `--db`, the knowledge DB is `rag.db` under the selected data directory. Choose the data directory with `--data-dir`, then `SHELLBOY_DATA_DIR`, then the OS default. `--config` selects embedding configuration; its TOML `data_dir` does not select either DB. Running from a project folder does not auto-discover `knowledge/rag.db`. Use the same DB options for later operations on that store.

```sh
# Project materials only; no conversation history is searched.
shellboy rag --db ./knowledge/rag.db search --scope source:notes --query 'release checks'
# Deliberately add history from a specified conversation data directory.
shellboy --data-dir /path/to/conversations rag --db ./knowledge/rag.db search --scope source:notes --scope history --query 'release checks'
```

Identify a source by the ID returned by source add or its name. Register a canonical path only once per knowledge DB; a different name for that path is rejected with the existing name. Existing registrations are not merged or removed automatically. Originals are not copied; the index and extracted text are saved. Even if an original changes after synchronization, get returns the saved revision used by the search. When a provider failure pauses a sync, unverified retained revisions are marked `stale` with `provenance.reason: embedding_paused` in search. Retry synchronization to verify them.

```bash
shellboy rag status
shellboy rag inspect --id DOCUMENT_ID
shellboy rag source remove --source notes
```

Ordinary synchronization can retire a revision from search, but does not delete its saved embeddings. Identical chunks can reuse them when indexed again with the same profile, even after editing, excluding, or removing the original file. `rag source remove --source NAME` explicitly deletes the named registration and its cached embeddings that are not shared with another registered source. It never deletes original files or other sources' embeddings. Older stores can be upgraded with `rag init` without a provider call; existing vectors whose source cannot be recovered are retained, not assigned to an unrelated source. When synchronization fails and retains an old revision, results report `stale` and the reason. Temporary inability to access the original directory is not treated as document deletion.

## Semantic search and multiple scopes

```bash
shellboy rag index --source notes --mode semantic
shellboy docs index --source shellboy --dry-run
shellboy docs index --source shellboy
shellboy rag search --query 'recover after restarting' --scope source:shellboy --scope source:notes --mode hybrid
shellboy rag search --query 'earlier decisions' --scope history:THREAD_ID --mode keyword
# Search every conversation only when that is intended:
shellboy rag search --query 'earlier decisions' --scope history --mode keyword
```

semantic/hybrid use the existing `[embedding]` and connection settings. Select configuration with `--config PATH`. A missing required API connection produces an explicit error rather than silently switching to keyword search. Repeat scopes to select `history:THREAD_ID`, global `history`, or `source:REGISTERED_ID_OR_NAME`. Without scopes, registered material is searched. Conversation history is included only when explicitly selected. History access is read-only: it neither starts/recovers the conversation application nor reindexes it.

The old `--scope docs` is retired. Run `shellboy docs index --source shellboy`, then search `--scope source:shellboy`. The standalone store upgrade removes the old built-in source but keeps cached embeddings; identical chunks can reuse them. The docs dry-run previews guide counts and bytes without initializing the store or exporting material. Search does not automatically embed documents. Query embeddings for semantic or hybrid search may incur API costs.

If hybrid keyword search succeeds and the only confirmed semantic issue is an uncreated index, unindexed current profile, or missing embeddings, it returns keyword results with `ok:true`, exit code 0, and `result.degraded:true`. `scope_status` reports each scope's keyword/semantic usage, reason, missing document/chunk counts, and recovery commands in `next.steps[].command` with explicit source selection. Repeated degradation reasons appear only in `warnings`; `scope_errors` is reserved for actual scope failures. Unknown missing counts are null. Invalid/missing configuration, authentication, communication, DB/identity mismatches, and similar faults remain actual errors. Explicit semantic searches do not fall back to keyword. `shellboy docs` keyword search remains available without a DB or API.

Result excerpts are not full text. Use the opaque `reference` to retrieve the saved indexed text:

```bash
shellboy rag get --ref REFERENCE --limit-bytes 16384
shellboy rag get --ref REFERENCE --limit-bytes 16384 --cursor CURSOR
```

For the current original, run the hit's `owner_read` command instead and compare the returned fingerprint with `indexed_fingerprint`. File owner reads return `text` for UTF-8 or `bytes_base64` for binary originals, with a `next_cursor` when more bytes remain. These reads do not substitute the saved extraction if the original is missing.

The saved body is in `result.text`, with `next_cursor` for continuation. Keep using the same reference. A reclaimed saved revision returns `revision_unavailable`, never the current revision's text as a substitute. Document retrieval uses `provenance.retrieval_unit=block` (an extracted block such as a page or sheet); a guide's unit is the entire document, and history's unit is the referenced body kind. get offsets are UTF-8 bytes within that retrieval unit. Location `source` is the original location reported by the backend; `chunk_start_bytes/end_bytes` describe saved extracted text, not byte offsets in the original file.

Actual failures in some scopes return partial results, per-scope errors, and a nonzero exit code. Only the confirmed not-ready warnings described above count as degraded success. Within one search, scopes with the same provider request identity (connection, endpoint, model and dimensions) reuse a successful query embedding. A later search may charge again. Distances from different embedding spaces are not comparable, so combined search fuses ranks. Multiple scopes do not form one simultaneous snapshot. History keyword search scans through its initial watermark using short reads and validates references by revision/hash. Search again to include newly appended history.

Search results are limited to 100 items and 512 KiB; check truncated when the output cap is reached. Synchronization returns at most 50 failure entries, with total count and truncation reported separately. Shared authentication, balance, or rate-limit failures stop further embedding calls within that execution and report remaining work as deferred. Three ordinary communication failures in the same execution also stop further calls. After addressing the problem, explicitly run the same index operation again to reuse saved successes. There is no infinite retry.

## Convert PDFs, Office files, and images externally

RAG folder synchronization handles UTF-8 text/Markdown. For conversion, the model uses shell to inspect available tools and chooses one suitable for the material. Ask the user before installing another tool; corporate environments may have installation rules. Shellboy neither downloads/runs converters nor maintains a converter allowlist.

1. Convert the original into Markdown/UTF-8 text using an external tool.
2. Register the converted body and metadata with the original file specified.
3. Explicitly build a semantic index if needed.

```bash
shellboy rag ingest --id report-1 --source-file /absolute/path/to/report.pdf \
  --kind extracted --text-file /absolute/path/to/report.md \
  --metadata-file /absolute/path/to/meta.json
shellboy rag index --source derived --mode semantic
shellboy rag search --scope source:derived --mode hybrid --query 'main findings'
```

Metadata must be a JSON object of at most 64 KiB. Record the converter name/version, generation time, original hash, and other known information. Example fields:

```json
{"tool":"chosen-converter","tool_version":"recorded-version","generated_at":"2026-10-28T00:00:00Z","source_format":"application/pdf","source_hash":"hash-observed-by-workflow","review_required":true}
```

Use `extracted` for tool-converted text. If a model reads scans or diagrams and produces interpretation or a summary, use `llm-explanation`/`summary` and record `model` in metadata too. Attaching the original is an alternative for that purpose; support depends on the endpoint/proxy/model. Search and get expose `kind` to distinguish close-to-source conversion from model interpretation. Registering only a converted file with `source add` does not retain its relationship to the original; use ingest when derived text needs provenance.

`--text-stdin` is also supported. Derived text is limited to 16 MiB of UTF-8. The original must be a regular file and is hashed incrementally, so it may exceed 16 MiB; its contents are not saved in RAG. Distinguish `provenance.source` fields: `original_path`, `observed_original_hash` (observed at ingest), `original_hash_claim` (declared in metadata), and `derived_body_hash` (derived text). As `hash_observation` explains, the record does not prove which original was used to generate a summary or an atomic snapshot of a changing file. Saved text and provenance survive moving/deleting the original. There is no automatic revalidation or review workflow. Treat a RAG DB containing metadata and paths according to its appropriate storage/access scope.

## Check index results and update times

`rag index` JSON returns `changed`/`unchanged`/`removed`/`processed`, `failure_count`/`failures`/`failures_truncated`, `partial_failure`, `embedding_paused`/`deferred`, and retry guidance. Per-source results appear in `result.sources`. A new document with no successful revision is `failed`; one retaining an old active revision is `stale`. The failure list contains at most 50 entries, with total count and truncation reported separately. A supported file that fails on every run makes every `rag index` return `ok:false`, `partial_failure`, and a nonzero exit code, even when other changes and removals were applied successfully. This does not roll back successful work; inspect the counts and failures before retrying.

`rag status` projects per-source state from existing saved rows: `last_scan`, `last_changed_at` (the maximum content-creation time of active revisions), and failed documents' key/code/message. `semantic_missing` counts chunks missing embeddings for the current embedding configuration. Zero means no missing embeddings among current active chunks; it is neither the time indexing ran nor an API-call count. If configuration cannot be checked, it is null; inspect `semantic_configuration`.

Semantic dry-run calls no external API and reports `text_bytes`/`segments` and `embedding_may_charge`. It measures segmentation for supported UTF-8 text and counts unsupported formats separately in `selection.unsupported`. Shellboy has no embedded price list, so it does not calculate total cost.

### Final results from a background indexing shell

A model passes `command: "shellboy rag index --source notes --mode semantic"` with `background:true` to the shell tool and retains the **work execution ID** returned by that tool. If using explicit `--db`/`--config`/`--data-dir`, include those same options in the command.

```bash
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"WORK_EXECUTION_ID"}]' --timeout-ms 30000
shellboy shell read --execution WORK_EXECUTION_ID --encoding UTF-8 --limit-bytes 65536
```

A wait timeout means the work continues. After termination, read the index JSON inside read's `result.text` and inspect `ok`, counts, and failures. If output is long, continue with the returned `cursor`. Do not infer complete success from the exit code alone. The ID here belongs to the shell that ran the index command, not an acceptance CLI running `message send`, or an execution of wait/read itself. Raw output has a retention period; use `rag status` or `rag inspect` for later checks. No new operation-results DB or Core receipt is created. See the [Shell guide](shell.md).

## Search one conversation without embedding

Use `--scope history:THREAD_ID --mode keyword` to recover context after a trim,
without embedding configuration, an index, or an API call. The scope is the same
as `history search --thread THREAD_ID`: the current path and recursive sources
of completed summary, spill and omit replacements. It does not include unrelated
nodes or an entire parent/fork lineage.

A scoped hit's `thread_scope` contains `thread_id` and `on_current_path`. False
means the node belongs through replacement provenance, not that it is currently
sent to the model. `provenance.current_thread_paths` still lists only actual
current paths. When several explicit Thread scopes match one reference, the
fused result retains their memberships in `thread_scopes`; global search has no
implied caller Thread. A missing or deleted Thread reports a scope failure.
Use the returned reference with `rag get`, or the original node with `history
get`, before deciding whether to summarize or restore a smaller range. Restoring
a large range may recreate the failure that led to trimming.

Overlapping selectors such as `--scope history --scope history:THREAD_ID`, or a
parent and fork, retain all scope labels but do not multiply the same reference's
rank vote. The best rank contributes once per retrieval method; keyword and
semantic remain independent methods in hybrid search. If the selected Thread is
deleted while a query embedding is in flight, the scope reports
`thread_not_found`; this is not a transient index-change hint to retry.


`indexing.auto_history=false` controls automatic conversation indexing only;
it does not disable standalone source indexing or semantic/hybrid queries.
History-scoped semantic/hybrid queries still search retained vectors, but report
`degraded=true` and `scope_status.reason=auto_history_disabled` because newer
conversation nodes may be absent. Keyword history retrieval remains available, subject to the limit below.

Semantic history coverage also depends on `indexing.auto_kinds` (default: user messages, summaries and model replies). History-scoped semantic/hybrid results then carry a `history_kinds_not_indexed` warning and `scope_status.kinds_not_auto_indexed`; `--mode keyword` searches the originals of every kind. See [History and search](history-and-search.md#choosing-which-kinds-are-indexed).

History keyword search processes at most 2,000 nodes per call across all history
scopes. Unvisited scopes get a start cursor. If a scope is
`partial`, repeat the same query and one history scope in keyword mode with the
scope's `next_cursor`; a single-scope result also has `result.next_cursor`:

```sh
shellboy rag search --scope history:THREAD_ID --mode keyword --query 'earlier decisions' --cursor "$CURSOR"
```

Continue until the cursor is null. A page can be empty; result-count or byte
budgets can also end a page. Pages are ranked independently. With hybrid or mixed
scopes, the cursor restarts that history keyword stream from its initial boundary,
so deduplicate repeated references. The cursor is bound to the database, query,
and Thread. If an in-progress node changes, `cursor_expired` requires restarting;
changes between calls are not a frozen snapshot.
