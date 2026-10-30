ALTER TABLE turn ADD COLUMN interrupt_requested_at TEXT;
CREATE INDEX draft_state_thread_updated ON draft(state,thread_id,updated_at DESC,id DESC);
CREATE INDEX draft_submitted_source_revision ON draft(json_extract(body_json,'$._submission.source_draft_id'),json_extract(body_json,'$._submission.source_revision')) WHERE state='submitted';
INSERT INTO schema_migration(version,name) VALUES(14,'0014_turn_interrupt');
PRAGMA user_version=14;
