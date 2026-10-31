//! Opt-in real-TUI preview fixture. Only a new marked synthetic directory is accepted.
//! An external loopback byte releases the actual DB worker; no time-based success rule.
use clap::Parser;
use serde_json::json;
use shellboy::{
    app::DataOwner,
    cli::Cli,
    control::Control,
    frontend,
    history::append_node,
    ipc::LocalServer,
    protocol::{ApiError, ErrorCode},
    ui::{self, UiAction, actions::ActionStream},
};
use std::{
    net::{TcpListener, TcpStream},
    sync::Arc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
struct NoClipboard;
impl frontend::ClipboardBackend for NoClipboard {
    fn capture(
        &self,
        _: u64,
        _: CancellationToken,
    ) -> frontend::ClipboardFuture<'_, shellboy::attachments::clipboard::ClipboardPixels> {
        Box::pin(async {
            Err(ApiError::new(
                ErrorCode::InvalidState,
                "fixture does not acquire the clipboard",
            ))
        })
    }
    fn copy(
        &self,
        _: String,
        _: shellboy::config::CopyBackend,
        _: usize,
        _: u64,
        _: CancellationToken,
    ) -> frontend::ClipboardFuture<'_, shellboy::attachments::clipboard::CopyOutcome> {
        Box::pin(async {
            Err(ApiError::new(
                ErrorCode::InvalidState,
                "fixture does not copy to the clipboard",
            ))
        })
    }
}
#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let config = cli.config.ok_or("fixture requires explicit --config")?;
    let root = config.parent().ok_or("missing fixture root")?.to_path_buf();
    let data = cli.data_dir.ok_or("fixture requires explicit --data-dir")?;
    if cli.command.is_some()
        || data != root.join("data")
        || data.exists()
        || std::fs::read_to_string(root.join(".preview-fixture"))? != "synthetic-preview-v1\n"
    {
        return Err("only a fresh marked synthetic fixture is accepted".into());
    }
    let settings = shellboy::config::Config::parse(&std::fs::read_to_string(&config)?)?;
    if settings.ui.copy_backend != shellboy::config::CopyBackend::Osc52
        || settings.connections.values().any(|c| {
            !c.base_url.starts_with("http://127.0.0.1:")
                || c.api_key_env.is_some()
                || !c.headers.is_empty()
        })
    {
        return Err("fixture requires OSC52 and unauthenticated loopback configuration".into());
    }
    let owner = DataOwner::start(Some(config), Some(data), root.clone()).await?;
    owner.db.call(|db| {
        let tx=db.transaction()?;
        let now="2026-10-31T00:00:00Z";
        tx.execute_batch("INSERT INTO thread(id,title,cwd,agents_path,profile_id,model_id,updated_at)VALUES('preview-thread','Gated preview','/','/fixture/AGENTS.md','fixture','offline-fixture','2026-10-31T00:00:00Z');
          INSERT INTO run(id,thread_id,state,created_at,ended_at)VALUES('preview-run','preview-thread','completed','2026-10-31T00:00:00Z','2026-10-31T00:00:01Z');
          INSERT INTO turn(id,thread_id,run_id,state,created_at,ended_at)VALUES('preview-turn','preview-thread','preview-run','completed','2026-10-31T00:00:00Z','2026-10-31T00:00:01Z');
          INSERT INTO model_attempt(id,turn_id,generation,selection_revision,profile_id,model_id,reasoning_epoch,state,input_manifest_json,started_at)VALUES('preview-attempt','preview-turn',0,0,'fixture','offline-fixture',0,'completed','{}','2026-10-31T00:00:00Z');
          INSERT INTO tool_group(id,attempt_id,turn_id,state,created_at)VALUES('preview-group','preview-attempt','preview-turn','completed','2026-10-31T00:00:00Z');")?;
        let call=json!({"group_id":"preview-group","output":[{"type":"function_call","call_id":"preview-call","name":"shell","arguments":"{\"command\":\"printf GATED_OUTPUT_END\"}"}]});
        let result=json!({"group_id":"preview-group","item":{"type":"function_call_output","call_id":"preview-call","output":"{\"execution_id\":\"preview-execution\",\"status\":\"exited\",\"exit_code\":1,\"output_complete\":true}"}});
        append_node(&tx,"preview-thread","preview-call-node",Some("preview-turn"),"assistant_response",&call,now)?;
        append_node(&tx,"preview-thread","preview-result-node",Some("preview-turn"),"tool_result",&result,now)?;
        tx.execute("INSERT INTO output_item(attempt_id,ordinal,item_type,status,payload_json,node_id)VALUES('preview-attempt',0,'function_call','completed',?1,'preview-call-node')",[json!({"call_id":"preview-call"}).to_string()])?;
        tx.execute_batch("INSERT INTO tool_call(group_id,call_id,ordinal,tool_name,arguments_json,state,operation_id)VALUES('preview-group','preview-call',0,'shell','{\"command\":\"printf GATED_OUTPUT_END\"}','completed','preview-execution');
          INSERT INTO tool_result(group_id,call_id,node_id,status)VALUES('preview-group','preview-call','preview-result-node','exited');
          INSERT INTO shell_execution(id,owner_run_id,turn_id,operation_id,state,cwd,shell_json,input_encoding,output_encoding,stdin_state,exit_code,output_complete,started_at,ended_at)VALUES('preview-execution','preview-run','preview-turn','preview-execution','exited','/','{\"completion\":{\"elapsed_ms\":1}}','UTF-8','UTF-8','closed',1,1,'2026-10-31T00:00:00Z','2026-10-31T00:00:01Z');")?;
        tx.execute("INSERT INTO output_chunk(execution_id,sequence,stream,stream_offset,bytes,received_at)VALUES('preview-execution',0,'stdout',0,?1,?2)",rusqlite::params![b"GATED_OUTPUT_END\n".as_slice(),now])?;
        tx.commit()?;Ok(())
    }).await?;
    let control = Control::new(&owner, root.clone());
    let settings = control.store.config.clone();
    let (rpc, requests) = mpsc::channel(16);
    let server = LocalServer::bind(&owner, rpc).await?;
    tokio::spawn(control.clone().serve(requests));
    let (input, mut intercepted) = ui::actions::channel(32);
    input.close_tool_starts_on_quit(&control);
    let (forward, actions) = mpsc::channel(32);
    let (updates, incoming) = std::sync::mpsc::sync_channel(128);
    let (release, held) = std::sync::mpsc::sync_channel(1);
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    std::fs::write(root.join("preview-gate-address"), address.to_string())?;
    let signal = release.clone();
    let gate_server = std::thread::spawn(move || {
        if listener.accept().is_ok() {
            let _ = signal.try_send(());
        }
    });
    let database = owner.db.clone();
    let proxy = tokio::spawn(async move {
        let mut held = Some(held);
        let mut requests = 0;
        while let Some(action) = intercepted.next_action().await {
            if matches!(&action, UiAction::LoadToolPreview { .. }) {
                requests += 1;
                let _ = std::fs::write(root.join("preview-read-requests"), requests.to_string());
            }
            if matches!(&action, UiAction::LoadToolPreview { .. })
                && let Some(held) = held.take()
            {
                let (entered, wait) = oneshot::channel();
                let db = database.clone();
                let marker = root.join("preview-gate-entered");
                tokio::spawn(async move {
                    let _ = db
                        .call(move |_| {
                            let _ = std::fs::write(marker, "blocked\n");
                            let _ = entered.send(());
                            let _ = held.recv();
                            Ok(())
                        })
                        .await;
                });
                if wait.await.is_err() {
                    break;
                }
            }
            if matches!(&action, UiAction::Quit | UiAction::ForceQuit) {
                let _ = release.try_send(());
            }
            if forward.send(action).await.is_err() {
                break;
            }
        }
    });
    let bridge = tokio::spawn(frontend::run_with_clipboard(
        control,
        actions,
        updates,
        std::env::current_dir()?,
        owner,
        server,
        Arc::new(NoClipboard),
    ));
    let result = ui::run(&settings, input, incoming);
    let _ = TcpStream::connect(address);
    proxy.abort();
    gate_server.join().map_err(|_| "gate thread panicked")?;
    bridge.await??;
    result?;
    Ok(())
}
