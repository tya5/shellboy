-- Display context usage (#141): exact newest matching completed attempt per
-- turn in one seek. Derived index only; implicit trailing rowid orders it.
CREATE INDEX attempts_completed_scope ON model_attempt(turn_id,profile_id,model_id) WHERE state='completed';
INSERT INTO schema_migration(version,name) VALUES(19,'0019_context_attempt_scope');
PRAGMA user_version=19;
