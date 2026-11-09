use super::*;
use crate::config::Api;
use serde_json::json;

fn sse(event: Value) -> Vec<u8> {
    format!("data: {event}\n\n").into_bytes()
}

#[test]
fn exact_output_limit_reasons_are_typed_and_keep_partial_text_for_each_dialect() {
    let mut responses = dialect::Stream::new(Api::Responses);
    let delta = responses
        .push(&sse(
            json!({"type":"response.output_text.delta","delta":"partial-r"}),
        ))
        .unwrap();
    assert_eq!(delta[0]["delta"], "partial-r");
    responses
        .push(&sse(json!({
            "type":"response.incomplete",
            "response":{"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},
                "output":[{"type":"message","role":"assistant","status":"incomplete",
                    "content":[{"type":"output_text","text":"partial-r"}]}]}
        })))
        .unwrap();
    assert_eq!(responses.finish(false).unwrap_err().kind, "output_limit");

    let nonstream: Result<ModelResponse, _> = ModelResponse::completed(
        json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","role":"assistant","status":"incomplete","content":[{"type":"output_text","text":"partial-json"}]}]}),
        false,
    );
    assert_eq!(nonstream.unwrap_err().kind, "output_limit");
    let chat_json = dialect::decode_json(
        Api::ChatCompletions,
        json!({"id":"chat-json","choices":[{"index":0,"message":{"role":"assistant","content":"partial-json"},"finish_reason":"length"}]}),
    ).unwrap();
    assert_eq!(chat_json["status"], "incomplete");
    assert_eq!(chat_json["output"][0]["content"][0]["text"], "partial-json");
    assert_eq!(
        ModelResponse::completed(chat_json, false).unwrap_err().kind,
        "output_limit"
    );

    let mut retained = ResponseStream::default();
    retained
        .push(&sse(
            json!({"type":"response.output_text.delta","delta":"partial-raw"}),
        ))
        .unwrap();
    let raw_response = json!({"status":"incomplete","incomplete_details":{"reason":"max_output_tokens"},"output":[{"type":"message","role":"assistant","status":"incomplete","content":[{"type":"output_text","text":"partial-raw"}]}]});
    retained
        .push(&sse(
            json!({"type":"response.incomplete","response":raw_response}),
        ))
        .unwrap();
    assert_eq!(retained.partial_text, "partial-raw");
    assert_eq!(retained.failed_response(), Some(&raw_response));
    let retained_error = retained.finish(false).unwrap_err();
    assert_eq!(retained_error.kind, "output_limit");
    let facts = http::ResponseFacts {
        received: http::ResponseReceived::Output,
        terminal_received: true,
        ..Default::default()
    };
    assert_eq!(
        retry_class::classify_retry(&retained_error, &facts, true),
        retry_class::RetryPolicy::Ineligible,
        "provider terminal output-limit signal must never be retried"
    );

    let mut failed_not_incomplete = ResponseStream::default();
    failed_not_incomplete
        .push(&sse(json!({"type":"response.failed","response":{"status":"failed","incomplete_details":{"reason":"max_output_tokens"}}})))
        .unwrap();
    assert_eq!(
        failed_not_incomplete.finish(false).unwrap_err().kind,
        "incomplete",
        "failed terminal with coincidental reason is not the incomplete contract"
    );

    let mut chat = dialect::Stream::new(Api::ChatCompletions);
    let delta = chat
        .push(&sse(json!({"id":"chat-1","choices":[{"index":0,"delta":{"content":"partial-c"},"finish_reason":null}]})))
        .unwrap();
    assert_eq!(delta[0]["delta"], "partial-c");
    chat.push(&sse(
        json!({"id":"chat-1","choices":[{"index":0,"delta":{},"finish_reason":"length"}]}),
    ))
    .unwrap();
    chat.push(b"data: [DONE]\n\n").unwrap();
    assert_eq!(chat.finish(false).unwrap_err().kind, "output_limit");

    let mut messages = dialect::Stream::new(Api::AnthropicMessages);
    messages
        .push(&sse(json!({"type":"message_start","message":{"id":"msg-1","model":"m","usage":{"input_tokens":1}}})))
        .unwrap();
    messages
        .push(&sse(json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}})))
        .unwrap();
    let delta = messages
        .push(&sse(json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"partial-a"}})))
        .unwrap();
    assert_eq!(delta[0]["delta"], "partial-a");
    messages
        .push(&sse(
            json!({"type":"message_delta","delta":{"stop_reason":"max_tokens"}}),
        ))
        .unwrap();
    messages.push(&sse(json!({"type":"message_stop"}))).unwrap();
    assert_eq!(messages.finish(false).unwrap_err().kind, "output_limit");
}

#[test]
fn unrelated_incomplete_reasons_and_transport_eof_are_not_output_limit() {
    let responses_content_filter =
        json!({"status":"incomplete","incomplete_details":{"reason":"content_filter"},"output":[]});
    assert_eq!(
        ModelResponse::completed(responses_content_filter, false)
            .unwrap_err()
            .kind,
        "incomplete"
    );
    let responses_other =
        json!({"status":"incomplete","incomplete_details":{"reason":"other"},"output":[]});
    assert_eq!(
        ModelResponse::completed(responses_other, false)
            .unwrap_err()
            .kind,
        "incomplete"
    );
    let responses_missing_reason = json!({"status":"incomplete","output":[]});
    assert_eq!(
        ModelResponse::completed(responses_missing_reason, false)
            .unwrap_err()
            .kind,
        "incomplete"
    );

    let mut chat_filter = dialect::Stream::new(Api::ChatCompletions);
    chat_filter
        .push(&sse(json!({"id":"chat-filter","choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]})))
        .unwrap();
    assert_eq!(chat_filter.finish(false).unwrap_err().kind, "incomplete");

    let mut messages_context = dialect::Stream::new(Api::AnthropicMessages);
    messages_context
        .push(&sse(
            json!({"type":"message_start","message":{"id":"msg-context","model":"m"}}),
        ))
        .unwrap();
    messages_context
        .push(&sse(
            json!({"type":"message_delta","delta":{"stop_reason":"model_context_window_exceeded"}}),
        ))
        .unwrap();
    messages_context
        .push(&sse(json!({"type":"message_stop"})))
        .unwrap();
    assert_eq!(
        messages_context.finish(false).unwrap_err().kind,
        "incomplete"
    );

    let mut interrupted = dialect::Stream::new(Api::Responses);
    interrupted
        .push(&sse(
            json!({"type":"response.output_text.delta","delta":"partial"}),
        ))
        .unwrap();
    assert_eq!(interrupted.finish(false).unwrap_err().kind, "incomplete");
}
