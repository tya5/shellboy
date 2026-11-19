# Shellboy v0.13.1 (draft)

Reliability and history-migration follow-ups to v0.13.0. The protocol remains 5;
the conversation schema advances from 44 to 49.

## What changes for users

- Retryable conversation stream failures are retried only when the response facts
  prove that no assistant output was emitted. Missing or contradictory terminal
  metadata fails closed rather than authorizing a retry.
- Human questions are durably admitted to the canonical Thread history. Question
  request acceptance, durable receipt completion and later path admission remain
  distinct; answering is still a separate human reply.
- Automatic compaction honors the Thread's effective spill preference. For example,
  `shellboy thread set --thread THREAD --spill off` skips automatic spill while
  retaining the automatic summary route; `--spill ''` restores inherited behavior.
- Newly generated compaction summaries are saved with the user role. Migration 49
  converts only canonical generated summaries with positively verified provenance,
  preserving their text and source/provenance records.

## Conversation schema 49

Migrations 45-49 add the Thread spill-preference setting, update compaction scope
identity records, remove obsolete selection-generation state, admit previously
applied human-question receipts to the canonical history path, and convert eligible
canonical generated summaries to the user role. Ambiguous summary rows fail closed.
The upgrade is forward-only: an older binary cannot open a migrated database. Back
up the data directory before first startup with this version.

## Qualification and publication gates

The release candidate requires independent review and successful ordinary three-OS
Core CI on the exact candidate SHA. Then run `.github/workflows/release.yml` with
`release_tag=v0.13.1` and `candidate_ref=<exact main SHA>` to build/package the native
archives, run packaged smoke and installer checks, and retain candidate provenance.
That candidate-ref workflow does not create a tag or publish a Release. Verify exact
SHA, candidate assets/checksums, and all required gates before any separately
approved publication. A real data-directory upgrade is also a separate, explicitly
approved step (schema 44 to 49, backup first).
