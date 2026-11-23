ALTER TABLE operation_receipt ADD COLUMN lane TEXT NOT NULL DEFAULT 'normal' CHECK(lane IN ('normal','wait'));
INSERT INTO schema_migration(version,name) VALUES(57,'0057_operation_receipt_lane');
PRAGMA user_version=57;
