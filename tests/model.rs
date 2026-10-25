use serde_json::{Value, json};
use shellboy::model::{ModelError, ModelResponse, ResponseStream, SseDecoder, response_request};

fn event(value: &Value) -> String {
    format!("data: {value}\r\n\r\n")
}
fn completed(output: Vec<Value>) -> Value {
    json!({"id":"resp-fixture","status":"completed","output":output})
}
fn call(id: &str, args: &str) -> Value {
    json!({"type":"function_call","id":format!("item-{id}"),"call_id":id,"name":"shell","arguments":args,"status":"completed"})
}

#[test]
fn byte_fragmented_utf8_crlf_and_final_opaque_reasoning_are_preserved() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/responses/reasoning.json")).unwrap();
    let mut stream = ResponseStream::default();
    let mut bytes = event(&json!({"type":"response.output_text.delta","delta":"日本語😀"}));
    bytes.push_str(&event(
        &json!({"type":"response.output_item.done","output_index":0,"item":response["output"][0]}),
    ));
    bytes.push_str(&event(
        &json!({"type":"response.completed","response":response}),
    ));
    bytes.push_str("data: [DONE]\r\n\r\n");
    for byte in bytes.bytes() {
        stream.push(&[byte]).unwrap();
    }
    assert_eq!(stream.partial_text, "日本語😀");
    let result = stream.finish(true).unwrap();
    assert_eq!(result.text, "日本語😀");
    assert_eq!(
        result.output[0]["encrypted_content"],
        "opaque-final-fixture"
    );
    assert_eq!(result.output[1]["phase"], "commentary");
    assert_eq!(result.raw, response);
}

#[test]
fn no_tool_group_until_completed_and_all_calls_valid() {
    let good = call("a", r#"{"command":"printf ok"}"#);
    let mut stream = ResponseStream::default();
    stream
        .push(
            event(&json!({"type":"response.output_item.done","output_index":0,"item":good}))
                .as_bytes(),
        )
        .unwrap();
    assert!(!stream.is_terminal());
    assert_eq!(stream.finish(true).unwrap_err().kind, "incomplete");
    let good_pair = completed(vec![
        good.clone(),
        call("b", r#"{"command":"pwd","background":true}"#),
    ]);
    let result = ModelResponse::completed(good_pair, true).unwrap();
    assert_eq!(result.calls.len(), 2);
    assert!(!result.calls[0].args.as_ref().unwrap().background);
    assert!(result.calls[1].args.as_ref().unwrap().background);
    assert!(ModelResponse::completed(completed(vec![good.clone(), good.clone()]), true).is_err());
    assert!(
        ModelResponse::completed(
            completed(vec![good.clone(), call("b", "broken-json")]),
            true
        )
        .unwrap()
        .calls[1]
            .args
            .is_err()
    );
    assert!(ModelResponse::completed(completed(vec![good]), false).is_err());
}

#[test]
fn completed_commentary_empty_and_refusal_are_normal_answers() {
    for output in [
        vec![],
        vec![json!({"type":"message","role":"assistant","phase":"commentary","content":[]})],
        vec![
            json!({"type":"message","role":"assistant","content":[{"type":"refusal","refusal":"Cannot comply"}]}),
        ],
    ] {
        let result = ModelResponse::completed(completed(output), true).unwrap();
        assert!(result.calls.is_empty());
    }
}

#[test]
fn incomplete_status_unknown_items_and_inconsistent_done_are_errors() {
    let mut response = completed(vec![]);
    response["status"] = json!("incomplete");
    assert_eq!(
        ModelResponse::completed(response, true).unwrap_err().kind,
        "incomplete"
    );
    assert_eq!(
        ModelResponse::completed(
            completed(vec![json!({"type":"new_provider_thing","value":42})]),
            true
        )
        .unwrap_err()
        .kind,
        "unsupported_item"
    );
    let mut stream = ResponseStream::default();
    stream.push(event(&json!({"type":"response.output_item.done","output_index":0,"item":{"type":"reasoning","encrypted_content":"final"}})).as_bytes()).unwrap();
    assert!(
        stream
            .push(
                event(&json!({"type":"response.completed","response":completed(vec![])}))
                    .as_bytes()
            )
            .is_err()
    );
}

#[test]
fn unknown_http_error_is_not_context_overflow() {
    for (status, code, yes) in [
        (413, "request_too_large", false),
        (400, "invalid_request_error", false),
        (400, "context_length_exceeded", true),
    ] {
        let error = ModelError::provider(
            Some(status),
            &json!({"error":{"code":code,"message":"fixture"}}),
        );
        assert_eq!(error.is_context_limit(), yes);
    }
    let mut stream = ResponseStream::default();
    stream.push(event(&json!({"type":"response.failed","response":{"error":{"code":"context_length_exceeded","message":"context limit"}}})).as_bytes()).unwrap();
    assert!(stream.finish(true).unwrap_err().is_context_limit());
}

#[test]
fn request_is_stateless_and_effort_omission_does_not_invent_a_value() {
    let response: Value =
        serde_json::from_str(include_str!("fixtures/responses/reasoning.json")).unwrap();
    let input = response["output"].as_array().unwrap().clone();
    let request = response_request("proxy-model", input.clone(), None, true);
    assert_eq!(request["store"], false);
    assert_eq!(request["input"], json!(input));
    assert!(request.get("reasoning").is_none());
    assert!(request.get("previous_response_id").is_none());
    assert_eq!(request["tools"].as_array().unwrap().len(), 1);
    let request = response_request(
        "another-model",
        vec![],
        Some("arbitrary-provider-value"),
        false,
    );
    assert_eq!(request["reasoning"]["effort"], "arbitrary-provider-value");
    assert_eq!(request["tools"], json!([]));
}

#[test]
fn multiline_data_comments_and_bad_eof_are_handled() {
    let mut decoder = SseDecoder::default();
    assert_eq!(
        decoder
            .push(b": comment\r\nevent: ignored\r\ndata: {\r\ndata: \"ok\":true}\r\n\r\n")
            .unwrap(),
        vec![json!({"ok":true})]
    );
    let mut stream = ResponseStream::default();
    stream.push(b"data: {\"type\":").unwrap();
    assert_eq!(stream.finish(true).unwrap_err().kind, "incomplete");
    let mut decoder = SseDecoder::default();
    assert!(decoder.push(b"data: {bad}\n\n").is_err());
}

#[test]
fn terminal_error_is_not_masked_by_trailing_frames_at_any_byte_split() {
    let primary = json!({"type":"error","code":"credit_balance_exhausted","message":"Synthetic quota failure"});
    let mut body = event(&primary);
    body.push_str(&event(&json!({"type":"response.failed","response":{"status":"failed","error":{"code":"different_error","message":"secondary"}}})));
    body.push_str("data: {invalid trailing JSON\r\n\r\ndata: {partial");
    for split in 0..=body.len() {
        let mut stream = ResponseStream::default();
        let mut seen = stream.push(&body.as_bytes()[..split]).unwrap();
        seen.extend(stream.push(&body.as_bytes()[split..]).unwrap());
        assert_eq!(seen, vec![primary.clone()], "split {split}");
        let error = stream.finish(false).unwrap_err();
        assert_eq!(
            error.provider_code.as_deref(),
            Some("credit_balance_exhausted")
        );
        assert_eq!(error.message, "Synthetic quota failure");
    }
}

#[test]
fn completed_terminal_boundary_ignores_only_bytes_after_the_complete_frame() {
    let response = completed(vec![
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"OK"}]}),
    ]);
    let mut body = event(&json!({"type":"response.completed","response":response}));
    body.push_str("data: {truncated");
    for split in 0..=body.len() {
        let mut stream = ResponseStream::default();
        stream.push(&body.as_bytes()[..split]).unwrap();
        stream.push(&body.as_bytes()[split..]).unwrap();
        assert_eq!(stream.finish(false).unwrap().text, "OK");
    }
    let mut stream = ResponseStream::default();
    stream
        .push(b"data: {\"type\":\"error\",\"message\":\"unfinished\"}")
        .unwrap();
    assert_eq!(stream.finish(false).unwrap_err().kind, "incomplete");
}
