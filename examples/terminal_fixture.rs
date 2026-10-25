//! Local-only manual terminal fixture. It never calls a provider or starts a shell itself,
//! reads attachments from disk, or performs semantic embedding.
use serde_json::{Value, json};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Semaphore,
};
const MAX_BODY: usize = 8 * 1024 * 1024;

async fn request(stream: &mut TcpStream) -> io::Result<(String, Value)> {
    let mut bytes = Vec::new();
    let split = loop {
        let mut chunk = [0; 4096];
        let n = stream.read(&mut chunk).await?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        bytes.extend_from_slice(&chunk[..n]);
        if let Some(at) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
            break at + 4;
        }
        if bytes.len() > 16 * 1024 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "header too large",
            ));
        }
    };
    if split > 16 * 1024 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "header too large",
        ));
    }
    let header = std::str::from_utf8(&bytes[..split]).map_err(|_| io::ErrorKind::InvalidData)?;
    let first = header.lines().next().unwrap_or_default();
    let path = first
        .strip_prefix("POST ")
        .and_then(|s| s.strip_suffix(" HTTP/1.1"))
        .ok_or(io::ErrorKind::InvalidInput)?
        .to_owned();
    let lengths: Vec<usize> = header
        .lines()
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
        .map(|(_, value)| value.trim().parse().map_err(|_| io::ErrorKind::InvalidData))
        .collect::<Result<_, _>>()?;
    if lengths.len() != 1 || lengths[0] > MAX_BODY {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "one Content-Length within 8 MiB is required",
        ));
    }
    let end = split + lengths[0];
    while bytes.len() < end {
        let mut chunk = [0; 8192];
        let n = stream
            .read(&mut chunk[..(end - bytes.len()).min(8192)])
            .await?;
        if n == 0 {
            return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
        }
        bytes.extend_from_slice(&chunk[..n]);
    }
    let body =
        serde_json::from_slice(&bytes[split..end]).map_err(|_| io::ErrorKind::InvalidData)?;
    Ok((path, body))
}
async fn json_reply(stream: &mut TcpStream, status: u16, value: Value) -> io::Result<()> {
    let body = value.to_string();
    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await
}
async fn serve(
    mut stream: TcpStream,
    review_acceptance: bool,
    recovery_failed: &AtomicBool,
) -> io::Result<()> {
    let (path,body)=match tokio::time::timeout(Duration::from_secs(10),request(&mut stream)).await {
        Ok(Ok(value))=>value,
        _=>return json_reply(&mut stream,400,json!({"error":{"message":"Local fixture requires a bounded JSON POST with Content-Length"}})).await,
    };
    if path == "/v1/embeddings" {
        let count = match body.get("input") {
            Some(Value::Array(items)) => items.len(),
            Some(Value::String(_)) => 1,
            _ => 0,
        };
        if count == 0 || count > 128 {
            return json_reply(
                &mut stream,
                400,
                json!({"error":{"message":"Local fixture accepts 1..=128 input items"}}),
            )
            .await;
        }
        let data: Vec<Value> = (0..count)
            .map(|index| json!({"object":"embedding","index":index,"embedding":[1.0,0.0,0.0]}))
            .collect();
        return json_reply(&mut stream,200,json!({"object":"list","data":data,"model":"fixture","usage":{"prompt_tokens":0,"total_tokens":0}})).await;
    }
    if path != "/v1/responses" {
        return json_reply(
            &mut stream,
            404,
            json!({"error":{"message":"Only /v1/responses and /v1/embeddings are available"}}),
        )
        .await;
    }
    let text = body["input"]
        .as_array()
        .into_iter()
        .flatten()
        .rev()
        .filter(|item| item["role"] == "user")
        .find_map(|item| {
            item["content"].as_array().and_then(|parts| {
                parts.iter().rev().find_map(|part| {
                    if part["type"] == "input_text" {
                        part["text"].as_str()
                    } else {
                        None
                    }
                })
            })
        })
        .unwrap_or("");
    // Explicit opt-in scenarios for actual-terminal acceptance. These commands
    // are fixed synthetic output only; the fixture never executes them itself.
    if review_acceptance
        && text == "SHELLBOY_REVIEW_RECOVERY"
        && !recovery_failed.swap(true, Ordering::SeqCst)
    {
        return json_reply(
            &mut stream,
            500,
            json!({"error":{
                "code":"review_fixture_failure",
                "message":"実端末確認用の一度だけの失敗。/retryで同じTurnを再試行してください。"
            }}),
        )
        .await;
    }
    if review_acceptance && text == "SHELLBOY_REVIEW_BACKGROUND" {
        let has_result = body["input"].as_array().into_iter().flatten().any(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "review-background"
        });
        let output = if has_result {
            vec![json!({"type":"message","role":"assistant","content":[{
                "type":"output_text","text":"背景コマンドを受付済みです。保存された終了通知から続行を確認してください。"
            }]})]
        } else {
            vec![json!({"type":"function_call","id":"review-background-item",
                "call_id":"review-background","name":"shell",
                "arguments":json!({"command":"sleep 2; printf REVIEW_BACKGROUND_DONE","background":true}).to_string()
            })]
        };
        return streaming_reply(
            &mut stream,
            json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":output,
                "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}),
            "",
        )
        .await;
    }
    if review_acceptance && text == "SHELLBOY_REVIEW_TOOLS" {
        let has_results = body["input"].as_array().into_iter().flatten().any(|item| {
            item["type"] == "function_call_output" && item["call_id"] == "review-terminal-two"
        });
        if !has_results {
            let output: Vec<Value> = [
                ("one", "printf REVIEW_TOOL_ONE"),
                ("two", "printf REVIEW_TOOL_TWO >&2; exit 7"),
            ]
            .into_iter()
            .map(|(id, command)| {
                json!({
                    "type":"function_call", "id":format!("review-item-{id}"),
                    "call_id":format!("review-terminal-{id}"), "name":"shell",
                    "arguments":json!({"command":command}).to_string()
                })
            })
            .collect();
            return streaming_reply(
                &mut stream,
                json!({
                    "id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":output,
                    "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}
                }),
                "",
            )
            .await;
        }
        let answer = "## コマンドの確認結果\n\n- 一つ目は `REVIEW_TOOL_ONE` を出力して成功しました。\n- 二つ目はエラー側へ `REVIEW_TOOL_TWO` を出力し、終了コード **7** で終了しました。\n\n各呼出しと返却結果の番号・コマンド・状態を照合してください。";
        return streaming_reply(
            &mut stream,
            json!({
                "id":uuid::Uuid::new_v4().to_string(),"status":"completed",
                "output":[{"type":"message","role":"assistant","status":"completed",
                    "content":[{"type":"output_text","text":answer}]}],
                "usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}
            }),
            answer,
        )
        .await;
    }
    if review_acceptance
        && matches!(
            text,
            "SHELLBOY_REVIEW_SHUTDOWN" | "SHELLBOY_REVIEW_CHILD_BACKGROUND"
        )
    {
        let child = text == "SHELLBOY_REVIEW_CHILD_BACKGROUND";
        let call_id = if child {
            "review-child-background"
        } else {
            "review-parent-wait"
        };
        let has_result = body["input"]
            .as_array()
            .into_iter()
            .flatten()
            .any(|item| item["type"] == "function_call_output" && item["call_id"] == call_id);
        let output = if has_result {
            vec![
                json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"専用fixtureの背景処理を保持しています。外部providerは使用していません。"}]}),
            ]
        } else {
            // Real authenticated CLI ownership and wait; no fixture DB forgery.
            // This opt-in Unix manual scenario requires python3 in the inherited PATH.
            let command = if child {
                "printf SHELLBOY_REVIEW_CHILD_RUNNING; sleep 120"
            } else {
                r#"python3 - <<'PY'
import json, os, subprocess

def call(*args):
    completed = subprocess.run(["shellboy", *args], capture_output=True, text=True, check=True)
    reply = json.loads(completed.stdout)
    if not reply["ok"]:
        raise RuntimeError("Fixture CLI rejected a request")
    return reply["result"]

child = call("thread", "create", "--parent", os.environ["SHELLBOY_THREAD_ID"])["thread_id"]
accepted = call("message", "send", "--thread", child, "--text", "SHELLBOY_REVIEW_CHILD_BACKGROUND")
print("SHELLBOY_REVIEW_PARENT_WAITING", flush=True)
call("wait", "--conditions-json", json.dumps([{"type":"run_ended", "run_id":accepted["run_id"]}]), "--timeout-ms", "120000")
PY"#
            };
            vec![
                json!({"type":"function_call","id":call_id,"call_id":call_id,"name":"shell","arguments":json!({"command":command,"background":child}).to_string()}),
            ]
        };
        return streaming_reply(&mut stream, json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":output,"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}}), "").await;
    }
    let preview: String = text.chars().take(4000).collect();
    let answer = format!(
        "Local fixture / ローカル確認用\n受信テキスト: {}文字（表示は先頭4000文字まで）\n{}\n画像・fileの意味解析、実モデル互換、検索の意味品質はこのfixtureでは検証しません。",
        text.chars().count(),
        preview
    );
    let response = json!({"id":uuid::Uuid::new_v4().to_string(),"status":"completed","output":[{"type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":answer}]}],"usage":{"input_tokens":0,"output_tokens":0,"total_tokens":0}});
    streaming_reply(&mut stream, response, &answer).await
}
async fn streaming_reply(stream: &mut TcpStream, response: Value, answer: &str) -> io::Result<()> {
    let first = format!(
        "data: {}\n\n",
        json!({"type":"response.output_text.delta","delta":answer})
    );
    let last = format!(
        "data: {}\n\n",
        json!({"type":"response.completed","response":response})
    );
    stream.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",first.len()+last.len()).as_bytes()).await?;
    stream.write_all(first.as_bytes()).await?;
    tokio::time::sleep(Duration::from_millis(50)).await;
    stream.write_all(last.as_bytes()).await
}
#[tokio::main]
async fn main() -> io::Result<()> {
    let port = std::env::args()
        .nth(1)
        .map(|s| s.parse::<u16>())
        .transpose()
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "Port must be 0..=65535"))?
        .unwrap_or(0);
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    println!("Local fixture only; stop with Ctrl+C. No request bodies are logged.");
    println!(
        "[connections.fixture]\nbase_url = \"http://{}/v1\"",
        listener.local_addr()?
    );
    let review_acceptance = std::env::args().nth(2).as_deref() == Some("--review-acceptance");
    let recovery_failed = Arc::new(AtomicBool::new(false));
    if review_acceptance {
        println!(
            "Review mode: SHELLBOY_REVIEW_RECOVERY fails once; SHELLBOY_REVIEW_TOOLS requests two fixed harmless shell commands; SHELLBOY_REVIEW_BACKGROUND requests fixed background output; SHELLBOY_REVIEW_SHUTDOWN delegates a child sleep120 and waits through real authenticated CLI (Unix/python3 only)."
        );
    }
    let slots = Arc::new(Semaphore::new(16));
    let mut tasks = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            _=tokio::signal::ctrl_c()=>break,
            accepted=listener.accept()=>{
                let (stream,_)=accepted?;
                let Ok(permit)=slots.clone().try_acquire_owned()else{drop(stream);continue;};
                let recovery_failed=recovery_failed.clone();
                tasks.spawn(async move{let _permit=permit;if serve(stream,review_acceptance,&recovery_failed).await.is_err(){eprintln!("Local fixture connection ended before response completed");}});
            },
            Some(_)=tasks.join_next()=>{},
        }
    }
    tasks.abort_all();
    while tasks.join_next().await.is_some() {}
    Ok(())
}
