#!/bin/sh
# Compare activity-index write VM steps on disposable in-memory databases.
set -eu
base=origin/main
migration=migrations/0023_activity_bounded.sql
sqlite=sqlite3
while [ "$#" -gt 0 ]; do
    case "$1" in
        --base) base=$2; shift 2 ;;
        --migration) migration=$2; shift 2 ;;
        --sqlite) sqlite=$2; shift 2 ;;
        --help|-h)
            echo 'Usage: scripts/dev/measure-activity-index-vm.sh [--base REF] [--migration FILE] [--sqlite COMMAND]'
            echo 'Runs synthetic in-memory SQLite VM-step measurements; never opens user databases.'
            exit 0 ;;
        *) echo "unknown argument: $1" >&2; exit 2 ;;
    esac
done
cd "$(dirname "$0")/../.."
sha=$(git rev-parse --verify "$base^{commit}")
test -f "$migration"
command -v "$sqlite" >/dev/null
version=$(git show "$sha:src/storage/current.sql" | sed -n '/^PRAGMA user_version=/p' | tail -1)
[ "$version" = 'PRAGMA user_version=22;' ] || { echo 'base must be the schema 22 snapshot' >&2; exit 1; }
indexes=$(grep -c '^CREATE INDEX ' "$migration" || true)
[ "$indexes" = 6 ] || { echo 'migration must contain exactly six single-line indexes' >&2; exit 1; }
printf 'base: %s\nmigration: %s\nsqlite: ' "$sha" "$migration"
"$sqlite" -version
for variant in before after; do
    printf '\n=== %s ===\n' "$variant"
    {
        printf '.bail on\nPRAGMA foreign_keys=ON;\n'
        git show "$sha:src/storage/current.sql"
        if [ "$variant" = after ]; then
            sed -n '/^CREATE INDEX/p' "$migration"
        fi
        cat <<'SQL'
INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('t','/','/AGENTS.md','p','m','2026-11-02T00:00:00Z');
INSERT INTO run(id,thread_id,state,created_at) VALUES('r','t','live','2026-11-02T00:00:00Z');
WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO turn(id,thread_id,run_id,state,created_at) SELECT 'old-turn-'||i,'t','r','completed','2026-11-01T00:00:00Z' FROM n;
WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) SELECT 'old-attempt-'||i,'old-turn-'||i,0,0,'p','m',0,'completed','{}','2026-11-01T00:00:00Z' FROM n;
WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) SELECT 'old-group-'||i,'old-attempt-'||i,'old-turn-'||i,'completed','2026-11-01T00:00:00Z' FROM n;
WITH RECURSIVE n(i) AS (VALUES(1) UNION ALL SELECT i+1 FROM n WHERE i<1000) INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,output_complete,started_at) SELECT 'old-shell-'||i,'r','old-turn-'||i,'old-op-'||i,'exited','/','{"spawned":true}','UTF-8','UTF-8','closed',1,'2026-11-01T00:00:00Z' FROM n;
.stats vmstep
.print turn_insert
INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES('current-turn','t','r','preparing','2026-11-02T00:00:00Z');
.print turn_running
UPDATE turn SET state='running' WHERE id='current-turn';
.print attempt_insert
INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES('current-attempt','current-turn',0,0,'p','m',0,'preparing','{}','2026-11-02T00:00:00Z');
.print attempt_inflight
UPDATE model_attempt SET state='inflight' WHERE id='current-attempt';
.print group_insert
INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at) VALUES('current-group','current-attempt','current-turn','running','2026-11-02T00:00:00Z');
.print group_completed
UPDATE tool_group SET state='completed' WHERE id='current-group';
.print shell_insert
INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at) VALUES('current-shell','r','current-turn','current-op','starting','/','{"spawned":false}','UTF-8','UTF-8','open','2026-11-02T00:00:00Z');
.print shell_spawn
UPDATE shell_execution SET shell_json=json_set(shell_json,'$.spawned',json('true')),state=CASE WHEN state='starting' THEN 'running' ELSE state END WHERE id='current-shell';
.print shell_exit
UPDATE shell_execution SET state='exited',exit_code=0,end_reason=NULL,output_complete=0,ended_at='2026-11-02T00:01:00Z',shell_json=json_set(shell_json,'$.completion',json('{"forced":false,"elapsed_ms":123}')) WHERE id='current-shell' AND state IN ('starting','running','stopping');
.stats off
SQL
    } | "$sqlite" -batch :memory:
done
