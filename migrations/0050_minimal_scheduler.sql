CREATE TABLE scheduled_command(
 id TEXT PRIMARY KEY NOT NULL,
 command TEXT NOT NULL CHECK(length(command) BETWEEN 1 AND 16384),
 cwd TEXT NOT NULL CHECK(length(cwd) BETWEEN 1 AND 4096),
 cron_expression TEXT,
 at INTEGER,
 next_at INTEGER CHECK(next_at IS NULL OR next_at>=0),
 CHECK((cron_expression IS NOT NULL AND at IS NULL) OR
       (cron_expression IS NULL AND at IS NOT NULL))
);
CREATE INDEX scheduled_command_next_at ON scheduled_command(next_at,id);
CREATE TABLE scheduled_command_result(
 schedule_id TEXT PRIMARY KEY NOT NULL REFERENCES scheduled_command(id) ON DELETE CASCADE,
 scheduled_at INTEGER NOT NULL CHECK(scheduled_at>=0),
 finished_at INTEGER NOT NULL CHECK(finished_at>=0),
 status TEXT NOT NULL CHECK(status IN ('finished','overlap_skipped','capacity_skipped','launch_failed')),
 exit_code INTEGER,
 output_tail TEXT CHECK(output_tail IS NULL OR length(output_tail)<=4096),
 output_truncated INTEGER NOT NULL DEFAULT 0 CHECK(output_truncated IN (0,1))
);
INSERT INTO schema_migration(version,name) VALUES(50,'0050_minimal_scheduler');
PRAGMA user_version=50;
