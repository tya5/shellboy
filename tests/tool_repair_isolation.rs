//! Startup must isolate an invalid selected cursor before writing its snapshot.
use serde_json::{Value, json};
use shellboy::{app::DataOwner, control::Control, history, ipc::LocalServer};
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
    time::timeout,
};

async fn cli(directory: &std::path::Path, args: &[&str]) -> Value {
    let output = timeout(
        Duration::from_secs(15),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_shellboy"))
            .kill_on_drop(true)
            .arg("--data-dir")
            .arg(directory)
            .args(args)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "CLI JSON: {e}, stderr={}",
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert!(output.status.success(), "{value}");
    assert_eq!(value["ok"], true, "{value}");
    value["result"].clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn bad_cursor_does_not_block_owner_good_conversation_or_cli_deletion() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("AGENTS.md"), "").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let config = root.path().join("config.toml");
    let directory = root.path().join("data");
    std::fs::write(&config,format!("default_connection='p'\ndefault_model='m'\n[connections.p]\nbase_url={url:?}\nresponse_max_retries=0\n")).unwrap();
    let owner = DataOwner::start(
        Some(config.clone()),
        Some(directory.clone()),
        root.path().into(),
    )
    .await
    .unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    owner.db.call(move|db| {
        let tx=db.transaction()?;
        for id in ["bad","good","incomplete"] {
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?1,?2,?3,'p','m','old')",rusqlite::params![id,cwd,format!("{cwd}/AGENTS.md")])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?1,'live','old')",[id])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?1,?1,?1,'running','old')",[id])?;
            tx.execute("INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at) VALUES(?1,?1,0,0,'p','m',0,'completed','{}','old')",[id])?;
            tx.execute("INSERT INTO tool_group VALUES(?1,?1,?1,'running','old')",[id])?;
            let call=json!({"type":"function_call","call_id":format!("call-{id}"),"name":"shell","arguments":"{}"});
            if id=="incomplete" {
                history::append_node(&tx,id,"before-incomplete",None,"user",&json!({"role":"user","content":[{"type":"input_text","text":"complete earlier request"}]}),"old")?;
            }
            let source=format!("source-{id}");
            history::append_node(&tx,id,&source,Some(id),"assistant_response",&json!({"output":[call],"group_id":id}),"old")?;
            if id!="incomplete" {
            tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id) VALUES(?1,0,'function_call','completed',?2,?3)",rusqlite::params![id,call.to_string(),source])?;
            }
            tx.execute("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state) VALUES(?1,?2,0,'shell','{}','pending')",rusqlite::params![id,format!("call-{id}")])?;
        }
        history::append_node(&tx,"bad","bad-tail",None,"interruption_context",&json!({"text":"old audit"}),"old")?;
        tx.execute("UPDATE thread SET cursor_node_id='source-bad' WHERE id='bad'",[])?;
        tx.commit()?;Ok(())
    }).await.unwrap();
    // Closing only the store preserves the deliberately interrupted durable fixture.
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(directory.clone()), root.path().into())
        .await
        .expect("one corrupt Thread must not block startup");
    owner.db.call(|db| {
        assert_eq!(db.query_row("SELECT cursor_node_id FROM thread WHERE id='bad'",[],|r|r.get::<_,String>(0))?,"source-bad");
        assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id='bad'",[],|r|r.get::<_,i64>(0))?,2);
        assert_eq!(db.query_row("SELECT count(*) FROM tool_result WHERE group_id='bad'",[],|r|r.get::<_,i64>(0))?,0);
        assert_eq!(db.query_row("SELECT state FROM tool_group WHERE id='bad'",[],|r|r.get::<_,String>(0))?,"running");
        assert_eq!(db.query_row("SELECT count(*) FROM tool_result WHERE group_id='good'",[],|r|r.get::<_,i64>(0))?,1);
        assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='good' AND n.kind='interruption_context')",[],|r|r.get::<_,bool>(0))?);
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(16);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let list = cli(&directory, &["thread", "list"]).await;
    assert!(
        list["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item["thread_id"] == "bad")
    );
    let broken = cli(
        &directory,
        &[
            "message",
            "send",
            "--thread",
            "incomplete",
            "--text",
            "show local failure",
        ],
    )
    .await;
    let broken_turn = broken["turn_id"].as_str().unwrap().to_owned();
    let reason = timeout(Duration::from_secs(15),async {
        loop {
            let turn = broken_turn.clone();
            let reason = owner.db.call(move|db| {
                use rusqlite::OptionalExtension;
                Ok(db.query_row("SELECT recovery_reason_json FROM turn WHERE id=?1 AND state='recovery_wait'",[turn],|r|r.get::<_,String>(0)).optional()?)
            }).await.unwrap();
            if let Some(reason)=reason {break reason;}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }).await.unwrap();
    assert!(
        reason.contains("fork at a complete node before the incomplete tool group"),
        "{reason}"
    );
    assert!(reason.contains("delete the affected Thread"), "{reason}");
    assert!(!reason.contains("Restart Shellboy"));
    assert!(
        timeout(Duration::from_millis(100), listener.accept())
            .await
            .is_err(),
        "unresolved group must fail locally before any HTTP connection"
    );
    let forked = cli(
        &directory,
        &[
            "thread",
            "fork",
            "--thread",
            "incomplete",
            "--node",
            "before-incomplete",
        ],
    )
    .await;
    assert_ne!(forked["thread_id"], "incomplete");
    let model = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let mut chunk = [0; 8192];
        let body = loop {
            let n = socket.read(&mut chunk).await.unwrap();
            assert!(n > 0);
            bytes.extend_from_slice(&chunk[..n]);
            assert!(bytes.len() < 1024 * 1024);
            if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                let len = String::from_utf8_lossy(&bytes[..at])
                    .lines()
                    .find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .map(|v| v.trim().parse::<usize>().unwrap())
                    })
                    .unwrap();
                if bytes.len() >= at + 4 + len {
                    break serde_json::from_slice::<Value>(&bytes[at + 4..at + 4 + len]).unwrap();
                }
            }
        };
        let response=json!({"id":"good-answer","status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":"good conversation continued"}]}]}).to_string();
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}",response.len()).as_bytes()).await.unwrap();
        body
    });
    let accepted = cli(
        &directory,
        &[
            "message",
            "send",
            "--thread",
            "good",
            "--text",
            "continue good conversation",
        ],
    )
    .await;
    let body = timeout(Duration::from_secs(15), model)
        .await
        .unwrap()
        .unwrap();
    let input = body["input"].as_array().unwrap();
    let call = input
        .iter()
        .position(|item| item["type"] == "function_call")
        .unwrap();
    assert_eq!(input[call + 1]["type"], "function_call_output");
    assert_eq!(input[call + 1]["call_id"], "call-good");
    let run = accepted["run_id"].as_str().unwrap().to_owned();
    timeout(Duration::from_secs(15), async {
        loop {
            let target = run.clone();
            if control
                .store
                .database
                .call(move |db| {
                    Ok(db.query_row(
                        "SELECT state='completed' FROM run WHERE id=?1",
                        [target],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .await
                .unwrap()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    cli(&directory, &["thread", "delete", "--thread", "bad"]).await;
    owner
        .db
        .call(|db| {
            assert!(db.query_row(
                "SELECT deleted_at IS NOT NULL FROM thread WHERE id='bad'",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            Ok(())
        })
        .await
        .unwrap();
    control.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
    let logs = std::fs::read_dir(directory.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    assert!(
        logs.lines()
            .filter_map(|line| serde_json::from_str::<Value>(line).ok())
            .any(|entry| entry["tool_repair"]["thread_id"] == "bad"
                && entry["tool_repair"]["reason"] == "capture_cursor_mismatch")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn malformed_revisit_capture_json_is_isolated_without_hiding_sql_failures() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let directory = root.path().join("data");
    std::fs::write(&config,"default_connection='p'\ndefault_model='m'\n[connections.p]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
    let owner = DataOwner::start(
        Some(config.clone()),
        Some(directory.clone()),
        root.path().into(),
    )
    .await
    .unwrap();
    let cwd = root.path().to_string_lossy().into_owned();
    owner.db.call(move|db| {
        // app_meta admits JSON syntax/schema defects without violating SQLite integrity.
        let tx=db.transaction()?;
        for id in ["healthy","bad-revisit-syntax","bad-revisit-shape"] {
            tx.execute("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at) VALUES(?1,?1,?2,?2,'p','m','2026-10-30T00:00:00Z')",rusqlite::params![id,cwd])?;
            tx.execute("INSERT INTO run(id,thread_id,state,created_at) VALUES(?1,?1,'live','2026-10-30T00:00:00Z')",[id])?;
            tx.execute("INSERT INTO turn(id,thread_id,run_id,state,created_at) VALUES(?1,?1,?1,'recovery_wait','2026-10-30T00:00:00Z')",[id])?;
        }
        tx.execute("INSERT INTO app_meta(key,value) VALUES('revisit.bad-revisit-syntax','{')",[])?;
        tx.execute("INSERT INTO app_meta(key,value) VALUES('revisit.bad-revisit-shape',?1)",[json!({"pending":"wrong shape","consumed_count":"invalid","last_fired_at":null}).to_string()])?;
        tx.commit()?;
        Ok(())
    }).await.unwrap();
    owner.shutdown().await.unwrap();
    let owner = DataOwner::start(Some(config), Some(directory.clone()), root.path().into())
        .await
        .expect("Thread-local malformed JSON must not prevent owner startup");
    owner.db.call(|db| {
        assert!(db.query_row("SELECT EXISTS(SELECT 1 FROM thread_path p JOIN node n ON n.id=p.node_id WHERE p.thread_id='healthy' AND n.kind='interruption_context')",[],|r|r.get::<_,bool>(0))?);
        for id in ["bad-revisit-syntax","bad-revisit-shape"] {
            assert_eq!(db.query_row("SELECT count(*) FROM thread_path WHERE thread_id=?1",[id],|r|r.get::<_,i64>(0))?,0,"{id}: malformed capture must not write a partial snapshot");
            assert_eq!(db.query_row("SELECT state FROM run WHERE id=?1",[id],|r|r.get::<_,String>(0))?,"stopped");
        }
        assert_eq!(db.query_row("SELECT count(*) FROM app_meta WHERE key IN ('revisit.bad-revisit-syntax','revisit.bad-revisit-shape')",[],|r|r.get::<_,i64>(0))?,0);
        Ok(())
    }).await.unwrap();
    let control = Control::new(&owner, root.path().into());
    let (send, receive) = tokio::sync::mpsc::channel(8);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let healthy = cli(&directory, &["thread", "get", "--thread", "healthy"]).await;
    assert_eq!(healthy["thread_id"], "healthy");
    let threads = cli(&directory, &["thread", "list"]).await;
    assert_eq!(threads["items"].as_array().unwrap().len(), 3);
    control.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
    let logs = std::fs::read_dir(directory.join("logs"))
        .unwrap()
        .map(|entry| std::fs::read_to_string(entry.unwrap().path()).unwrap())
        .collect::<String>();
    let records = logs
        .lines()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|entry| entry.get("tool_repair").cloned())
        .collect::<Vec<_>>();
    for (id, reason) in [
        ("bad-revisit-syntax", "capture_revisit_malformed"),
        ("bad-revisit-shape", "capture_revisit_malformed"),
    ] {
        assert!(
            records.iter().any(|entry| entry["thread_id"] == id
                && entry["reason"] == reason
                && entry.get("group_id").is_none()),
            "{id}: {records:?}"
        );
    }
}

#[tokio::test]
async fn check_violating_recovery_json_remains_a_fatal_startup_integrity_error() {
    let root = tempfile::tempdir().unwrap();
    let config = root.path().join("config.toml");
    let directory = root.path().join("data");
    std::fs::write(&config, "").unwrap();
    let owner = DataOwner::start(
        Some(config.clone()),
        Some(directory.clone()),
        root.path().into(),
    )
    .await
    .unwrap();
    owner.db.call(|db| {
        // Deliberately bypass the CHECK solely to simulate an integrity-violating DB.
        db.execute_batch("PRAGMA ignore_check_constraints=ON;
            INSERT INTO thread(id,cwd,agents_path,profile_id,model_id,updated_at) VALUES('bad','/','/','p','m','old');
            INSERT INTO run(id,thread_id,state,created_at) VALUES('bad','bad','live','old');
            INSERT INTO turn(id,thread_id,run_id,state,created_at,recovery_reason_json) VALUES('bad','bad','bad','recovery_wait','old','{');
            PRAGMA ignore_check_constraints=OFF;")?;
        Ok(())
    }).await.unwrap();
    owner.shutdown().await.unwrap();
    let error =
        match DataOwner::start(Some(config), Some(directory.clone()), root.path().into()).await {
            Ok(owner) => {
                owner.shutdown().await.unwrap();
                panic!("CHECK-violating records must not be accepted as an isolated Thread defect");
            }
            Err(error) => error,
        };
    assert_eq!(error.code, shellboy::protocol::ErrorCode::SaveFailed);
    assert!(error.message.contains("quick_check"), "{error:?}");
    assert!(
        error.message.contains("CHECK constraint failed in turn"),
        "{error:?}"
    );
    let db = rusqlite::Connection::open(directory.join("shellboy.db")).unwrap();
    assert_eq!(
        db.query_row(
            "SELECT recovery_reason_json FROM turn WHERE id='bad'",
            [],
            |r| r.get::<_, String>(0)
        )
        .unwrap(),
        "{"
    );
    assert_eq!(
        db.query_row("SELECT state FROM run WHERE id='bad'", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "live"
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM node", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
}
