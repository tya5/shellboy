ALTER TABLE attachment_send ADD COLUMN omission_reason TEXT CHECK(omission_reason IN ('attachment_changed','attachment_unavailable'));
INSERT INTO schema_migration(version,name) VALUES(72,'next_attachment_omission');
PRAGMA user_version=72;
