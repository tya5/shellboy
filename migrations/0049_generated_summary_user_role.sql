-- Native startup follows this ledger step with a provenance-checked Rust
-- payload conversion in the same IMMEDIATE transaction. SQL-only stagers must
-- fail closed when eligible rows exist because SQLite cannot recompute the
-- canonical SHA-256 body hash used by the runtime writer.
INSERT INTO schema_migration(version,name) VALUES(49,'0049_generated_summary_user_role');
PRAGMA user_version=49;
