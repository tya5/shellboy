ALTER TABLE run ADD COLUMN stop_settlement_failure TEXT CHECK(stop_settlement_failure IS NULL OR (state = 'stopping' AND stop_settlement_failure IN ('storage', 'invariant')));
INSERT INTO schema_migration(version,name) VALUES(66,'next_run_stop_settlement_failure');
PRAGMA user_version=66;
