-- Per-Thread overrides of the summary segment size and the maximum reduce rounds (#231). NULL uses the connection/[compaction] value.
ALTER TABLE thread ADD COLUMN segment_chars INTEGER CHECK(segment_chars IS NULL OR segment_chars BETWEEN 2000 AND 1000000);
ALTER TABLE thread ADD COLUMN reduce_rounds INTEGER CHECK(reduce_rounds IS NULL OR reduce_rounds BETWEEN 1 AND 16);
INSERT INTO schema_migration(version,name) VALUES(31,'0031_thread_summary_parameters');
PRAGMA user_version=31;
