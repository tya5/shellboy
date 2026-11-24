ALTER TABLE run ADD COLUMN max_model_requests INTEGER CHECK(max_model_requests > 0);
ALTER TABLE run ADD COLUMN model_requests_used INTEGER NOT NULL DEFAULT 0 CHECK(model_requests_used >= 0 AND (max_model_requests IS NULL OR model_requests_used <= max_model_requests));
INSERT INTO schema_migration(version,name) VALUES(65,'next_run_request_ceiling');
PRAGMA user_version=65;
