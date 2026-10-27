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
fn incomplete_and_unknown_final_responses_fail_but_item_differences_are_observations() {
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
    stream
        .push(event(&json!({"type":"response.completed","response":completed(vec![])})).as_bytes())
        .unwrap();
    let response = stream.finish(true).unwrap();
    assert!(response.output.is_empty());
    let observation = response.stream_observation.unwrap();
    assert_eq!(observation.total_differences, 1);
    assert_eq!(
        observation.completed_items[&0]["encrypted_content"],
        "final"
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

#[test]
fn proxy_metadata_variation_uses_final_json_and_keeps_both_received_sources() {
    let done = json!({"type":"message","id":"stream-id","status":"in_progress","role":"assistant","content":[{"type":"output_text","text":"受信済み","annotations":null,"logprobs":[]}],"provider_extension":{"stage":"item-done"}});
    let final_item = json!({"type":"message","id":"final-id","status":"completed","phase":"final","role":"assistant","content":[{"type":"output_text","text":"受信済み","annotations":[],"logprobs":null}],"provider_extension":{"stage":"final"}});
    let raw = completed(vec![final_item.clone()]);
    let wire = event(&json!({"type":"response.output_text.delta","delta":"受信済み"}))
        + &event(&json!({"type":"response.output_item.done","output_index":0,"item":done}))
        + &event(&json!({"type":"response.completed","response":raw}));
    for split in 0..=wire.len() {
        let mut stream = ResponseStream::default();
        stream.push(&wire.as_bytes()[..split]).unwrap();
        stream.push(&wire.as_bytes()[split..]).unwrap();
        let response = stream.finish(true).unwrap();
        assert_eq!(response.raw, raw);
        assert_eq!(response.output, vec![final_item.clone()]);
        assert_eq!(response.text, "受信済み");
        let observation = response.stream_observation.unwrap();
        assert_eq!(observation.completed_items[&0], done);
        assert_eq!(observation.partial_text, "受信済み");
        assert_eq!(observation.total_differences, 1);
        assert_eq!(
            observation.differences[0].field,
            shellboy::model::OutputMismatchField::Metadata
        );
    }
}

#[test]
fn final_payload_changes_are_observations_and_only_final_tool_arguments_are_accepted() {
    let mut stream = ResponseStream::default();
    let old = call("call-old", r#"{"command":"echo before"}"#);
    let new = call("call-final", r#"{"command":"echo after"}"#);
    stream
        .push(
            event(&json!({"type":"response.reasoning_summary_text.delta","delta":"受信した推論"}))
                .as_bytes(),
        )
        .unwrap();
    stream
        .push(
            event(&json!({"type":"response.output_item.done","output_index":0,"item":old}))
                .as_bytes(),
        )
        .unwrap();
    assert!(!stream.is_terminal());
    stream
        .push(
            event(&json!({"type":"response.completed","response":completed(vec![new.clone()])}))
                .as_bytes(),
        )
        .unwrap();
    let response = stream.finish(true).unwrap();
    assert_eq!(response.calls.len(), 1);
    assert_eq!(response.calls[0].call_id, "call-final");
    assert_eq!(
        response.calls[0].args.as_ref().unwrap().command,
        "echo after"
    );
    assert_eq!(response.output, vec![new]);
    let observation = response.stream_observation.unwrap();
    assert_eq!(observation.completed_items[&0], old);
    assert_eq!(observation.partial_reasoning, "受信した推論");
}

#[test]
fn difference_detail_is_bounded_while_all_received_items_are_preserved() {
    let mut stream = ResponseStream::default();
    let mut output = Vec::new();
    for index in 0..32 {
        let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"received"}],"provider_extra":index});
        stream
            .push(
                event(
                    &json!({"type":"response.output_item.done","output_index":index,"item":item}),
                )
                .as_bytes(),
            )
            .unwrap();
        output.push(json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"final"}]}));
    }
    stream
        .push(event(&json!({"type":"response.completed","response":completed(output)})).as_bytes())
        .unwrap();
    let response = stream.finish(true).unwrap();
    let observation = response.stream_observation.unwrap();
    assert_eq!(observation.total_differences, 32);
    assert_eq!(observation.differences.len(), 16);
    assert_eq!(observation.completed_items.len(), 32);
    assert!(
        observation
            .differences
            .iter()
            .all(|difference| difference.field == shellboy::model::OutputMismatchField::Text)
    );
}

#[test]
fn malformed_final_payload_still_fails_even_when_received_item_was_valid() {
    let mut stream = ResponseStream::default();
    stream.push(event(&json!({"type":"response.output_item.done","output_index":0,"item":call("safe",r#"{"command":"echo ignored"}"#)})).as_bytes()).unwrap();
    let malformed = json!({"status":"completed","output":null});
    assert!(
        stream
            .push(event(&json!({"type":"response.completed","response":malformed})).as_bytes())
            .is_err()
    );
    assert!(stream.is_terminal());
    stream
        .push(event(&json!({"type":"response.completed","response":completed(vec![])})).as_bytes())
        .unwrap();
    assert_eq!(stream.finish(true).unwrap_err().kind, "protocol");
}

#[test]
fn delta_only_changes_are_indexless_observations_and_do_not_change_final_adoption() {
    use shellboy::model::OutputMismatchField as Field;
    let output = vec![
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":"final reasoning"}]}),
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"final answer"}]}),
    ];
    let raw = completed(output.clone());
    for with_done in [false, true] {
        let mut body =
            event(&json!({"type":"response.output_text.delta","delta":"paid old answer"}));
        body += &event(
            &json!({"type":"response.reasoning_summary_text.delta","delta":"paid old reasoning"}),
        );
        if with_done {
            for (index, item) in output.iter().enumerate() {
                body += &event(
                    &json!({"type":"response.output_item.done","output_index":index,"item":item}),
                );
            }
        }
        body += &event(&json!({"type":"response.completed","response":raw}));
        for split in 0..=body.len() {
            let mut stream = ResponseStream::default();
            stream.push(&body.as_bytes()[..split]).unwrap();
            stream.push(&body.as_bytes()[split..]).unwrap();
            let response = stream.finish(true).unwrap();
            assert_eq!(response.raw, raw);
            assert_eq!(response.output, output);
            assert_eq!(response.text, "final answer");
            assert!(response.calls.is_empty());
            let observation = response.stream_observation.unwrap();
            assert!(
                observation.differences.is_empty(),
                "no fabricated item index"
            );
            assert_eq!(
                observation.delta_differences,
                vec![Field::Text, Field::ReasoningContent]
            );
            assert_eq!(observation.total_differences, 2);
            assert_eq!(observation.partial_text, "paid old answer");
            assert_eq!(observation.partial_reasoning, "paid old reasoning");
        }
    }
}

#[test]
fn matching_or_absent_delta_channels_do_not_create_observations() {
    let output = vec![
        json!({"type":"reasoning","summary":[{"type":"summary_text","text":"summary"}],"content":[{"type":"reasoning_text","text":"content"}],"encrypted_content":"opaque"}),
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"answer"}]}),
    ];
    for deltas in [false, true] {
        let mut stream = ResponseStream::default();
        if deltas {
            for (kind, text) in [
                ("response.output_text.delta", "answer"),
                ("response.reasoning_summary_text.delta", "summary"),
                ("response.reasoning_text.delta", "content"),
            ] {
                stream
                    .push(event(&json!({"type":kind,"delta":text})).as_bytes())
                    .unwrap();
            }
        }
        stream
            .push(
                event(&json!({"type":"response.completed","response":completed(output.clone())}))
                    .as_bytes(),
            )
            .unwrap();
        assert!(stream.finish(true).unwrap().stream_observation.is_none());
    }
}

#[test]
fn malformed_later_frame_keeps_received_prefix_at_every_chunk_split() {
    let item = json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"paid done text"}]} );
    let mut body = event(&json!({"type":"response.output_text.delta","delta":"paid answer 🦀"}));
    body += &event(&json!({"type":"response.reasoning_text.delta","delta":"paid reasoning"}));
    body += &event(&json!({"type":"response.output_item.done","output_index":0,"item":item}));
    body += "data: {malformed\n\n";
    for split in 0..=body.len() {
        let mut stream = ResponseStream::default();
        let failure = match stream.push(&body.as_bytes()[..split]) {
            Err(error) => error,
            Ok(_) => stream.push(&body.as_bytes()[split..]).unwrap_err(),
        };
        assert_eq!(failure.kind, "protocol", "split {split}");
        assert_eq!(stream.partial_text, "paid answer 🦀", "split {split}");
        assert_eq!(stream.partial_reasoning, "paid reasoning", "split {split}");
        assert_eq!(stream.completed_items[&0], item, "split {split}");
    }
}

#[test]
fn sse_framing_leading_bom_preserves_completed_at_every_split() {
    let raw = completed(vec![
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"日本語😀"}]}),
    ]);
    // A one-event stream exposes a lost first data line without an earlier created event.
    let wire = format!(
        "\u{feff}{}",
        event(&json!({"type":"response.completed","response":raw}))
    );
    for split in 0..=wire.len() {
        let mut stream = ResponseStream::default();
        stream.push(&wire.as_bytes()[..split]).unwrap();
        stream.push(&wire.as_bytes()[split..]).unwrap();
        let result = stream
            .finish(false)
            .unwrap_or_else(|e| panic!("split {split}: {e}"));
        assert_eq!(result.raw, raw);
        assert_eq!(result.text, "日本語😀");
    }
}

#[test]
fn sse_framing_bare_cr_terminal_after_lf_delta_is_not_lost() {
    let raw = completed(vec![
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"complete"}]}),
    ]);
    for ending in ["\r\r", "\r\n\n", "\r\r\n"] {
        let wire = format!(
            "data: {}\n\ndata: {}{ending}",
            json!({"type":"response.output_text.delta","delta":"complete"}),
            json!({"type":"response.completed","response":raw})
        );
        for split in 0..=wire.len() {
            let mut stream = ResponseStream::default();
            stream.push(&wire.as_bytes()[..split]).unwrap();
            stream.push(&wire.as_bytes()[split..]).unwrap();
            assert_eq!(stream.partial_text, "complete");
            assert_eq!(
                stream
                    .finish(false)
                    .unwrap_or_else(|e| panic!("split {split}: {e}"))
                    .raw,
                raw
            );
        }
    }
}

#[test]
fn sse_framing_all_standard_line_endings_preserve_multiline_data_and_utf8() {
    let raw = completed(vec![
        json!({"type":"message","role":"assistant","content":[{"type":"output_text","text":"日😀"}]}),
    ]);
    for newline in ["\n", "\r\n", "\r"] {
        let wire = format!(
            "\u{feff}: first comment{newline}{newline}event: response.completed{newline}data: {{\"type\":\"response.completed\",{newline}data: \"response\":{raw}}}{newline}{newline}"
        );
        for split in 0..=wire.len() {
            let mut stream = ResponseStream::default();
            stream.push(&wire.as_bytes()[..split]).unwrap();
            stream.push(&wire.as_bytes()[split..]).unwrap();
            assert_eq!(stream.finish(false).unwrap().raw, raw);
        }
        let mut stream = ResponseStream::default();
        for byte in wire.as_bytes() {
            stream.push(&[*byte]).unwrap();
        }
        assert_eq!(stream.finish(false).unwrap().text, "日😀");
    }
}

#[test]
fn sse_framing_eof_never_dispatches_an_unterminated_terminal_frame() {
    let raw = completed(vec![]);
    for newline in ["\n", "\r\n", "\r"] {
        for ending in ["", newline] {
            let wire = format!(
                "data: {}{ending}",
                json!({"type":"response.completed","response":raw})
            );
            let mut stream = ResponseStream::default();
            for byte in wire.as_bytes() {
                stream.push(&[*byte]).unwrap();
            }
            let error = stream.finish(false).unwrap_err();
            assert_eq!(error.kind, "incomplete");
            assert!(error.message.starts_with("EOF in partial SSE event"));
            assert!(error.message.contains("parsed_events=0"));
        }
    }
}

#[test]
fn sse_framing_strips_only_one_bom_at_the_start_of_the_stream() {
    let frame = event(&json!({"type":"response.completed","response":completed(vec![])}));
    for wire in [
        format!("\u{feff}\u{feff}{frame}"),
        format!(": earlier\n\n\u{feff}{frame}"),
    ] {
        let mut stream = ResponseStream::default();
        for byte in wire.as_bytes() {
            stream.push(&[*byte]).unwrap();
        }
        let error = stream.finish(false).unwrap_err();
        assert!(error.message.starts_with("EOF before response.completed"));
        assert!(error.message.contains("parsed_events=0"));
    }
}

#[test]
fn eof_diagnostics_distinguish_untyped_done_only_and_truncated_frames_without_content() {
    let secret = "synthetic-body-not-a-diagnostic";
    let mut stream = ResponseStream::default();
    let wire = format!(
        "data: {{\"type\":\"response.output_text.delta\",\"delta\":\"{secret}\"}}\n\ndata: {{\"response\":{{\"status\":\"completed\"}}}}\n\ndata: [DONE]\n\n"
    );
    stream.push(wire.as_bytes()).unwrap();
    let error = stream.finish(false).unwrap_err();
    assert!(
        error
            .message
            .contains(&format!("received_bytes={}", wire.len()))
    );
    for fact in [
        "parsed_events=2",
        "done_markers=1",
        "pending_line_bytes=0",
        "pending_data_bytes=0",
        "last_event=untyped",
    ] {
        assert!(error.message.contains(fact), "{}", error.message);
    }
    assert!(!error.message.contains(secret));
    let mut stream = ResponseStream::default();
    stream
        .push(event(&json!({"type":secret})).as_bytes())
        .unwrap();
    let error = stream.finish(false).unwrap_err();
    assert!(error.message.contains("last_event=other"));
    assert!(!error.message.contains(secret));
}

#[test]
fn stream_output_observation_distinguishes_metadata_from_all_output_families() {
    let mut metadata = ResponseStream::default();
    metadata.push(b": heartbeat\n\n").unwrap();
    for kind in ["response.created", "response.in_progress"] {
        metadata
            .push(event(&json!({"type":kind,"response":{"output":[]}})).as_bytes())
            .unwrap();
        assert!(!metadata.has_output(), "{kind}");
    }
    for value in [
        json!({"type":"response.output_text.delta","delta":"answer"}),
        json!({"type":"response.refusal.delta","delta":"no"}),
        json!({"type":"response.reasoning_summary_text.delta","delta":"thought"}),
        json!({"type":"response.reasoning_text.delta","delta":"thought"}),
        json!({"type":"response.function_call_arguments.delta","delta":"{"}),
        json!({"type":"response.output_item.added","item":{"type":"function_call"}}),
        json!({"type":"response.content_part.added","part":{"type":"output_text","text":""}}),
        json!({"type":"response.output_future_kind.delta"}),
        json!({"type":"response.in_progress","response":{"output":[{"type":"reasoning"}]}}),
    ] {
        let mut stream = ResponseStream::default();
        stream.push(event(&value).as_bytes()).unwrap();
        assert!(stream.has_output(), "{value}");
    }
}
