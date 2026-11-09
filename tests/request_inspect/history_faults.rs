use super::*;

// Only synthetic SQLite rows; never opens an owner or calls a provider.
#[tokio::test]
async fn diagnosis_284_active_incomplete_and_settled_corrupt_groups() {
    for (case, run_state, turn_state, group_state, result_ids) in [
        ("active_missing", "live", "running", "running", vec![]),
        ("active_partial", "live", "running", "running", vec!["a"]),
        (
            "duplicate_result",
            "live",
            "running",
            "running",
            vec!["a", "a"],
        ),
        ("duplicate_call", "live", "running", "running", vec![]),
        ("malformed_call", "live", "running", "running", vec![]),
        ("spill_invalid", "live", "running", "running", vec![]),
        ("wrong_attempt", "live", "running", "running", vec![]),
        (
            "missing_metadata_call",
            "live",
            "running",
            "running",
            vec![],
        ),
        (
            "active_corrupt",
            "live",
            "running",
            "running",
            vec!["wrong"],
        ),
        (
            "recovery_older",
            "live",
            "recovery_wait",
            "running",
            vec!["a"],
        ),
        ("future_generation", "live", "running", "running", vec![]),
        ("ambiguous_generation", "live", "running", "running", vec![]),
        ("unresolved", "live", "running", "running", vec![]),
        (
            "old_group_new_run",
            "stopped",
            "stopped",
            "running",
            vec!["a"],
        ),
        ("missing_then_corrupt", "live", "running", "running", vec![]),
        ("sqlite_failure", "live", "running", "running", vec![]),
        (
            "stopped_incomplete",
            "stopped",
            "stopped",
            "running",
            vec!["a"],
        ),
        (
            "settled_missing",
            "completed",
            "completed",
            "completed",
            vec!["a"],
        ),
        (
            "settled_wrong_id",
            "completed",
            "completed",
            "completed",
            vec!["a", "wrong"],
        ),
        (
            "settled_complete",
            "completed",
            "completed",
            "completed",
            vec!["a", "b"],
        ),
    ] {
        let root = tempfile::tempdir().unwrap();
        let config_path = root.path().join("config.toml");
        let data = root.path().join("data");
        std::fs::create_dir(&data).unwrap();
        std::fs::write(&config_path, "default_connection='fixture'\ndefault_model='synthetic'\n[connections.fixture]\nbase_url='http://127.0.0.1:1/v1'\n").unwrap();
        let loaded = Config::load_with_overrides(
            Some(&config_path),
            Some(&data),
            root.path(),
            &PathEnvironment {
                platform: shellboy::config::Platform::Linux,
                home: None,
                xdg_config_home: None,
                xdg_data_home: None,
                appdata: None,
                localappdata: None,
                data_dir: None,
            },
        )
        .unwrap();
        let path = data.join("shellboy.db");
        let mut db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(include_str!("../../src/storage/current.sql"))
            .unwrap();
        let tx = db.transaction().unwrap();
        tx.execute("INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at)VALUES('fixture','/','','fixture','synthetic','now')", []).unwrap();
        tx.execute(
            "INSERT INTO run(id,thread_id,state,created_at)VALUES('run','fixture',?1,'now')",
            [run_state],
        )
        .unwrap();
        tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('turn','fixture','run',?1,'now')", [turn_state]).unwrap();
        let manifest = json!({"replay":{"nodes":0,"sha256":sha256(b"")}}).to_string();
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('attempt','turn',0,0,'fixture','synthetic',0,'completed',?1,'now')", [&manifest]).unwrap();
        tx.execute(
            "INSERT INTO tool_group VALUES('group','attempt','turn',?1,'now')",
            [group_state],
        )
        .unwrap();
        let calls = json!({"group_id":"group","output":[{"type":"function_call","call_id":"a","name":"shell","arguments":"{}"},{"type":"function_call","call_id":"b","name":"shell","arguments":"{}"}]});
        shellboy::history::append_node(
            &tx,
            "fixture",
            "calls",
            Some("turn"),
            "assistant_response",
            &calls,
            "now",
        )
        .unwrap();
        for (n, item) in calls["output"].as_array().unwrap().iter().enumerate() {
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('attempt',?1,'function_call','completed',?2,'calls')",rusqlite::params![n as i64,item.to_string()]).unwrap();
        }
        for (n, id) in ["a", "b"].iter().enumerate() {
            tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state)VALUES('group',?1,?2,'shell','{}','pending')",rusqlite::params![id,n as i64]).unwrap();
        }
        for (n, id) in result_ids.iter().enumerate() {
            shellboy::history::append_node(&tx,"fixture",&format!("result-{n}"),Some("turn"),"tool_result",&json!({"group_id":"group","item":{"type":"function_call_output","call_id":id,"output":"synthetic saved result"}}),"now").unwrap();
        }
        for (n, _) in result_ids.iter().enumerate() {
            let call = ["a", "b"][n];
            tx.execute(
                "INSERT INTO tool_result VALUES('group',?1,?2,'completed')",
                rusqlite::params![call, format!("result-{n}")],
            )
            .unwrap();
            tx.execute(
                "UPDATE tool_call SET state='completed' WHERE group_id='group' AND call_id=?1",
                [call],
            )
            .unwrap();
        }
        match case {
            "duplicate_call" => {
                tx.execute("UPDATE node SET payload_json=json_set(payload_json,'$.output[1].call_id','a') WHERE id='calls'", []).unwrap();
            }
            "malformed_call" => {
                tx.execute("UPDATE node SET payload_json=json_set(payload_json,'$.output[1].call_id','') WHERE id='calls'", []).unwrap();
            }
            "spill_invalid" => {
                tx.execute("UPDATE node SET payload_json=json_set(payload_json,'$.spill_version',1) WHERE id='calls'", []).unwrap();
            }
            "wrong_attempt" => {
                tx.execute(
                    "UPDATE output_item SET node_id=NULL WHERE attempt_id='attempt'",
                    [],
                )
                .unwrap();
            }
            "missing_metadata_call" => {
                tx.execute("DELETE FROM tool_call WHERE call_id='b'", [])
                    .unwrap();
            }
            "recovery_older" | "ambiguous_generation" => {
                tx.execute("UPDATE turn SET generation=1 WHERE id='turn'", [])
                    .unwrap();
            }
            "future_generation" => {
                tx.execute(
                    "UPDATE model_attempt SET generation=1 WHERE id='attempt'",
                    [],
                )
                .unwrap();
            }
            "unresolved" => {
                tx.execute("DELETE FROM tool_call", []).unwrap();
                tx.execute("DELETE FROM tool_group", []).unwrap();
            }
            "old_group_new_run" => {
                tx.execute("INSERT INTO run(id,thread_id,state,created_at)VALUES('new-run','fixture','live','later')", []).unwrap();
                tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at)VALUES('new-turn','fixture','new-run','running','later')", []).unwrap();
            }
            "missing_then_corrupt" => {
                shellboy::history::append_node(&tx,"fixture","orphan",Some("turn"),"tool_result",&json!({"group_id":"unrelated","item":{"type":"function_call_output","call_id":"other","output":"synthetic"}}),"now").unwrap();
            }
            "sqlite_failure" => {
                tx.execute("DELETE FROM tool_call", []).unwrap();
                tx.execute("DROP TABLE tool_group", []).unwrap();
            }
            _ => {}
        }
        // A second saved attempt selects the whole current synthetic path.
        let framed = {
            let mut stmt = tx.prepare("SELECT n.id,n.payload_revision,n.deleted_reason IS NOT NULL OR n.payload_json IS NULL FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='fixture' ORDER BY p.ordinal").unwrap();
            stmt.query_map([], |r| {
                Ok(format!(
                    "{}\u{1f}{}\u{1f}{}\u{1e}",
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    u8::from(r.get::<_, bool>(2)?)
                ))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .concat()
        };
        let count: i64 = tx
            .query_row(
                "SELECT count(*) FROM thread_path WHERE thread_id='fixture'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let full_manifest =
            json!({"replay":{"nodes":count,"sha256":sha256(framed.as_bytes())}}).to_string();
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('full-attempt','turn',0,0,'fixture','synthetic',0,'completed',?1,'later')",[full_manifest]).unwrap();
        let stale_manifest =
            json!({"replay":{"nodes":count,"sha256":"not-the-selected-prefix"}}).to_string();
        tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('stale-attempt','turn',0,0,'fixture','synthetic',0,'completed',?1,'later')",[stale_manifest]).unwrap();
        tx.commit().unwrap();
        drop(db);
        let before = std::fs::read(&path).unwrap();
        let mut options = RequestOptions {
            thread_id: "fixture".into(),
            attempt_id: None,
            compare_attempt_id: None,
            body: true,
        };
        let current = inspect(&loaded, &options);
        let cli = shellboy::request_inspect::execute(
            options.clone(),
            Some(config_path.clone()),
            Some(data.clone()),
            root.path(),
        )
        .await;
        if case != "settled_complete" {
            assert_eq!(cli["ok"], false);
            assert!(cli.get("result").is_none());
            assert_eq!(cli["error"]["code"], current.as_ref().unwrap_err().code);
            if case != "sqlite_failure" {
                assert_eq!(
                    cli["error"]["details"],
                    *current.as_ref().unwrap_err().details.as_ref().unwrap()
                );
                assert!(cli["error"]["hint"].as_str().is_some_and(|s| !s.is_empty()));
            }
        }
        if case == "settled_complete" {
            assert!(current.is_ok(), "{case}: {:?}", current.err());
            eprintln!("{case}: current=ok");
        } else {
            let error = current.unwrap_err();
            if case == "sqlite_failure" {
                assert_eq!(error.code, "storage_failed");
                assert!(error.details.is_none());
                assert_eq!(std::fs::read(&path).unwrap(), before);
                continue;
            }
            assert_eq!(error.code, "invalid_state");
            assert!(error.hint.as_ref().is_some_and(|s| !s.is_empty()));
            let reason = match case {
                "active_missing" | "active_partial" => "request_history_busy",
                "stopped_incomplete" | "recovery_older" | "old_group_new_run" => {
                    "request_history_incomplete"
                }
                _ => "request_history_invalid",
            };
            assert_eq!(error.details.as_ref().unwrap()["reason"], reason);

            eprintln!("{case}: current={} / {}", error.code, error.message);
        }
        options.attempt_id = Some("attempt".into());
        assert!(
            inspect(&loaded, &options).is_ok(),
            "{case}: saved pre-group attempt must remain inspectable"
        );
        options.compare_attempt_id = Some("stale-attempt".into());
        let stale = inspect(&loaded, &options).unwrap();
        assert!(stale["compare"].is_object());
        options.compare_attempt_id = Some("attempt".into());
        assert!(
            inspect(&loaded, &options).is_ok(),
            "{case}: two empty selected prefixes"
        );
        options.compare_attempt_id = Some("full-attempt".into());
        let compared = inspect(&loaded, &options);
        if case == "settled_complete" {
            assert!(compared.is_ok());
        } else {
            assert_eq!(
                compared.unwrap_err().code,
                "invalid_state",
                "{case}: compare must validate its own prefix"
            );
        }
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "{case}: inspection must not write history"
        );
    }
}
