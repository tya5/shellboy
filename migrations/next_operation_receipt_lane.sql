-- SQLite ADD COLUMN with CHECK also checks legacy columns. The new default is
-- always valid; leave pre-existing corruption for the unchanged startup quick_check.
PRAGMA ignore_check_constraints=ON;
ALTER TABLE operation_receipt ADD COLUMN lane TEXT NOT NULL DEFAULT 'normal' CHECK(lane IN ('normal','wait'));
PRAGMA ignore_check_constraints=OFF;
INSERT INTO schema_migration(version,name) VALUES(60,'next_operation_receipt_lane');
PRAGMA user_version=60;
