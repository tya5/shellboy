-- Exact accepted Turn input, in delivery order (rowid breaks timestamp ties).
CREATE INDEX receipt_accepted_turn ON receipt(turn_id,created_at) WHERE state='accepted';
INSERT INTO schema_migration(version,name) VALUES(73,'next_accepted_receipt_turn');
PRAGMA user_version=73;
