# Shellboy v0.11.2 Candidate

Preparation record, not a publication or installation announcement. This candidate
adds #296 opt-in saved item projection to the existing history.window read. Protocol
remains 5 and conversation schema remains 39; no migration or new operation.
The exact release SHA, independent review and hosted normal/all-OS CI are separate
qualification facts. Other schema-correction candidates are not silently included.

## User-visible contract

Call history.window with items=true to obtain ordered typed items on each ordinary
window node. Default false retains the legacy wire shape. Text carries canonical
item_identity, text and text_complete; tools carry verified execution IDs; reasoning
is a marker without secret text/summary/encrypted content. Unknown source is an
explicit unavailable barrier. This enables consumers to preserve body text and tool
order without interpreting assistant roles or JSON-looking prose heuristically.

Policy caps are node source 1 MiB before Rust body materialization, text prefix
16 KiB per item, 8 execution links per item, and 4 MiB sum of item text per response.
Arrays are whole-node atomic. From the first response-budget overflow node onward,
ordinary nodes remain present with items=null and response_budget. Rewindow using
existing ordinal/revision mechanisms. No item cursor, fragments or reassembly.

The existing history read lane uses inspection_work with a read-only/query_only
connection and caller transaction. Earlier candidate source reports that called it
a shared-writer route were incorrect; exact source tracing supersedes that claim.
No new reader/executor or correctness timeout is introduced.
Source guard tests measure Rust payload acquisition; they do not prove that SQLite
never touches source pages internally. These are policy limits, not measured
production capacity or real-provider performance claims.

## Qualification And Installation Boundaries

Focused synthetic producer, schema, guard, privacy and rewindow evidence is recorded
against immutable source/tree identities outside this document; previous compile,
red and interrupted runs remain preserved. The producer-schema regression work on
other branches is separate and not adopted by proximity to this release.

Before release require independent same-exact review, successful hosted standard
and all-OS CI, candidate artifacts/checksums, and the coordinated synthetic upgrade
checks. Actual installed-owner read acceptance remains a distinct postpublication
step. Do not call these pending steps complete from a source test.

Agent shells must not install, signal, stop, restart or roll back the running owner.
Only Claude outside the owner performs any coordinated postpublication owner
update. Current Studio stays installed until its own release gates; Core publication
alone is not Studio 0.2.2 rollout approval. No real provider, private user DB or
installed owner was used for the author fixtures.
