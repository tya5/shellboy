//! Synthetic, credential-free wire fixtures; no provider interoperability claim.
use serde_json::{Value, json};
use shellboy::{
    config::{Api, ApiEndpoint, Config, Connection},
    model::{
        dialect::{self, Stream},
        http::HttpModelClient,
        response_request,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::Duration,
};
use tokio_util::sync::CancellationToken;
const TEXT: &str = include_str!("fixtures/chat/text-usage.sse");
const TOOLS: &str = include_str!("fixtures/chat/tools.sse");
const STOP_TOOLS: &str = include_str!("fixtures/chat/stop-complete-tools.sse");
const REPEATED_NAME: &str = include_str!("fixtures/chat/repeated-function-name.sse");
const MISSING_IDS: &str = include_str!("fixtures/chat/missing-tool-id.sse");
const USAGE_NO_CHOICES: &str = include_str!("fixtures/chat/usage-no-choices.sse");
const LENGTH: &str = include_str!("fixtures/chat/length.sse");
const UNKNOWN: &str = include_str!("fixtures/chat/missing-usage.sse");
const CONTEXT: &str = include_str!("fixtures/chat/context-error.sse");

const JSON_REPLY: &str = r#"{"id":"json-fixture","choices":[{"index":0,"message":{"role":"assistant","content":"Hello"},"finish_reason":"stop"}],"usage":{"prompt_tokens":2}}"#;
const JSON_LENGTH: &str = r#"{"id":"json-length","choices":[{"index":0,"message":{"role":"assistant","content":"partial"},"finish_reason":"length"}],"usage":{"completion_tokens":3}}"#;
const PARTIAL: &str = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"part\"},\"finish_reason\":null}]}\n\n";
fn request() -> Value {
    response_request(
        "fixture",
        vec![json!({"role":"user","content":[{"type":"input_text","text":"fixture"}]})],
        Some("low"),
        true,
    )
}
fn connection(url: String) -> Connection {
    Connection {
        api: Api::ChatCompletions,
        base_url: url,
        ..Default::default()
    }
}

#[test]
fn default_endpoint_configuration_and_stateless_replay_mapping() {
    assert_eq!(Connection::default().api, Api::Responses);
    let c = Config::parse(
        "[connections.test]\napi='chat_completions'\nbase_url='http://127.0.0.1:1/v1/'",
    )
    .unwrap();
    assert_eq!(
        c.connections["test"]
            .endpoint(ApiEndpoint::Responses)
            .unwrap()
            .path(),
        "/v1/chat/completions"
    );
    assert_eq!(
        c.connections["test"]
            .endpoint(ApiEndpoint::Models)
            .unwrap()
            .path(),
        "/v1/models"
    );
    assert_eq!(
        c.connections["test"]
            .endpoint(ApiEndpoint::Embeddings)
            .unwrap()
            .path(),
        "/v1/embeddings"
    );
    for extra in [
        "api='unknown'",
        "api='chat_completions'\nprompt_cache_key=true",
    ] {
        assert!(
            Config::parse(&format!(
                "[connections.test]\nbase_url='http://localhost/v1'\n{extra}"
            ))
            .is_err()
        );
    }
    let original = response_request(
        "fixture",
        vec![
            json!({"role":"system","content":[{"type":"input_text","text":"root"}]}),
            json!({"role":"developer","content":[{"type":"input_text","text":"instructions"}]}),
            json!({"role":"user","content":[{"type":"input_text","text":"look"},{"type":"input_image","image_url":"data:image/png;base64,Zml4dHVyZQ==","detail":"low"}]}),
            json!({"type":"reasoning","encrypted_content":"not-replayed","summary":[]}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"tools"}]}),
            json!({"type":"function_call","call_id":"a","name":"shell","arguments":"{}"}),
            json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"continued"}]}),
            json!({"type":"function_call","call_id":"b","name":"shell","arguments":"{}"}),
            json!({"type":"function_call_output","call_id":"a","output":"first"}),
            json!({"type":"function_call_output","call_id":"b","output":"second"}),
        ],
        Some("low"),
        true,
    );
    assert_eq!(
        dialect::encode(Api::Responses, original.clone()).unwrap(),
        original
    );
    let chat = dialect::encode(Api::ChatCompletions, original).unwrap();
    assert!(
        chat.get("input").is_none() && chat.get("include").is_none() && chat.get("store").is_none()
    );
    assert!(!chat.to_string().contains("not-replayed"));
    assert_eq!(chat["stream_options"]["include_usage"], true);
    assert_eq!(chat["reasoning_effort"], "low");
    assert_eq!(chat["messages"][1]["role"], "system");
    assert_eq!(
        chat["messages"][2]["content"][1]["image_url"]["detail"],
        "low"
    );
    assert_eq!(
        chat["messages"][3]["tool_calls"].as_array().unwrap().len(),
        2
    );
    assert_eq!(chat["messages"][4]["tool_call_id"], "a");
    assert_eq!(chat["messages"][5]["tool_call_id"], "b");
    assert_eq!(chat["tools"][0]["function"]["name"], "shell");
}
#[test]
fn recorded_chunk_boundaries_multiple_calls_and_actual_usage_only() {
    for size in [1, 2, 7, 64, 4096] {
        let mut stream = Stream::new(Api::ChatCompletions);
        for bytes in TEXT.as_bytes().chunks(size) {
            stream.push(bytes).unwrap();
        }
        assert!(stream.is_terminal());
        let result = stream.finish(true).unwrap();
        assert_eq!(result.text, "Hello");
        assert_eq!(
            result.raw["usage"],
            json!({"input_tokens":120,"input_tokens_details":{"cached_tokens":100},"output_tokens":7,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":127})
        );
    }
    let mut stream = Stream::new(Api::ChatCompletions);
    for bytes in TOOLS.as_bytes().chunks(3) {
        stream.push(bytes).unwrap();
    }
    let result = stream.finish(true).unwrap();
    assert_eq!(result.calls.len(), 2);
    assert_eq!(result.calls[0].call_id, "call-a");
    assert_eq!(result.calls[0].args.as_ref().unwrap().command, "echo one");
    assert_eq!(result.calls[1].args.as_ref().unwrap().command, "echo two");
    assert_eq!(result.raw["usage"], json!({"input_tokens":42}));
    let mut stream = Stream::new(Api::ChatCompletions);
    stream.push(UNKNOWN.as_bytes()).unwrap();
    assert!(stream.finish(true).unwrap().raw.get("usage").is_none());
}
#[test]
fn incomplete_length_disconnect_explicit_errors_and_terminal_junk_do_not_invent_success() {
    let mut stream = Stream::new(Api::ChatCompletions);
    stream.push(LENGTH.as_bytes()).unwrap();
    assert!(stream.is_terminal());
    let error = stream.finish(true).unwrap_err();
    assert_eq!(error.kind, "incomplete");
    assert!(!error.is_context_limit());
    assert!(error.message.contains("length"));
    let mut stream = Stream::new(Api::ChatCompletions);
    stream.push(CONTEXT.as_bytes()).unwrap();
    assert!(stream.finish(true).unwrap_err().is_context_limit());
    let mut stream = Stream::new(Api::ChatCompletions);
    stream.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"part\"},\"finish_reason\":null}]}\n\n").unwrap();
    assert!(stream.has_output());
    assert_eq!(stream.partial_text, "part");
    assert_eq!(stream.finish(true).unwrap_err().kind, "incomplete");
    let mut stream = Stream::new(Api::ChatCompletions);
    stream
        .push(format!("{TEXT}data: invalid trailing JSON\n\n").as_bytes())
        .unwrap();
    assert_eq!(stream.finish(true).unwrap().text, "Hello");
    let mut stream = Stream::new(Api::ChatCompletions);
    assert!(stream.push(b"data: [DONE]\n\n").is_err());
    assert!(
        stream.is_terminal(),
        "a received DONE is a boundary even when validation fails"
    );
    let mut ordered = Stream::new(Api::ChatCompletions);
    for delta in [json!({"refusal":"first"}), json!({"content":"second"})] {
        ordered
            .push(
                format!(
                    "data: {}\n\n",
                    json!({"choices":[{"index":0,"delta":delta,"finish_reason":null}]})
                )
                .as_bytes(),
            )
            .unwrap();
    }
    ordered.push(b"data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n").unwrap();
    assert_eq!(ordered.finish(true).unwrap().text, "firstsecond");

    assert!(!shellboy::model::ModelError::provider(Some(400),&json!({"error":{"code":"invalid_request_error","message":"context words are not structured proof"}})).is_context_limit());
}
async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let mut head_end = None;
    let mut length = 0;
    loop {
        if let Some(end) = head_end
            && bytes.len() >= end + length
        {
            break;
        }
        let mut buffer = [0; 4096];
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0 && bytes.len() < 8 * 1024 * 1024);
        bytes.extend_from_slice(&buffer[..n]);
        if head_end.is_none()
            && let Some(index) = bytes.windows(4).position(|v| v == b"\r\n\r\n")
        {
            let end = index + 4;
            let head = String::from_utf8_lossy(&bytes[..end]);
            length = head
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
                .unwrap_or(0);
            head_end = Some(end);
        }
    }
    let end = head_end.unwrap();
    (
        String::from_utf8(bytes[..end].to_vec()).unwrap(),
        if length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&bytes[end..end + length]).unwrap()
        },
    )
}
async fn reply(stream: &mut tokio::net::TcpStream, status: u16, kind: &str, body: &str) {
    stream.write_all(format!("HTTP/1.1 {status} Fixture\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_transport_headers_error_facts_and_partial_usage_share_the_existing_client() {
    crate::test_watchdog::watchdog("tests/chat_completions.rs: loopback_transport_headers_error_facts_and_partial_usage_share_the_existing_client", async {
        let usage_cutoff = TEXT.strip_suffix("data: [DONE]\n\n").unwrap();
        for (status, body, kind) in [
            (200, TEXT, "text/event-stream"),
            (200, JSON_REPLY, "application/json"),
            (200, JSON_LENGTH, "application/json"),
            (200, PARTIAL, "text/event-stream"),
            (200, usage_cutoff, "text/event-stream"),
            (200, LENGTH, "text/event-stream"),
            (
                400,
                r#"{"error":{"code":"context_length_exceeded","message":"fixture limit"}}"#,
                "application/json",
            ),
            (
                429,
                r#"{"error":{"code":"rate_limit","message":"fixture limited"}}"#,
                "application/json",
            ),
            (
                503,
                r#"{"error":{"message":"fixture unavailable"}}"#,
                "application/json",
            ),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                let (head, body_in) = read_request(&mut stream).await;
                assert!(head.starts_with("POST /v1/chat/completions HTTP/1.1"));
                assert!(
                    head.to_ascii_lowercase()
                        .contains("x-shellboy-session-id: fixture-thread")
                );
                assert!(body_in.get("input").is_none());
                assert_eq!(body_in["stream_options"]["include_usage"], true);
                reply(&mut stream, status, kind, body).await;
            });
            let client =
                HttpModelClient::for_connection("chat", connection(format!("http://{address}/v1")))
                    .unwrap();
            let result = client
                .response_for_thread("fixture-thread", request(), CancellationToken::new(), None)
                .await;
            if status == 200 && (body == TEXT || body == JSON_REPLY) {
                assert_eq!(result.unwrap().text, "Hello");
            } else {
                let failure = result.unwrap_err();
                assert_eq!(failure.facts.status, Some(status));
                if status == 400 {
                    assert!(failure.error.is_context_limit());
                }
                if status == 200 && (body == LENGTH || body == JSON_LENGTH) {
                    assert_eq!(failure.facts.received, shellboy::model::http::ResponseReceived::Output);
                    assert!(failure.facts.terminal_received);
                    assert_eq!(failure.raw_response.as_ref().unwrap()["usage"]["output_tokens"], 3);
                }
                if body == usage_cutoff {
                    assert!(!failure.facts.terminal_received);
                    assert_eq!(failure.raw_response.as_ref().unwrap()["usage"]["input_tokens_details"]["cached_tokens"], 100);
                }
                if body == PARTIAL {
                    assert!(!failure.facts.terminal_received);
                    assert_eq!(failure.partial_text, "part");
                    assert_eq!(failure.error.kind, "incomplete");
                }
            }
            server.await.unwrap();
        }
    })
    .await
    .unwrap();
}
#[tokio::test]
async fn chat_inactivity_timeout_is_a_shared_deadline_with_a_controlled_clock() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        read_request(&mut stream).await;
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\ndata: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"part\"},\"finish_reason\":null}]}\n\n").await.unwrap();
        let _ = stopped.await;
    });
    let client = HttpModelClient::for_connection(
        "chat",
        Connection {
            stream_idle_timeout_ms: 60_000,
            ..connection(format!("http://{address}/v1"))
        },
    )
    .unwrap();
    let (events, mut receive) = tokio::sync::mpsc::channel(8);
    let attempt = tokio::spawn(async move {
        client
            .response(request(), CancellationToken::new(), Some(events))
            .await
    });
    crate::test_watchdog::watchdog("tests/chat_completions.rs: chat_inactivity_timeout_is_a_shared_deadline_with_a_controlled_clock", receive.recv()).await.unwrap().unwrap();
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(61)).await;
    let failure = crate::test_watchdog::watchdog("tests/chat_completions.rs: chat_inactivity_timeout_is_a_shared_deadline_with_a_controlled_clock", attempt).await.unwrap().unwrap().unwrap_err();
    tokio::time::resume();
    assert_eq!(failure.error.kind, "timeout");
    assert_eq!(failure.partial_text, "part");
    assert!(!failure.facts.terminal_received);
    let _ = stop.send(());
    crate::test_watchdog::watchdog("tests/chat_completions.rs: chat_inactivity_timeout_is_a_shared_deadline_with_a_controlled_clock", server).await.unwrap().unwrap();
}

async fn wait_turn(
    control: &std::sync::Arc<shellboy::control::Control>,
    instance: &str,
    turn: &str,
    changes: &mut tokio::sync::broadcast::Receiver<shellboy::control::EngineEvent>,
) {
    use shellboy::protocol::*;
    crate::test_watchdog::watchdog("tests/chat_completions.rs: wait_turn", async {
        loop {
            let reply = control
                .request(Request::new(
                    format!("{instance}:{}", uuid::Uuid::new_v4()),
                    Command::TurnGet(TurnTarget {
                        turn_id: turn.into(),
                    }),
                ))
                .await;
            let value = reply.result.unwrap();
            if value["state"] == "completed" {
                break;
            }
            assert_ne!(value["state"], "recovery_wait", "{value}");
            match changes.recv().await {
                Ok(_) | Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {}
                Err(error) => panic!("notification closed: {error}"),
            }
        }
    })
    .await
    .unwrap();
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn core_retries_tools_context_reduction_and_saved_cache_usage_are_shared() {
    use shellboy::{app::DataOwner, control::Control, protocol::*};
    crate::test_watchdog::watchdog("tests/chat_completions.rs: core_retries_tools_context_reduction_and_saved_cache_usage_are_shared",async {
        let root=tempfile::tempdir().unwrap();
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();let address=listener.local_addr().unwrap();
        let config=root.path().join("config.toml");
        std::fs::write(&config,format!("default_connection='chat'\ndefault_model='fixture'\n[connections.chat]\napi='chat_completions'\nbase_url='http://{address}/v1'\nresponse_retry_max_delay_ms=1\n")).unwrap();
        let owner=DataOwner::start(Some(config),Some(root.path().join("data")),root.path().into()).await.unwrap();
        let control=Control::new(&owner,root.path().into());control.start().await.unwrap();
        let req=|command|Request::new(format!("{}:{}",owner.instance_id,uuid::Uuid::new_v4()),command);
        let created=control.request(req(Command::ThreadCreate(ThreadCreateArgs{agents:None,connection:None,cwd:None,model:None,effort:None,parent_id:None}))).await;
        assert!(created.ok,"{created:?}");let thread=created.result.unwrap()["thread_id"].as_str().unwrap().to_owned();
        let expected_thread=thread.clone();
        let server=tokio::spawn(async move {
            let mut requests=Vec::new();
            for index in 0..7 {
                let (mut stream,_)=listener.accept().await.unwrap();
                let (head,body)=read_request(&mut stream).await;
                if index==6 {
                    assert!(head.starts_with("GET /v1/models"));reply(&mut stream,200,"application/json",r#"{"data":[{"id":"fixture"}]}"#).await;
                } else {
                    assert!(head.starts_with("POST /v1/chat/completions"));
                    assert!(head.to_ascii_lowercase().contains(&format!("x-shellboy-session-id: {expected_thread}")));
                    match index {
                        0=>reply(&mut stream,503,"application/json",r#"{"error":{"message":"temporary fixture"}}"#).await,
                        1=>reply(&mut stream,429,"application/json",r#"{"error":{"code":"rate_limit","message":"temporary fixture"}}"#).await,
                        2=>reply(&mut stream,200,"text/event-stream",STOP_TOOLS).await,
                        4=>reply(&mut stream,400,"application/json",r#"{"error":{"code":"context_length_exceeded","message":"explicit synthetic overflow"}}"#).await,
                        _=>reply(&mut stream,200,"text/event-stream",TEXT).await,
                    }
                }
                requests.push(body);
            }
            requests
        });
        let mut changes=control.subscribe();
        for text in ["first fixture","second fixture"] {
            let sent=control.request(req(Command::MessageSend(MessageSendArgs{sender_label:None,thread_id:Some(thread.clone()),text:text.into(),now:false}))).await;
            assert!(sent.ok,"{sent:?}");let turn=sent.result.unwrap()["turn_id"].as_str().unwrap().to_owned();
            wait_turn(&control,&owner.instance_id,&turn,&mut changes).await;
        }
        let usage=control.request(req(Command::UsageGet(UsageGetArgs{thread_id:Some(thread.clone()),..Default::default()}))).await;
        assert!(usage.ok,"{usage:?}");let usage=usage.result.unwrap();
        assert_eq!(usage["model_attempts"]["completed"]["count"],3,"{usage}");
        assert_eq!(usage["model_attempts"]["completed"]["cached_tokens"]["known_sum"],200,"{usage}");
        assert_eq!(usage["model_attempts"]["completed"]["cached_tokens"]["unknown_count"],1);
        let catalog=control.request(req(Command::ModelList(ModelListArgs{connection:Some("chat".into()),thread_id:None}))).await;
        assert!(catalog.ok,"{catalog:?}");let catalog=catalog.result.unwrap();
        assert_eq!(catalog["api"],"chat_completions");
        assert!(catalog["unavailable_features"].as_array().unwrap().contains(&json!("encrypted_reasoning")));
        let requests=server.await.unwrap();
        assert_eq!(requests[0]["messages"],requests[1]["messages"]);
        let results=requests[3]["messages"].as_array().unwrap().iter().filter(|item|item["role"]=="tool").collect::<Vec<_>>();
        assert_eq!(results.len(),2);assert!(results.iter().any(|item|item["content"].as_str().unwrap().contains("one")));
        assert!(requests[4]["messages"].as_array().unwrap().iter().any(|item|item["role"]=="tool"));
        assert_ne!(requests[4]["messages"], requests[5]["messages"]);
        let reduced = owner.db.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM model_attempt WHERE json_valid(error_json) AND json_extract(error_json,'$.automatic_trim_attempt.outcome')='adopted'", [], |row|row.get::<_,i64>(0))?)).await.unwrap();
        assert_eq!(reduced,1,"explicit overflow must use the shared reduction path");
        let kept = owner.db.call(|db| Ok(db.query_row("SELECT COUNT(*) FROM node WHERE kind='assistant_response' AND json_valid(payload_json) AND json_extract(payload_json,'$.output[0].chat_completions.finish_reason')='stop' AND json_extract(payload_json,'$.output[0].chat_completions.stop_with_complete_tool_calls')=1", [], |row|row.get::<_,i64>(0))?)).await.unwrap();
        assert_eq!(kept,1,"accepted stop/tool-call facts must survive Core storage");

        control.shutdown().await.unwrap();owner.shutdown().await.unwrap();
    }).await.unwrap();
}

fn jitter_trace(trace: &str, size: usize) -> shellboy::model::ModelResponse {
    let mut stream = Stream::new(Api::ChatCompletions);
    for bytes in trace.as_bytes().chunks(size) {
        stream.push(bytes).unwrap();
    }
    stream.finish(true).unwrap()
}
#[test]
fn d010_complete_stop_calls_and_repeated_full_names_preserve_facts() {
    for size in [1, 7, 4096] {
        let result = jitter_trace(STOP_TOOLS, size);
        assert_eq!(result.calls.len(), 2);
        assert!(result.calls.iter().all(|call| call.args.is_ok()));
        assert_eq!(result.raw["finish_reason"], "stop");
        for item in &result.output {
            assert_eq!(item["chat_completions"]["finish_reason"], "stop");
            assert_eq!(
                item["chat_completions"]["stop_with_complete_tool_calls"],
                true
            );
        }
        let result = jitter_trace(REPEATED_NAME, size);
        assert_eq!(
            result.calls[0].args.as_ref().unwrap().command,
            "echo repeated"
        );
        assert_eq!(result.output[0]["name"], "shell");
        assert_eq!(
            result.output[0]["chat_completions"]["repeated_full_name_deltas"],
            1
        );
    }
}
#[test]
fn d010_missing_ids_are_stable_per_response_and_index_and_replay_consistently() {
    let baseline = jitter_trace(MISSING_IDS, 1);
    assert_eq!(baseline.calls.len(), 2);
    let ids = baseline
        .calls
        .iter()
        .map(|call| call.call_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        ids[0],
        "call_sb_1482c0ddd2fcae00f5c6546169d3058e1e3adf342b3f8096"
    );
    assert_ne!(ids[0], ids[1]);
    assert!(
        ids.iter()
            .all(|id| id.starts_with("call_sb_") && id.len() == 56)
    );
    for size in [2, 19, 4096] {
        let result = jitter_trace(MISSING_IDS, size);
        assert_eq!(
            result
                .calls
                .iter()
                .map(|call| call.call_id.clone())
                .collect::<Vec<_>>(),
            ids
        );
    }
    let different = jitter_trace(
        &MISSING_IDS.replace("chat-missing-id", "another-response"),
        3,
    );
    assert_ne!(different.calls[0].call_id, ids[0]);
    for (index, item) in baseline.output.iter().enumerate() {
        assert_eq!(item["chat_completions"]["index"], index);
        assert_eq!(item["chat_completions"]["id_synthesized"], true);
        assert_eq!(item["chat_completions"]["response_id"], "chat-missing-id");
        assert_eq!(item["chat_completions"]["original_id"], Value::Null);
        assert_eq!(
            item["chat_completions"]["id_source"],
            "sha256_response_id_index"
        );
    }
    let mut input = baseline.output;
    for id in &ids {
        input.push(json!({"type":"function_call_output","call_id":id,"output":"fixture result"}));
    }
    let encoded = dialect::encode(
        Api::ChatCompletions,
        response_request("fixture", input, None, true),
    )
    .unwrap();
    for (index, id) in ids.iter().enumerate() {
        assert_eq!(encoded["messages"][0]["tool_calls"][index]["id"], *id);
        assert_eq!(encoded["messages"][index + 1]["tool_call_id"], *id);
    }
    let decoded=dialect::decode_json(Api::ChatCompletions,json!({"id":"chat-missing-id","choices":[{"index":0,"message":{"role":"assistant","tool_calls":[{"type":"function","function":{"name":"shell","arguments":"{}"}}]},"finish_reason":"tool_calls"}]})).unwrap();
    assert_eq!(decoded["output"][0]["call_id"], ids[0]);
}
#[test]
fn d010_omitted_choices_usage_is_retained_without_inventing_token_counters() {
    let result = jitter_trace(USAGE_NO_CHOICES, 2);
    assert_eq!(result.text, "Hello");
    assert_eq!(
        result.raw["usage"]["input_tokens_details"]["cached_tokens"],
        100
    );
    assert_eq!(
        result.raw["chat_completions"]["usage_chunks_without_choices"],
        1
    );
    assert_eq!(
        result.output[0]["chat_completions"]["usage_chunks_without_choices"],
        1
    );
    let empty = "data: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: {\"usage\":{}}\n\ndata: [DONE]\n\n";
    let result = jitter_trace(empty, 1);
    assert_eq!(result.raw["usage"], json!({}));
    assert_eq!(
        result.output[0]["chat_completions"]["usage_chunks_without_choices"], 1,
        "facts survive an otherwise empty answer"
    );
}
#[test]
fn d010_does_not_accept_incomplete_calls_or_guess_missing_response_identity() {
    let base = "data: {\"id\":\"negative\",\"choices\":[{\"index\":0,\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call-a\",\"function\":{\"name\":\"shell\",\"arguments\":\"{}\"}}]},\"finish_reason\":null}]}\n\ndata: {\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
    for invalid in [
        base.replace("\"arguments\":\"{}\"", "\"arguments\":\"{\""),
        base.replace("\"arguments\":\"{}\"", "\"arguments\":\"\""),
        base.replace("\"arguments\":\"{}\"", "\"arguments\":\"[]\""),
        base.replace("\"name\":\"shell\"", "\"name\":\"\""),
        base.replace("\"id\":\"negative\",", "")
            .replace("\"id\":\"call-a\",", ""),
    ] {
        assert_ne!(invalid, base);
        let mut stream = Stream::new(Api::ChatCompletions);
        assert!(stream.push(invalid.as_bytes()).is_err());
        assert!(stream.is_terminal());
    }
    let mut stream = Stream::new(Api::ChatCompletions);
    assert!(stream.push(b"data: {\"id\":\"not-usage\"}\n\n").is_err());
    let mut oversized = Stream::new(Api::ChatCompletions);
    let error = oversized
        .push(format!("data: {}\n\n", json!({"id":"x".repeat(1025),"choices":[]})).as_bytes())
        .unwrap_err();
    assert_eq!(error.kind, "input_limit");
    let mut oversized_reason = Stream::new(Api::ChatCompletions);
    let error = oversized_reason
        .push(
            format!(
                "data: {}\n\n",
                json!({"choices":[{"index":0,"delta":{},"finish_reason":"x".repeat(65)}]})
            )
            .as_bytes(),
        )
        .unwrap_err();
    assert_eq!(error.kind, "input_limit");

    let fragments = REPEATED_NAME.replacen("\"name\":\"shell\"", "\"name\":\"s\"", 1);
    let result = jitter_trace(&fragments, 3);
    assert!(
        result.calls[0].args.is_err(),
        "partial names must not be repaired into the declared function"
    );
    assert_eq!(
        result.output[0]["chat_completions"]["repeated_full_name_deltas"],
        0
    );
}
