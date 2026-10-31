//! Opt-in discovery check using only the local LiteLLM proxy and a fresh cwd/store.
#![cfg(unix)]
use serde_json::Value;
use shellboy::{app::DataOwner, control::Control, ipc::LocalServer, protocol::*};
use std::{sync::Arc, time::Duration};

async fn request(control: &Arc<Control>, command: Command) -> Value {
    let response = control
        .request(Request::new(
            format!("{}:{}", control.store.instance_id, uuid::Uuid::new_v4()),
            command,
        ))
        .await;
    assert!(response.ok, "{:?}", response.error);
    response.result.unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "opt-in actual luna API requests through 127.0.0.1:4000"]
async fn luna_discovers_bundled_guides_and_observes_its_background_shell() {
    let root = tempfile::tempdir().unwrap();
    let cwd = root.path().join("empty-project");
    std::fs::create_dir(&cwd).unwrap();
    let binary = std::path::Path::new(env!("CARGO_BIN_EXE_shellboy"));
    let search_path = format!(
        "{}:/usr/bin:/bin:/usr/sbin:/sbin",
        binary.parent().unwrap().display()
    );
    // The driver prepends current_exe's directory. In an integration test that
    // is target/debug/deps, not the actual CLI directory, and would otherwise
    // resolve the user's older installed shellboy. Pin this fixture's shell PATH.
    let shell = root.path().join("fixture-shell");
    std::fs::write(
        &shell,
        format!(
            "#!/bin/sh\nexport PATH='{}'\nexec /bin/bash \"$@\"\n",
            search_path.replace('\'', "'\\''")
        ),
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700)).unwrap();
    let config = root.path().join("config.toml");
    std::fs::write(&config, format!(
        "default_connection='local'\ndefault_model='gpt-5.6-luna'\n[connections.local]\nbase_url='http://127.0.0.1:4000/v1'\n[shell]\nexecutable={}\n",
        serde_json::to_string(&shell).unwrap(),
    )).unwrap();
    let owner = DataOwner::start(Some(config), Some(root.path().join("data")), cwd.clone())
        .await
        .unwrap();
    let control = Control::new(&owner, cwd);
    control.start().await.unwrap();
    let (send, receive) = tokio::sync::mpsc::channel(32);
    let server = LocalServer::bind(&owner, send).await.unwrap();
    let serving = tokio::spawn(control.clone().serve(receive));
    let checked: Result<(), String> = async {
        let thread = request(&control, Command::ThreadCreate(ThreadCreateArgs { agents: None,
            connection: None, cwd: None, model: None, effort: None, parent_id: None,
        })).await;
        let thread_id = thread["thread_id"].as_str().unwrap().to_owned();
        request(&control, Command::MessageSend(MessageSendArgs { sender_label: None,
now: false,
            thread_id: Some(thread_id.clone()),
            text: "操作に不慣れなので、まず利用可能な操作ガイドで背景shellの手順を確認してから実行して。shell toolで command='sleep 2; printf SHELLBOY_DOCS_DISCOVERY_V1' をbackground:trueで直接起動し、その実行の出力だけを後続のshell get/readで確認して結果を報告してください。リポジトリはありません。続いて子エージェントを一つ作り、SHELLBOY_DOCS_CHILD_V1 とだけ回答するよう依頼し、回答原文を取得して報告してください。親自身への会話送信は不要です。".into(),
        })).await;
        crate::test_watchdog::watchdog("tests/docs_proxy.rs: luna_discovers_bundled_guides_and_observes_its_background_shell", async {
            loop {
                let target = thread_id.clone();
                let state = control.store.database.call(move |db| {
                    Ok(db.query_row("SELECT state FROM turn WHERE thread_id=?1 ORDER BY rowid DESC LIMIT 1", [target], |r| r.get::<_, String>(0))?)
                }).await.map_err(|e| e.to_string())?;
                match state.as_str() {
                    "completed" => {
                        let target = thread_id.clone();
                        let live = control.store.database.call(move |db| {
                            Ok(db.query_row("SELECT count(*) FROM run WHERE thread_id=?1 AND state IN ('live','stopping')", [target], |r| r.get::<_, i64>(0))?)
                        }).await.map_err(|e| e.to_string())?;
                        if live == 0 { break; }
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    },
                    "recovery_wait" | "stopped" => return Err(format!("model turn ended in {state}")),
                    _ => tokio::time::sleep(Duration::from_millis(100)).await,
                }
            }
            Ok::<_, String>(())
        }).await.map_err(|_| "discovery timed out".to_owned())??;
        let target = thread_id;
        let commands = control.store.database.call(move |db| {
            Ok(db.prepare("SELECT c.arguments_json FROM tool_call c JOIN tool_group g ON g.id=c.group_id JOIN turn t ON t.id=g.turn_id WHERE t.thread_id=?1 ORDER BY c.rowid")?
                .query_map([target], |r| r.get::<_, String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?)
        }).await.map_err(|e| e.to_string())?;
        let calls: Vec<Value> = commands.iter().map(|s| serde_json::from_str(s).unwrap()).collect();
        if !calls.iter().any(|v| v["command"].as_str().is_some_and(|c| c.contains("docs get"))) {
            return Err("model did not retrieve a bundled guide".into());
        }
        if !calls.iter().any(|v| v["background"] == true && v["command"].as_str().is_some_and(|c| c.contains("printf SHELLBOY_DOCS_DISCOVERY_V1"))) {
            return Err("model did not directly start the requested background command".into());
        }
        if !calls.iter().any(|v| v["command"].as_str().is_some_and(|c| c.contains("shell read") && c.contains("--execution"))) {
            return Err("model did not read its execution".into());
        }
        let evidence = control.store.database.call(|db| {
            let origin: String = db.query_row("SELECT id FROM shell_execution WHERE json_extract(shell_json,'$.command')='sleep 2; printf SHELLBOY_DOCS_DISCOVERY_V1'", [], |r|r.get(0))?;
            let original = db.prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?.query_map([&origin], |r|r.get::<_,Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.concat();
            let readers = db.prepare("SELECT id FROM shell_execution WHERE instr(json_extract(shell_json,'$.command'),'shell read')>0 AND instr(json_extract(shell_json,'$.command'),?1)>0")?.query_map([&origin], |r|r.get::<_,String>(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            let mut read_exact = false;
            for reader in readers {
                let bytes = db.prepare("SELECT bytes FROM output_chunk WHERE execution_id=?1 AND stream='stdout' ORDER BY sequence")?.query_map([reader], |r|r.get::<_,Vec<u8>>(0))?.collect::<rusqlite::Result<Vec<_>>>()?.concat();
                for line in bytes.split(|b|*b==b'\n') {
                    if let Ok(value) = serde_json::from_slice::<Value>(line) {
                        read_exact |= value["result"]["execution_id"] == origin && value["result"]["text"] == "SHELLBOY_DOCS_DISCOVERY_V1";
                    }
                }
            }
            let child_answers: i64 = db.query_row("SELECT count(*) FROM thread c JOIN turn t ON t.thread_id=c.id JOIN node n ON n.id=t.answer_node_id JOIN run r ON r.id=t.run_id WHERE c.parent_thread_id IS NOT NULL AND r.owner_run_id IS NOT NULL AND t.state='completed' AND n.payload_json LIKE '%SHELLBOY_DOCS_CHILD_V1%'", [], |r|r.get(0))?;
            let child_reads: i64 = db.query_row("SELECT count(*) FROM shell_execution WHERE instr(json_extract(shell_json,'$.command'),'history get')>0", [], |r|r.get(0))?;
            Ok((original, read_exact, child_answers, child_reads))
        }).await.map_err(|e|e.to_string())?;
        if evidence.0 != b"SHELLBOY_DOCS_DISCOVERY_V1" || !evidence.1 {
            return Err("the original execution and its exact shell-read result were not both verified".into());
        }
        if evidence.2 == 0 || evidence.3 == 0 {
            return Err("owned child answer and parent original-history retrieval were not observed".into());
        }
        println!("discovery: bundled guide retrieved, background execution read, {} tool calls", calls.len());
        Ok(())
    }.await;
    control.shutdown().await.unwrap();
    server.shutdown().await.unwrap();
    serving.abort();
    owner.shutdown().await.unwrap();
    if let Err(error) = checked {
        let fixture = root.keep();
        panic!(
            "{error}; synthetic diagnostic fixture retained at {}",
            fixture.display()
        );
    }
}
