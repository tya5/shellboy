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
fn run(
    body: &str,
    chunk: usize,
) -> Result<shellboy::model::ModelResponse, shellboy::model::ModelError> {
    let mut stream = Stream::new(Api::AnthropicMessages);
    for piece in body.as_bytes().chunks(chunk) {
        stream.push(piece)?;
    }
    stream.finish(true)
}

#[test]
fn request_has_only_what_the_conversation_needs() {
    let wire = dialect::encode_with(Api::AnthropicMessages, request(vec![]), 32768).unwrap();
    let mut keys: Vec<&str> = wire
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort();
    assert_eq!(
        keys,
        [
            "max_tokens",
            "messages",
            "model",
            "output_config",
            "stream",
            "system",
            "tools"
        ]
    );
    assert_eq!(wire["output_config"], json!({"effort":"low"}));
    assert_eq!(wire["max_tokens"], 32768);
    assert_eq!(wire["stream"], true);
    assert_eq!(wire["system"], "You are Shellboy.");
    // Nothing optional or provider-specific is added on the user's behalf, and
    // the Shellboy effort setting is sent only as output_config.effort.
    for absent in [
        "thinking",
        "temperature",
        "top_p",
        "top_k",
        "tool_choice",
        "cache_control",
        "metadata",
        "stop_sequences",
        "reasoning",
        "reasoning_effort",
        "store",
        "include",
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
fn effort_is_sent_only_when_set_and_uses_one_mapping() {
    let none = response_request("claude-fixture", vec![user("hi")], None, false);
    let wire = dialect::encode_with(Api::AnthropicMessages, none, 32768).unwrap();
    assert!(wire.get("output_config").is_none());
    for (given, sent) in [
        ("minimal", "low"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("xhigh", "xhigh"),
        ("max", "max"),
    ] {
        let req = response_request("claude-fixture", vec![user("hi")], Some(given), false);
        let wire = dialect::encode_with(Api::AnthropicMessages, req, 32768).unwrap();
        assert_eq!(wire["output_config"], json!({"effort":sent}), "{given}");
        assert!(wire.get("thinking").is_none() && wire.get("reasoning").is_none());
    }
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
    assert!(
        results[2]["text"]
            .as_str()
            .unwrap()
            .starts_with("Shellboy event v1:")
    );
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
    let error =
        dialect::encode_with(Api::AnthropicMessages, request(vec![assistant]), 100).unwrap_err();
    assert_eq!(error.kind, "input_invalid");
}

#[test]
fn images_map_to_base64_and_url_sources() {
    let image =
        |url: &str| json!({"role":"user","content":[{"type":"input_image","image_url":url}]});
    let wire = dialect::encode_with(
        Api::AnthropicMessages,
        request(vec![
            image("data:image/png;base64,AAAA"),
            image("https://example.test/a.png"),
        ]),
        100,
    )
    .unwrap();
    let blocks = wire["messages"][0]["content"].as_array().unwrap();
    assert_eq!(
        blocks[1]["source"],
        json!({"type":"base64","media_type":"image/png","data":"AAAA"})
    );
    assert_eq!(
        blocks[2]["source"],
        json!({"type":"url","url":"https://example.test/a.png"})
    );
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
        // Cache writes are kept as their own saved counter (#127).
        assert_eq!(usage["input_tokens_details"]["cache_creation_tokens"], 100);
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
        assert_eq!(
            output[0]["block"]["thinking"],
            "Need a shell: 日本語 \"quoted\"\n"
        );
        assert_eq!(output[0]["block"]["signature"], "EqQBCgIYAhIM");
        // redacted_thinking and an unknown block type are stored untouched.
        assert_eq!(
            output[1]["block"],
            json!({"type":"redacted_thinking","data":"ENCRYPTED=="})
        );
        assert_eq!(output[2]["kind"], "future_block");
        assert_eq!(
            output[2]["block"],
            json!({"type":"future_block","payload":{"k":"v"}})
        );
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
    let saved: Vec<Value> =
        serde_json::from_str(&serde_json::to_string(&response.output).unwrap()).unwrap();
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
        assert!(
            !other.contains("EqQBCgIYAhIM") && !other.contains("ENCRYPTED"),
            "{api:?}"
        );
    }
}

#[test]
fn stop_reasons_complete_stay_incomplete_or_fail_without_guessing() {
    assert!(run(TEXT, 64).unwrap().raw["status"] == "completed");
    assert!(run(TOOL_THINKING, 64).unwrap().raw["status"] == "completed");
    for (reason, expected) in [
        ("max_tokens", "output_limit"),
        ("model_context_window_exceeded", "incomplete"),
    ] {
        let error = run(stop_fixture(reason), 64).unwrap_err();
        assert_eq!(error.kind, expected, "{reason}: {error:?}");
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
    // An in-stream error event is a terminal provider failure that keeps the
    // provider's error type; the stream reports it when it finishes.
    let error = run(OVERLOADED, 64).unwrap_err();
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
                .find_map(|l| {
                    l.strip_prefix("content-length:")
                        .map(|v| v.trim().parse::<usize>().unwrap())
                })
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

async fn serve_once(
    status: u16,
    kind: &str,
    body: &'static str,
) -> (String, tokio::task::JoinHandle<(String, Value)>) {
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
        config.extra_body =
            serde_json::from_value(json!({"metadata":{"user_id":"fixture"}})).unwrap();
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
        assert_eq!(body["metadata"], json!({"user_id":"fixture"}));
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
        // The Thread effort reaches the wire as output_config.effort.
        assert_eq!(body["output_config"], json!({"effort":"low"}));
        let mut keys: Vec<&str> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        keys.sort();
        assert_eq!(
            keys,
            [
                "max_tokens",
                "messages",
                "model",
                "output_config",
                "stream",
                "system",
                "tools"
            ]
        );
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
        Config::parse(&format!(
            "[connections.c]\nbase_url='https://api.example/v1'\napi='anthropic_messages'\n{extra}"
        ))
    };
    let c = ok("").unwrap();
    assert_eq!(c.connections["c"].api, Api::AnthropicMessages);
    assert_eq!(c.connections["c"].max_tokens, None);
    assert_eq!(
        ok("max_tokens=4096").unwrap().connections["c"].max_tokens,
        Some(4096)
    );
    assert!(ok("max_tokens=0").is_err());
    assert!(ok("max_tokens=-1").is_err());
    assert!(ok("prompt_cache_key=true").is_err());
    // The same optional setting maps to each API's native output-limit field.
    for api in ["responses", "chat_completions"] {
        let source = format!(
            "[connections.c]\nbase_url='https://api.example/v1'\napi='{api}'\nmax_tokens=10"
        );
        assert_eq!(
            Config::parse(&source).unwrap().connections["c"].max_tokens,
            Some(10),
            "{api}"
        );
    }
    // Reserved fields cannot be set through extra_body; anything else can.
    for key in [
        "model",
        "max_tokens",
        "stream",
        "messages",
        "system",
        "tools",
        "output_config",
        "cache_control",
    ] {
        assert!(ok(&format!("extra_body={{ {key} = 1 }}")).is_err(), "{key}");
    }
    ok("extra_body={ thinking = { type = 'enabled', budget_tokens = 1024 } }").unwrap();
    // The endpoint is /messages.
    use shellboy::config::ApiEndpoint;
    assert_eq!(
        c.connections["c"]
            .endpoint(ApiEndpoint::Responses)
            .unwrap()
            .path(),
        "/v1/messages"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn environment_key_custom_headers_are_sent_raw_and_redacted() {
    const CHILD: &str = "SHELLBOY_TEST_API_KEY_HEADER_CHILD";
    const KEY: &str = "SHELLBOY_TEST_API_KEY_HEADER_KEY";
    const SECRET: &str = "fixture-env-key-120";
    // A child-only environment avoids mutating process-global state while other
    // integration tests run concurrently. It never reads a user's API key.
    if std::env::var(CHILD).as_deref() != Ok("1") {
        // Capture the child's libtest output: inherited, its nested summary line
        // lands in the parent harness output and corrupts the suite's counts.
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "anthropic_messages::environment_key_custom_headers_are_sent_raw_and_redacted",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .env(KEY, SECRET)
            .stdin(std::process::Stdio::null())
            .output()
            .unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed;"),
            "isolated credential fixture failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        return;
    }
    crate::test_watchdog::watchdog("tests/anthropic_messages.rs: environment header", async {
        const RESPONSES: &str = "data: {\"type\":\"response.completed\",\"response\":{\"status\":\"completed\",\"output\":[{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"ok\"}]}]}}\n\n";
        const ERROR: &str = r#"{"type":"error","error":{"type":"invalid_request_error","message":"echo fixture-env-key-120"}}"#;
        for (api, header, status, kind, body) in [
            (Api::AnthropicMessages, "x-api-key", 200, "text/event-stream", TEXT),
            (Api::Responses, "api-key", 200, "text/event-stream", RESPONSES),
            (Api::AnthropicMessages, "X-Owner", 400, "application/json", ERROR),
        ] {
            let (url, server) = serve_once(status, kind, body).await;
            let mut config = connection(url);
            config.api = api;
            config.api_key_env = Some(KEY.into());
            config.api_key_header = Some(header.into());
            config.response_max_retries = 0;
            let client = HttpModelClient::for_connection("fixture", config).unwrap();
            let result = client.response_for_thread("fixture-thread", request(vec![]), CancellationToken::new(), None).await;
            let (head, _) = server.await.unwrap();
            let head = head.to_ascii_lowercase();
            assert!(head.contains(&format!("{}: {SECRET}", header.to_ascii_lowercase())));
            assert!(!head.contains("authorization:"));
            assert!(!head.contains(&format!("bearer {SECRET}")));
            if status == 200 {
                assert!(result.is_ok());
            } else {
                let failure = result.unwrap_err();
                assert!(!failure.error.message.contains(SECRET));
                assert!(failure.error.message.contains("[redacted]"));
            }
        }
    }).await.unwrap();
}

// --- Common cache hint (#127) ---

fn developer(text: &str) -> Value {
    json!({"role":"developer","content":[{"type":"input_text","text":text}]})
}

/// The shape the Driver sends: system, working instructions, the saved
/// conversation, then the per-request `Shellboy context v1` line.
fn driver_request(conversation: Vec<Value>, context: &str) -> Value {
    let mut input = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"You are Shellboy."}]}),
        developer("Working instructions from \"AGENTS.md\": be brief"),
    ];
    input.extend(conversation);
    input.push(developer(&format!("Shellboy context v1: turn={context}")));
    response_request("claude-fixture", input, None, true)
}

fn hinted(request: Value) -> Value {
    let hint = dialect::CacheHint::of(&request);
    dialect::encode_hinted(Api::AnthropicMessages, request, 100, Some(hint)).unwrap()
}

/// Every `cache_control` in the body, by JSON pointer.
fn markers(value: &Value, path: String, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                if key == "cache_control" {
                    assert_eq!(child, &json!({"type":"ephemeral"}), "5-minute default only");
                    out.push(path.clone());
                } else {
                    markers(child, format!("{path}/{key}"), out);
                }
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                markers(child, format!("{path}/{index}"), out);
            }
        }
        _ => {}
    }
}
fn marker_paths(wire: &Value) -> Vec<String> {
    let mut out = Vec::new();
    markers(wire, String::new(), &mut out);
    out
}

/// Message blocks in wire order with their role, markers removed, up to and
/// including the first marked one when `until_marker`.
fn blocks(wire: &Value, until_marker: bool) -> Vec<Value> {
    let mut out = Vec::new();
    for message in wire["messages"].as_array().unwrap() {
        for block in message["content"].as_array().unwrap() {
            let mut plain = block.clone();
            let marked = plain
                .as_object_mut()
                .unwrap()
                .remove("cache_control")
                .is_some();
            out.push(json!([message["role"], plain]));
            if until_marker && marked {
                return out;
            }
        }
    }
    assert!(!until_marker, "no conversation breakpoint in {wire}");
    out
}

fn thinking() -> Value {
    opaque::wrap(
        "anthropic_messages",
        "thinking",
        json!({"type":"thinking","thinking":"plan","signature":"sig"}),
    )
}

#[test]
fn cache_breakpoints_mark_tools_system_and_the_tail_before_per_request_context() {
    let wire = hinted(driver_request(
        vec![
            user("run it"),
            thinking(),
            shell_call("toolu_a"),
            shell_result("toolu_a"),
            // A saved event notice at the very end cannot be told apart from
            // per-request context, so it stays after the breakpoint.
            developer("Shellboy event v1: {}"),
        ],
        "t1",
    ));
    let paths = marker_paths(&wire);
    assert!(paths.len() <= 4, "{paths:?}");
    assert_eq!(
        paths,
        [
            "/messages/2/content/0".to_owned(),
            "/system/0".to_owned(),
            "/tools/0".to_owned()
        ],
        "{wire}"
    );
    // The leading system/developer items are one marked system block.
    assert_eq!(wire["system"][0]["type"], "text");
    assert!(
        wire["system"][0]["text"]
            .as_str()
            .unwrap()
            .ends_with("be brief")
    );
    // The tail is the tool result; the trailing context after it is unmarked.
    let last = wire["messages"][2]["content"].as_array().unwrap();
    assert_eq!(last.len(), 3);
    assert_eq!(last[0]["type"], "tool_result");
    assert!(
        last[1]["text"]
            .as_str()
            .unwrap()
            .starts_with("Shellboy event")
    );
    assert!(
        last[2]["text"]
            .as_str()
            .unwrap()
            .starts_with("Shellboy context")
    );
    // A thinking block is never a breakpoint.
    assert!(
        wire["messages"][1]["content"][0]
            .get("cache_control")
            .is_none()
    );
    // No top-level automatic marker.
    assert!(wire.get("cache_control").is_none());
}

#[test]
fn the_cached_conversation_prefix_recurs_in_the_next_request() {
    let first_turn = vec![user("run it")];
    let mut second = first_turn.clone();
    second.extend([shell_call("toolu_a"), shell_result("toolu_a")]);
    let mut third = second.clone();
    third.extend([
        json!({"role":"assistant","content":[{"type":"output_text","text":"done"}]}),
        user("again"),
    ]);
    let requests = [
        hinted(driver_request(first_turn, "t1")),
        hinted(driver_request(second, "t1")),
        hinted(driver_request(third, "t2")),
    ];
    for pair in requests.windows(2) {
        let (earlier, later) = (&pair[0], &pair[1]);
        assert_eq!(earlier["system"], later["system"]);
        assert_eq!(earlier["tools"], later["tools"]);
        // The block sequence written to the cache by one request is a prefix
        // of the next, so its tail breakpoint finds it within the lookback.
        let cached = blocks(earlier, true);
        let next = blocks(later, false);
        assert_eq!(&next[..cached.len()], &cached[..], "{earlier}\n{later}");
        assert!(next.len() - cached.len() < 20);
    }
}

#[test]
fn without_conversation_or_tools_only_present_parts_are_marked() {
    let input = vec![
        json!({"role":"system","content":[{"type":"input_text","text":"base"}]}),
        user("hi"),
        developer("Shellboy context v1: only"),
    ];
    let request = response_request("claude-fixture", input, None, false);
    assert_eq!(
        marker_paths(&hinted(request)),
        ["/messages/0/content/0".to_owned(), "/system/0".to_owned()]
    );
}

#[test]
fn other_dialects_send_nothing_new_for_the_hint() {
    let request = driver_request(vec![user("run it")], "t1");
    let hint = dialect::CacheHint::of(&request);
    assert_eq!(
        hint,
        dialect::CacheHint {
            prefix_items: 2,
            conversation_items: 3
        }
    );
    for api in [Api::Responses, Api::ChatCompletions] {
        let plain = dialect::encode(api, request.clone()).unwrap();
        let with_hint = dialect::encode_hinted(api, request.clone(), 100, Some(hint)).unwrap();
        assert_eq!(plain, with_hint, "{api:?}");
        assert!(!with_hint.to_string().contains("cache_control"));
        assert!(with_hint.get("prompt_cache_key").is_none());
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn caching_is_on_by_default_and_opt_out_sends_no_marker() {
    crate::test_watchdog::watchdog("tests/anthropic_messages.rs: caching", async {
        for (prompt_caching, role) in [
            (true, shellboy::config::ContextRole::User),
            (true, shellboy::config::ContextRole::Developer),
            (false, shellboy::config::ContextRole::Developer),
        ] {
            let (url, task) = serve_once(200, "text/event-stream", TEXT).await;
            let mut config = connection(url);
            config.prompt_caching = prompt_caching;
            config.context_role = role;
            let client = HttpModelClient::for_connection("claude", config).unwrap();
            client
                .response_for_thread(
                    "thread-1",
                    driver_request(vec![user("run it")], "t1"),
                    CancellationToken::new(),
                    None,
                )
                .await
                .unwrap();
            let (_, body) = task.await.unwrap();
            if prompt_caching {
                // context_role = user sends the instructions as the first user
                // block, but must not move the tail onto the context line.
                let tail = match role {
                    shellboy::config::ContextRole::User => "/messages/0/content/1",
                    shellboy::config::ContextRole::Developer => "/messages/0/content/0",
                };
                assert_eq!(
                    marker_paths(&body),
                    [
                        tail.to_owned(),
                        "/system/0".to_owned(),
                        "/tools/0".to_owned()
                    ],
                    "{role:?}: {body}"
                );
            } else {
                assert!(!body.to_string().contains("cache_control"), "{body}");
                assert!(body["system"].is_string());
            }
        }
    })
    .await
    .unwrap();
}

#[test]
fn prompt_caching_configuration() {
    let parse = |api: &str, extra: &str| {
        Config::parse(&format!(
            "[connections.c]\nbase_url='https://api.example/v1'\napi='{api}'\n{extra}"
        ))
    };
    for api in ["responses", "chat_completions", "anthropic_messages"] {
        assert!(parse(api, "").unwrap().connections["c"].prompt_caching);
        assert!(!parse(api, "prompt_caching=false").unwrap().connections["c"].prompt_caching);
    }
    parse("responses", "prompt_cache_key=true").unwrap();
    // A routing key without caching is contradictory.
    assert!(parse("responses", "prompt_caching=false\nprompt_cache_key=true").is_err());
}
