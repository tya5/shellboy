-- Valid v1 fact rows are converted by the startup-only Rust helper in this transaction.
INSERT INTO schema_migration(version,name) VALUES(46,'0046_scope_identity_v2');
PRAGMA user_version=46;
