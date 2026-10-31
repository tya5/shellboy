use super::*;
fn shells(db: &Connection, a: &ShellListArgs) -> Result<Value, ApiError> {
    super::shells(db, a, "test")
}
fn db() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(crate::storage::CURRENT_SCHEMA).unwrap();
    register(&c).unwrap();
    for t in ["t", "other"] {
        c.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES(?1,'/','','p','m','2026-10-31T00:00:00Z')",[t]).unwrap();
        c.execute("INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES(?1,?1,'completed','2026-10-31T00:00:00Z','2026-10-31T00:01:00Z')",[t]).unwrap();
        c.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES(?1,?1,?1,'completed','2026-10-31T00:00:00Z','2026-10-31T00:01:00Z')",[t]).unwrap();
    }
    c
}
fn shell_row(c: &Connection, id: &str, t: &str, at: &str, command: &str, exit: i32) {
    c.execute("INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,started_at,ended_at,exit_code)VALUES(?1,?2,?2,?1,'exited','/',?3,'UTF-8','UTF-8','closed',?4,'2026-10-31T00:01:00Z',?5)",params![id,t,json!({"command":command,"completion":{"elapsed_ms":1000}}).to_string(),at,exit]).unwrap();
}
fn args() -> ShellListArgs {
    ShellListArgs {
        thread_id: None,
        since: None,
        until: None,
        command_contains: None,
        state: None,
        exit_code: None,
        limit: 2,
        cursor: None,
    }
}
fn comp_args() -> CompactListArgs {
    CompactListArgs {
        thread_id: Some("t".into()),
        since: None,
        until: None,
        limit: 50,
    }
}
#[test]
fn normalized_shell_keys_preserve_offsets_nanos_leaps_and_unknowns() {
    watchdog(|| {
        let c = db();
        for raw in [
            "2026-10-31T00:00:00Z",
            "2026-10-31T09:00:00.000000001+09:00",
            "2026-10-31t00:00:00.1z",
            "2016-12-31T23:59:60.123Z",
            "2026-10-31T23:00:00+23:00",
            "0000-01-01T00:00:00+23:59",
        ] {
            let sql = format!("SELECT {},{}", time::seconds("?1"), time::nanos("?1"));
            let pair = c
                .query_row(&sql, [raw], |r| {
                    Ok((r.get::<_, i64>(0)?, r.get::<_, u32>(1)?))
                })
                .unwrap();
            let parsed = chrono::DateTime::parse_from_rfc3339(raw).unwrap();
            assert_eq!(
                pair,
                (parsed.timestamp(), parsed.timestamp_subsec_nanos()),
                "{raw}"
            );
        }
        for raw in [
            "bad",
            "2026-02-31T00:00:00Z",
            "2026-01-01T00:00:00.Z",
            "2026-01-01T00:00:00Z\0bad",
        ] {
            let s: i64 = c
                .query_row(&format!("SELECT {}", time::seconds("?1")), [raw], |r| {
                    r.get(0)
                })
                .unwrap();
            assert_eq!(s, i64::MIN, "{raw:?}");
        }
    });
}
#[test]
fn shell_filters_pages_watermark_and_snippets_are_bounded() {
    watchdog(|| {
        let c = db();
        shell_row(&c, "a", "t", "2026-10-31T00:00:00Z", "echo first", 0);
        shell_row(
            &c,
            "b",
            "other",
            "2026-10-31T09:00:00.000000001+09:00",
            "echo second",
            7,
        );
        shell_row(
            &c,
            "c",
            "t",
            "2026-10-31T00:00:00.000000002Z",
            "curl -H 'Authorization: Bearer SECRET-DO-NOT-RETURN'",
            0,
        );
        let mut a = args();
        let first = shells(&c, &a).unwrap();
        assert_eq!(first["items"][0]["execution_id"], "c");
        assert_eq!(first["items"][1]["execution_id"], "b");
        assert!(
            first["items"][0]["command_excerpt_redacted"]
                .as_bool()
                .unwrap()
        );
        assert!(!first.to_string().contains("SECRET-DO-NOT-RETURN"));
        assert!(!first.to_string().contains("raw_command"));
        assert!(!first.to_string().contains("stdout"));
        a.cursor = first["next_cursor"].as_str().map(str::to_owned);
        shell_row(&c, "new", "t", "2026-10-31T00:00:00Z", "echo late", 0);
        let second = shells(&c, &a).unwrap();
        assert_eq!(second["items"].as_array().unwrap().len(), 1);
        assert_eq!(second["items"][0]["execution_id"], "a");
        assert!(second["next_cursor"].is_null());
        a.command_contains = Some("echo".into());
        assert_eq!(shells(&c, &a).unwrap_err().code, ErrorCode::CursorExpired);
        a = args();
        a.thread_id = Some("other".into());
        a.exit_code = Some(7);
        a.command_contains = Some("second".into());
        a.state = Some("exited".into());
        let only = shells(&c, &a).unwrap();
        assert_eq!(only["items"].as_array().unwrap().len(), 1);
        assert_eq!(only["items"][0]["execution_id"], "b");
        a = args();
        a.since = Some("2026-10-31T00:00:00.000000001Z".into());
        a.until = Some("2026-10-31T00:00:00.000000002Z".into());
        let period = shells(&c, &a).unwrap();
        assert_eq!(period["items"][0]["execution_id"], "b");
        assert_eq!(period["items"].as_array().unwrap().len(), 1);
        let long = format!("echo \u{1b}[31m\u{202e}{}", "x".repeat(900));
        shell_row(&c, "long", "t", "2026-10-31T00:01:00Z", &long, 0);
        let p = shells(&c, &args()).unwrap();
        let text = p["items"][0]["command_excerpt"].as_str().unwrap();
        assert_eq!(text.chars().count(), 512);
        assert!(!text.chars().any(char::is_control));
        assert!(!text.contains('\u{202e}'));
    });
}
#[test]
fn bounded_candidate_windows_allow_empty_pages_without_losing_later_matches() {
    watchdog(|| {
        let c = db();
        let tx = c.unchecked_transaction().unwrap();
        for i in 0..2001 {
            shell_row(
                &tx,
                &format!("s{i:05}"),
                "t",
                "2026-10-31T00:00:00Z",
                if i == 0 { "needle" } else { "hay" },
                0,
            );
        }
        tx.commit().unwrap();
        let mut a = args();
        a.command_contains = Some("needle".into());
        let first = shells(&c, &a).unwrap();
        assert_eq!(first["items"], json!([]));
        assert_eq!(first["scanned"], 2000);
        assert_eq!(first["scan_budget_reached"], true);
        a.cursor = first["next_cursor"].as_str().map(str::to_owned);
        assert!(a.cursor.is_some());
        let next = shells(&c, &a).unwrap();
        assert_eq!(next["items"][0]["execution_id"], "s00000");
        assert!(next["next_cursor"].is_null());
    });
}
fn node(c: &Connection, id: &str, kind: &str, at: &str, payload: Value) {
    c.execute(
        "INSERT INTO node(id,kind,created_at,payload_json)VALUES(?1,?2,?3,?4)",
        params![id, kind, at, payload.to_string()],
    )
    .unwrap();
}
fn replacement(c: &Connection, id: &str, kind: &str, at: &str, payload: Value) {
    node(c, id, kind, at, payload);
    c.execute("INSERT INTO replacement(id,output_node_id,input_key,format_version,prompt_version,state,created_at)VALUES(?1,?1,?1,?2,'v1','completed',?3)",params![id,if kind=="summary"{"shellboy-summary-v1"}else{"spill-v1"},at]).unwrap();
    c.execute(
        "INSERT INTO thread_replacement(thread_id,replacement_id)VALUES('t',?1)",
        [id],
    )
    .unwrap();
    c.execute("INSERT INTO replacement_source(replacement_id,ordinal,source_node_id,source_revision)VALUES(?1,0,'original',0)",[id]).unwrap();
}
fn operation(c: &Connection, id: &str, action: &str, at: &str, result: Value) {
    c.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES(?1,'instance','{}','hash','completed',?2,'2026-10-31T00:00:00Z')",params![id,json!({"result":{"thread_id":"t","operation_id":id,"status":"queued","context_edit":{"action":action}}}).to_string()]).unwrap();
    c.execute(
        "INSERT INTO app_meta(key,value)VALUES(?1,?2)",
        params![
            format!("compact.result.{id}"),
            json!({"kind":"compact","state":"completed","ended_at":at,"outcome":{"result":result}})
                .to_string()
        ],
    )
    .unwrap();
}
#[test]
fn compaction_sources_merge_without_merging_restore_into_its_omission() {
    watchdog(|| {
        let c = db();
        node(
            &c,
            "original",
            "message",
            "2026-10-31T00:00:00Z",
            json!({"item":{"role":"user","content":"source"}}),
        );
        replacement(
            &c,
            "sum",
            "summary",
            "2026-10-31T00:01:00Z",
            json!({"item":{"role":"developer","content":[{"type":"input_text","text":"\u{03b1}\0\u{03b2}"}]},"summary_part_keys":["one","two"]}),
        );
        let summary = json!({"before_characters":1000,"after_characters":100,"omitted_nodes":0,"compact":{"source_nodes":1,"created_parts":2,"reused_parts":0,"summary_node_id":"sum","original_node_ids":["original"]}});
        operation(
            &c,
            "summary-op",
            "summary",
            "2026-10-31T00:01:01Z",
            summary.clone(),
        );
        operation(
            &c,
            "coalesced-op",
            "summary",
            "2026-10-31T00:01:01Z",
            summary,
        );
        replacement(
            &c,
            "marker",
            "spill",
            "2026-10-31T00:02:00Z",
            json!({"item":{"role":"developer","content":[]},"context_trim":{"reason":"failure","history_characters":1000,"omitted_nodes":1,"after_current_request_nodes":1}}),
        );
        let trimmed = json!({"marker_node_id":"marker","before_characters":1000,"after_characters":200,"omitted_nodes":1,"compact":{"source_nodes":1,"original_node_ids":["original"]}});
        c.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at,ended_at)VALUES('auto','t',0,0,'p','m',0,'failed','{}',?1,'2026-10-31T00:01:59Z','2026-10-31T00:02:01Z')",[json!({"error":{"kind":"provider"},"automatic_retry":{"number":2},"automatic_trim":trimmed}).to_string()]).unwrap();
        operation(
            &c,
            "restore-op",
            "restore",
            "2026-10-31T00:03:00Z",
            json!({"marker_node_id":"marker","before_characters":200,"after_characters":1000,"omitted_nodes":0,"compact":{"source_nodes":1,"original_node_ids":["original"]}}),
        );
        let page = compactions(&c, "t", &comp_args()).unwrap();
        let rows = page["items"].as_array().unwrap();
        assert_eq!(rows.len(), 3, "{page}");
        assert_eq!(rows[0]["kind"], "restore");
        assert_eq!(rows[1]["kind"], "automatic_reduction");
        assert_eq!(rows[1]["metrics"]["after_current_request_nodes"], 1);
        assert_eq!(
            rows[1]["trigger"]["retained_failures_in_turn_through_attempt"],
            1
        );
        assert_eq!(rows[2]["metrics"]["summary_characters"], 3);
        assert_eq!(rows[2]["metrics"]["summary_parts"], 2);
        assert_eq!(rows[2]["operation_count"], 2);
        let mut a = comp_args();
        a.since = Some("2026-10-31T00:02:00Z".into());
        a.until = Some("2026-10-31T00:03:00Z".into());
        let p = compactions(&c, "t", &a).unwrap();
        assert_eq!(p["items"].as_array().unwrap().len(), 1);
        assert_eq!(p["items"][0]["kind"], "automatic_reduction");
    });
}
#[test]
fn command_redaction_covers_credentials_controls_and_unicode_bounds() {
    watchdog(|| {
        for raw in [
            "OPENAI_API_KEY=sk-example-secret curl test",
            "curl -u user:pass https://example.test",
            "curl https://user:pass@example.test/a",
            "curl 'https://example.test?token=secret'",
            "echo ghp_01234567890123456789",
            "ConvertTo-SecureString secret -AsPlainText",
            "curl -H 'X-API-Key: abcdef'",
            "echo --password='secret'",
        ] {
            let (snippet, redacted) = shell::snippet(raw);
            assert!(redacted, "{raw}");
            assert_eq!(snippet, "[redacted: credential-like command]");
        }
    });
}

fn plans(c: &Connection, sql: &str, bindings: &[&dyn rusqlite::ToSql]) -> Vec<String> {
    c.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(bindings, |r| r.get::<_, String>(3))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}
fn no_scans(plan: &[String], persistent: &[&str]) {
    for d in plan {
        let words = d.split_whitespace().collect::<Vec<_>>();
        assert!(
            !(words.first() == Some(&"SCAN")
                && words.get(1).is_some_and(|a| persistent.contains(a))),
            "persistent scan: {plan:?}"
        );
    }
}
#[test]
fn readback_plans_seek_shared_tables_and_indexes_have_measured_sizes() {
    watchdog(|| {
        let c = db();
        let tx = c.unchecked_transaction().unwrap();
        for i in 0..5000 {
            shell_row(
                &tx,
                &format!("s{i:05}"),
                if i % 2 == 0 { "t" } else { "other" },
                "2026-10-31T00:00:00Z",
                "echo fixture",
                0,
            );
        }
        tx.commit().unwrap();
        let tx = c.unchecked_transaction().unwrap();
        for i in 0..5000 {
            let t = if i % 2 == 0 { "t" } else { "other" };
            tx.execute("INSERT INTO operation_receipt(operation_id,instance_id,origin_json,request_hash,state,result_ref_json,created_at)VALUES(?1,'test','{}','hash','completed',?2,'2026-10-31T00:00:00Z')",params![format!("op{i}"),json!({"result":{"thread_id":t,"status":"queued"}}).to_string()]).unwrap();
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,error_json,started_at)VALUES(?1,?2,0,0,'p','m',0,'failed','{}','{\"automatic_trim\":{}}','2026-10-31T00:00:00Z')",params![format!("a{i}"),t]).unwrap();
        }
        tx.commit().unwrap();
        for thread in [false, true] {
            let scope = if thread { Some("t") } else { None };
            let bindings = rusqlite::params![
                scope,
                1793404799_i64,
                0,
                1793404860_i64,
                0,
                i64::MAX,
                u32::MAX,
                "",
                i64::MAX,
                None::<String>,
                None::<i32>,
                None::<String>,
                2,
                1
            ];
            let p = plans(&c, &shell::query(thread), bindings);
            eprintln!("shell scope={thread}: {p:?}");
            no_scans(&p, &["s", "r"]);
            assert!(p.iter().any(|s| s.contains(if thread {
                "shell_owner_started_readback"
            } else {
                "shell_started_readback"
            }) && s.contains("SEARCH")));
        }
        let w = Window::new(Some("2026-10-31T00:00:00Z"), Some("2026-11-01T00:00:00Z")).unwrap();
        for sql in [
            compact::receipt_query(&w),
            compact::automatic_query(&w),
            compact::replacement_query(&w),
        ] {
            let p = plans(&c, &sql, params!["t", 50]);
            eprintln!("compact: {p:?}");
            no_scans(&p, &["o", "m", "u", "a", "earlier", "tr", "r", "n", "q"]);
        }
        for name in [
            "shell_started_readback",
            "shell_owner_started_readback",
            "compact_operation_thread",
            "automatic_compaction_turn",
        ] {
            let bytes: i64 = c
                .query_row(
                    "SELECT sum(pgsize) FROM dbstat WHERE name=?1",
                    [name],
                    |r| r.get(0),
                )
                .unwrap();
            assert!(bytes > 0);
            eprintln!("readback index {name}: {bytes} bytes at 5000 rows per indexed source");
        }
    });
}

pub(crate) fn watchdog(f: impl FnOnce() + Send + 'static) {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    let worker = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
        let _ = tx.send(result);
    });
    let result = rx
        .recv_timeout(std::time::Duration::from_secs(60))
        .expect("saved-readback fixture watchdog");
    worker.join().unwrap();
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn readonly_commands_complete_while_writer_is_held_without_provider_or_receipt_writes() {
    tokio::time::timeout(std::time::Duration::from_secs(60),async {
  use crate::{storage::DbWorker,control::db::Store,config::Config,protocol::*};
  use std::sync::Arc;
  use tokio::{net::TcpListener,io::AsyncWriteExt};
  let root=tempfile::tempdir().unwrap();let log=root.path().join("requests.jsonl");std::fs::write(&log,"").unwrap();
  let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();let file=log.clone();
  let server=tokio::spawn(async move {loop {let(mut socket,_)=listener.accept().await.unwrap();use std::io::Write;writeln!(std::fs::OpenOptions::new().append(true).open(&file).unwrap(),"unexpected provider request").unwrap();let _=socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}").await;}});
  let database=DbWorker::open(root.path().join("db")).await.unwrap();
  let config=Config::parse(&format!("default_connection=\"p\"\ndefault_model=\"m\"\n[connections.p]\nbase_url=\"http://{address}/v1\"\n")).unwrap();
  let store=Store::new(database.clone(),Arc::new(config),root.path().into(),"test".into());
  database.call(|c| {
   c.execute_batch("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('t','/','','p','m','2026-10-31T00:00:00Z');INSERT INTO run(id,thread_id,state,created_at)VALUES('r','t','completed','2026-10-31T00:00:00Z');INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('u','t','r','completed','2026-10-31T00:00:00Z');")?;Ok(())
  }).await.unwrap();
  database.call(|c| {operation(c,"prior","restore","2026-10-31T00:00:00Z",json!({"before_characters":10,"after_characters":20,"compact":{"source_nodes":0}}));Ok(())}).await.unwrap();
  let (started,ready)=tokio::sync::oneshot::channel();let (release,gate)=std::sync::mpsc::sync_channel(1);let writer=database.clone();
  let held=tokio::spawn(async move {writer.call(move|_|{let _=started.send(());let _=gate.recv();Ok(())}).await.unwrap()});ready.await.unwrap();
  let commands=[Command::ShellList(args()),Command::CompactList(comp_args()),Command::UsageGet(UsageGetArgs{thread_id:Some("t".into()),since:Some("2026-10-31T00:00:00Z".into()),..Default::default()}),Command::ThreadGet(ThreadTarget{thread_id:Some("t".into())}),Command::RunGet(RunTarget{run_id:"r".into()}),Command::TurnGet(TurnTarget{turn_id:"u".into()})];
  for command in commands {assert!(command.is_read_only());let is_thread=command.name()=="thread.get";let v=store.execute(Request::new(uuid::Uuid::new_v4().to_string(),command),None).await.unwrap();if is_thread {assert_eq!(v.result["last_compaction"]["kind"],"restore");}}
  // The writer is still gated: successful replies cannot have occupied its lane.
  release.send(()).unwrap();held.await.unwrap();
  database.call(|c|{assert_eq!(c.query_row("SELECT count(*) FROM operation_receipt",[],|r|r.get::<_,i64>(0))?,1);assert_eq!(c.query_row("SELECT count(*) FROM model_attempt",[],|r|r.get::<_,i64>(0))?,0);Ok(())}).await.unwrap();
  assert_eq!(std::fs::read_to_string(log).unwrap().lines().count(),0);database.shutdown().await.unwrap();server.abort();let _=server.await;
 }).await.expect("readonly lane fixture watchdog");
}
