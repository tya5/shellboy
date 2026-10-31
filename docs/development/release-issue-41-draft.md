# Conversation redesign release notes (draft)

Publication is not authorized by this document. Include only increments actually
admitted to main, with their exact source and validation record. E is awaiting
separate approval; F and G are local preparation.

## Storage upgrade notice for F

Before first starting a version containing F, stop Shellboy and back up the entire
data directory, including any SQLite database, WAL and SHM files. The local F
candidate advances conversation schema15 to16 with a parent-Thread index; a
later F lookup-index increment may advance it again. Confirm the final migration
numbers against main immediately before admission, then replace this draft's
numbers before publishing release notes. Standalone RAG remains schema3.

The upgrade preserves saved conversations but older binaries reject the newer
conversation schema. To return to an older binary, use the pre-upgrade backup;
do not edit the migration ledger or user_version manually. No user data is
upgraded merely by preparing or reviewing this code.
