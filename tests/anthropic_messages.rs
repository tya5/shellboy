//! Synthetic, credential-free Anthropic Messages fixtures (#120 stage 3). They
//! check Shellboy's own request and stream handling against the documented wire
//! shapes; they make no provider interoperability claim.
use serde_json::{Value, json};
use shellboy::{
    config::{Api, Config, Connection},
    model::{
        dialect::{self, ANTHROPIC_VERSION, Stream},
        http::HttpModelClient,
        opaque, response_request,
    },
};
use std::collections::BTreeMap;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

const TEXT: &str = include_str!("fixtures/messages/text-usage.sse");
const TOOL_THINKING: &str = include_str!("fixtures/messages/tool-thinking.sse");
const OVERLOADED: &str = include_str!("fixtures/messages/overloaded.sse");

fn stop_fixture(reason: &str) -> &'static str {
    match reason {
        "refusal" => include_str!("fixtures/messages/stop-refusal.sse"),
        "pause_turn" => include_str!("fixtures/messages/stop-pause_turn.sse"),
        "max_tokens" => include_str!("fixtures/messages/stop-max_tokens.sse"),
        "model_context_window_exceeded" => {
            include_str!("fixtures/messages/stop-model_context_window_exceeded.sse")
        }
        "novel_reason" => include_str!("fixtures/messages/stop-novel_reason.sse"),
        other => panic!("no fixture for {other}"),
    }
}

fn user(text: &str) -> Value {
    json!({"role":"user","content":[{"type":"input_text","text":text}]})
}
fn request(extra: Vec<Value>) -> Value {
    let mut input = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"You are Shellboy."}]}),
        user("run it"),
    ];
    input.extend(extra);
    response_request("claude-fixture", input, Some("low"), true)
}
fn shell_call(id: &str) -> Value {
    json!({"type":"function_call","call_id":id,"name":"shell","arguments":"{\"command\":\"printf ok\"}","status":"completed"})
}
fn shell_result(id: &str) -> Value {
    json!({"type":"function_call_output","call_id":id,"output":"ok"})
}

/// Feed a whole fixture, split at awkward byte offsets, through the stream reader.
fn run(body: &str, chunk: usize) -> Result<shellboy::model::ModelResponse, shellboy::model::ModelError> {
    let mut stream = Stream::new(Api::AnthropicMessages);
    for piece in body.as_bytes().chunks(chunk) {
        stream.push(piece)?;
    }
    stream.finish(true)
}

#[test]
fn request_has_only_what_the_conversation_needs() {
    let wire = dialect::encode_with(Api::AnthropicMessages, request(vec![]), 32768).unwrap();
    let mut keys: Vec<&str> = wire.as_object().unwrap().keys().map(String::as_str).collect();
    keys.sort();
    assert_eq!(keys, ["max_tokens", "messages", "model", "stream", "system", "tools"]);
    assert_eq!(wire["max_tokens"], 32768);
    assert_eq!(wire["stream"], true);
    assert_eq!(wire["system"], "You are Shellboy.");
    // Nothing optional or provider-specific is added on the user's behalf, and
    // the Shellboy effort setting is not translated into thinking.
    for absent in [
        "thinking", "temperature", "top_p", "top_k", "tool_choice", "cache_control",
        "metadata", "stop_sequences", "reasoning", "reasoning_effort", "store", "include",
    ] {
        assert!(wire.get(absent).is_none(), "{absent} must not be sent");
    }
    assert_eq!(wire["messages"].as_array().unwrap().len(), 1);
    assert_eq!(wire["messages"][0]["role"], "user");
    let tool = &wire["tools"][0];
    assert_eq!(tool["name"], "shell");
    assert!(tool["input_schema"].is_object());
    assert!(tool.get("parameters").is_none() && tool.get("type").is_none());
}

#[test]
fn tool_round_trip_maps_to_tool_use_and_leading_tool_results() {
    let call_a = shell_call("toolu_a");
    let call_b = shell_call("toolu_b");
    let wire = dialect::encode_with(
        Api::AnthropicMessages,
        request(vec![
            call_a,
            call_b,
            shell_result("toolu_a"),
            shell_result("toolu_b"),
            // Context inserted after the results must follow them in the same user turn.
            json!({"role":"developer","content":[{"type":"input_text","text":"Shellboy event v1: {}"}]}),
        ]),
        100,
    )
    .unwrap();
    let messages = wire["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{wire}");
    assert_eq!(messages[1]["role"], "assistant");
    let uses = messages[1]["content"].as_array().unwrap();
    assert_eq!(uses.len(), 2);
    assert_eq!(uses[0]["type"], "tool_use");
    assert_eq!(uses[0]["id"], "toolu_a");
    assert_eq!(uses[0]["input"], json!({"command":"printf ok"}));
    let results = messages[2]["content"].as_array().unwrap();
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(results[0]["type"], "tool_result");
    assert_eq!(results[0]["tool_use_id"], "toolu_a");
    assert_eq!(results[1]["tool_use_id"], "toolu_b");
    assert_eq!(results[2]["type"], "text");
    assert!(results[2]["text"].as_str().unwrap().starts_with("Shellboy event v1:"));
}

#[test]
fn inserted_context_is_a_user_message_and_the_conversation_must_end_with_one() {
    let notice = json!({"role":"developer","content":[{"type":"input_text","text":"Shellboy event v1: {}"}]});
    let assistant = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"done"}]});
    // A notice after an assistant answer ends the request with a user message.
    let wire = dialect::encode_with(
        Api::AnthropicMessages,
        request(vec![assistant.clone(), notice]),
        100,
    )
    .unwrap();
    let last = wire["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["role"], "user");
    // Replay ending with an assistant message is refused locally: no prefill is sent.
    let error = dialect::encode_with(Api::AnthropicMessages, request(vec![assistant]), 100).unwrap_err();
    assert_eq!(error.kind, "input_invalid");
}

#[test]
fn images_map_to_base64_and_url_sources() {
    let image = |url: &str| {
        json!({"role":"user","content":[{"type":"input_image","image_url":url}]})
    };
    let wire = dialect::encode_with(
        Api::AnthropicMessages,
        request(vec![image("data:image/png;base64,AAAA"), image("https://example.test/a.png")]),
        100,
    )
    .unwrap();
    let blocks = wire["messages"][0]["content"].as_array().unwrap();
    assert_eq!(blocks[1]["source"], json!({"type":"base64","media_type":"image/png","data":"AAAA"}));
    assert_eq!(blocks[2]["source"], json!({"type":"url","url":"https://example.test/a.png"}));
}

#[test]
fn text_stream_maps_text_ping_and_cache_usage_at_any_chunk_size() {
    for chunk in [1, 7, 64, TEXT.len()] {
        let response = run(TEXT, chunk).unwrap();
        assert_eq!(response.text, "Hello", "chunk {chunk}");
        assert!(response.calls.is_empty());
        let usage = &response.raw["usage"];
        // Total input is uncached + cache creation + cache read.
        assert_eq!(usage["input_tokens"], 25 + 100 + 400);
        assert_eq!(usage["input_tokens_details"]["cached_tokens"], 400);
        // Output tokens come from the cumulative message_delta, not message_start.
        assert_eq!(usage["output_tokens"], 15);
        assert!(usage.get("total_tokens").is_none(), "no invented total");
    }
}

#[test]
fn tool_use_arguments_are_assembled_and_unknown_blocks_are_kept_in_order_unread() {
    for chunk in [1, 13, TOOL_THINKING.len()] {
        let response = run(TOOL_THINKING, chunk).unwrap();
        let output = &response.output;
        assert_eq!(output.len(), 4, "{output:?}");
        // thinking: strings reassembled byte-exact, signature kept as received.
        assert!(opaque::is_opaque(&output[0]));
        assert_eq!(output[0]["dialect"], "anthropic_messages");
        assert_eq!(output[0]["kind"], "thinking");
        assert_eq!(output[0]["block"]["thinking"], "Need a shell: 日本語 \"quoted\"\n");
        assert_eq!(output[0]["block"]["signature"], "EqQBCgIYAhIM");
        // redacted_thinking and an unknown block type are stored untouched.
        assert_eq!(output[1]["block"], json!({"type":"redacted_thinking","data":"ENCRYPTED=="}));
        assert_eq!(output[2]["kind"], "future_block");
        assert_eq!(output[2]["block"], json!({"type":"future_block","payload":{"k":"v"}}));
        assert_eq!(output[3]["type"], "function_call");
        assert_eq!(output[3]["call_id"], "toolu_fixture_1");
        assert_eq!(response.calls.len(), 1);
        assert_eq!(response.calls[0].call_id, "toolu_fixture_1");
        assert_eq!(response.raw["messages"]["unknown_events"], 1);
    }
}

#[test]
fn saved_opaque_blocks_return_unchanged_to_the_same_dialect_and_are_skipped_elsewhere() {
    let response = run(TOOL_THINKING, 31).unwrap();
    // Replay exactly what was adopted, through a save/load round trip.
    let saved: Vec<Value> = serde_json::from_str(&serde_json::to_string(&response.output).unwrap()).unwrap();
    let mut replay = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"base"}]}),
        user("run it"),
    ];
    replay.extend(saved.iter().cloned());
    replay.push(shell_result("toolu_fixture_1"));
    let input = json!({"model":"claude-fixture","input":replay,"tools":[]});
    let wire = dialect::encode_with(Api::AnthropicMessages, input.clone(), 100).unwrap();
    let assistant = wire["messages"][1]["content"].as_array().unwrap();
    // The three opaque blocks come back first, in their original order and unchanged.
    assert_eq!(assistant[0], saved[0]["block"]);
    assert_eq!(assistant[1], saved[1]["block"]);
    assert_eq!(assistant[2], saved[2]["block"]);
    assert_eq!(assistant[3]["type"], "tool_use");
    assert_eq!(assistant[3]["input"], json!({"command":"printf ok"}));
    // Another dialect never sees them.
    for api in [Api::Responses, Api::ChatCompletions] {
        let other = dialect::encode(api, input.clone()).unwrap().to_string();
        assert!(!other.contains("EqQBCgIYAhIM") && !other.contains("ENCRYPTED"), "{api:?}");
    }
}

#[test]
fn stop_reasons_complete_stay_incomplete_or_fail_without_guessing() {
    assert!(run(TEXT, 64).unwrap().raw["status"] == "completed");
    assert!(run(TOOL_THINKING, 64).unwrap().raw["status"] == "completed");
    for reason in ["max_tokens", "model_context_window_exceeded"] {
        let error = run(stop_fixture(reason), 64).unwrap_err();
        assert_eq!(error.kind, "incomplete", "{reason}: {error:?}");
    }
    for reason in ["refusal", "pause_turn", "novel_reason"] {
        let error = run(stop_fixture(reason), 64).unwrap_err();
        assert_eq!(error.kind, "protocol", "{reason}: {error:?}");
        assert!(error.message.contains(reason), "{}", error.message);
    }
}

#[test]
fn a_stream_without_message_stop_is_incomplete_and_errors_keep_their_type() {
    let cut = TEXT.split("event: message_stop").next().unwrap();
    assert_eq!(run(cut, 64).unwrap_err().kind, "incomplete");
    let mut stream = Stream::new(Api::AnthropicMessages);
    let error = stream.push(OVERLOADED.as_bytes()).unwrap_err();
    assert_eq!(error.kind, "provider");
    assert_eq!(error.provider_code.as_deref(), Some("overloaded_error"));
    assert_eq!(error.message, "Overloaded");
}

fn connection(url: String) -> Connection {
    Connection {
        api: Api::AnthropicMessages,
        base_url: url,
        ..Default::default()
    }
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> (String, Value) {
    let mut bytes = Vec::new();
    let (end, length) = loop {
        let mut buffer = [0; 4096];
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
        if let Some(index) = bytes.windows(4).position(|v| v == b"\r\n\r\n") {
            let end = index + 4;
            let head = String::from_utf8_lossy(&bytes[..end]).to_ascii_lowercase();
            let length = head
                .lines()
                .find_map(|l| l.strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                .unwrap_or(0);
            break (end, length);
        }
    };
    while bytes.len() < end + length {
        let mut buffer = [0; 4096];
        let n = stream.read(&mut buffer).await.unwrap();
        assert!(n > 0);
        bytes.extend_from_slice(&buffer[..n]);
    }
    (
        String::from_utf8(bytes[..end].to_vec()).unwrap(),
        serde_json::from_slice(&bytes[end..end + length]).unwrap(),
    )
}

async fn serve_once(status: u16, kind: &str, body: &'static str) -> (String, tokio::task::JoinHandle<(String, Value)>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/v1", listener.local_addr().unwrap());
    let kind = kind.to_owned();
    let task = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let seen = read_request(&mut stream).await;
        let wire = format!(
            "HTTP/1.1 {status} Fixture\r\nContent-Type: {kind}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream.write_all(wire.as_bytes()).await.unwrap();
        seen
    });
    (url, task)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn loopback_sends_version_header_path_and_only_user_extras() {
    crate::test_watchdog::watchdog("tests/anthropic_messages.rs: loopback", async {
        let (url, task) = serve_once(200, "text/event-stream", TEXT).await;
        let mut config = connection(url);
        config.max_tokens = Some(2048);
        config.api_key_env = None;
        config.headers = BTreeMap::from([("x-api-key".into(), "fixture-key".into())]);
        config.extra_body = serde_json::from_value(json!({"cache_control":{"type":"ephemeral"}})).unwrap();
        config.extra_headers = BTreeMap::from([("anthropic-beta".into(), "fixture-beta".into())]);
        let client = HttpModelClient::for_connection("claude", config).unwrap();
        let response = client
            .response_for_thread("thread-1", request(vec![]), CancellationToken::new(), None)
            .await
            .unwrap();
        assert_eq!(response.text, "Hello");
        let (head, body) = task.await.unwrap();
        let lower = head.to_ascii_lowercase();
        assert!(head.starts_with("POST /v1/messages HTTP/1.1"), "{head}");
        assert!(lower.contains(&format!("anthropic-version: {ANTHROPIC_VERSION}")));
        assert!(lower.contains("x-api-key: fixture-key"));
        assert!(lower.contains("anthropic-beta: fixture-beta"));
        assert_eq!(body["max_tokens"], 2048);
        assert_eq!(body["cache_control"], json!({"type":"ephemeral"}));
        assert!(body.get("thinking").is_none() && body.get("temperature").is_none());
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn without_extra_body_no_optional_field_reaches_the_wire() {
    crate::test_watchdog::watchdog("tests/anthropic_messages.rs: baseline", async {
        let (url, task) = serve_once(200, "text/event-stream", TEXT).await;
        let client = HttpModelClient::for_connection("claude", connection(url)).unwrap();
        client
            .response_for_thread("thread-1", request(vec![]), CancellationToken::new(), None)
            .await
            .unwrap();
        let (_, body) = task.await.unwrap();
        let mut keys: Vec<&str> = body.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, ["max_tokens", "messages", "model", "stream", "system", "tools"]);
        assert_eq!(body["max_tokens"], 32768);
    })
    .await
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn http_errors_keep_their_provider_type_for_retry_decisions() {
    crate::test_watchdog::watchdog("tests/anthropic_messages.rs: http errors", async {
        for (status, kind) in [
            (529, "overloaded_error"),
            (429, "rate_limit_error"),
            (413, "request_too_large"),
            (400, "invalid_request_error"),
        ] {
            let body: &'static str = Box::leak(
                json!({"type":"error","error":{"type":kind,"message":"fixture"},"request_id":"req_x"})
                    .to_string()
                    .into_boxed_str(),
            );
            let (url, task) = serve_once(status, "application/json", body).await;
            let mut config = connection(url);
            config.response_max_retries = 0;
            let client = HttpModelClient::for_connection("claude", config).unwrap();
            let failure = client
                .response_for_thread("thread-1", request(vec![]), CancellationToken::new(), None)
                .await
                .unwrap_err();
            task.await.unwrap();
            assert_eq!(failure.facts.status, Some(status), "{kind}");
            assert_eq!(failure.error.status, Some(status));
            assert_eq!(failure.error.message, "fixture");
        }
    })
    .await
    .unwrap();
}

#[test]
fn configuration_validates_max_tokens_and_the_messages_api() {
    let ok = |extra: &str| {
        Config::parse(&format!("[connections.c]\nbase_url='https://api.example/v1'\napi='anthropic_messages'\n{extra}"))
    };
    let c = ok("").unwrap();
    assert_eq!(c.connections["c"].api, Api::AnthropicMessages);
    assert_eq!(c.connections["c"].max_tokens, None);
    assert_eq!(ok("max_tokens=4096").unwrap().connections["c"].max_tokens, Some(4096));
    assert!(ok("max_tokens=0").is_err());
    assert!(ok("max_tokens=-1").is_err());
    assert!(ok("prompt_cache_key=true").is_err());
    // max_tokens belongs to this dialect only.
    for api in ["responses", "chat_completions"] {
        let source = format!("[connections.c]\nbase_url='https://api.example/v1'\napi='{api}'\nmax_tokens=10");
        assert!(Config::parse(&source).is_err(), "{api}");
    }
    // Reserved fields cannot be set through extra_body; anything else can.
    for key in ["model", "max_tokens", "stream", "messages", "system", "tools"] {
        assert!(ok(&format!("extra_body={{ {key} = 1 }}")).is_err(), "{key}");
    }
    ok("extra_body={ thinking = { type = 'enabled', budget_tokens = 1024 }, cache_control = { type = 'ephemeral' } }").unwrap();
    // The endpoint is /messages.
    use shellboy::config::ApiEndpoint;
    assert_eq!(c.connections["c"].endpoint(ApiEndpoint::Responses).unwrap().path(), "/v1/messages");
}
