use super::*;
use crate::config::{Api, Connection, ContextRole};
use crate::model::http::HttpModelClient;
use serde_json::json;

fn len(value: &Value) -> u64 {
    serde_json::to_vec(value).unwrap().len() as u64
}

#[test]
fn nested_labels_partition_every_byte_and_framing_is_other() {
    let wire = json!({"input":[{"text":"日本語\n\"","extra":[1,2]},{"text":"x"}],"stream":true});
    let labels = vec![
        ("/input/0".into(), Label::Whole(Category::User)),
        ("/input/0/extra".into(), Label::Whole(Category::Attachment)),
        ("/input/1".into(), Label::Whole(Category::ToolResult)),
    ];
    let bytes = measure(&wire, &labels).unwrap();
    assert_eq!(bytes.total, len(&wire));
    // The member key, colon and one comma go with the labelled member;
    // `extra` sorts first, so it takes the comma after it.
    assert_eq!(
        bytes.get(Category::Attachment),
        len(&json!([1, 2])) + "\"extra\":,".len() as u64
    );
    assert_eq!(
        bytes.get(Category::User) + bytes.get(Category::Attachment),
        len(&wire["input"][0])
    );
    assert_eq!(bytes.get(Category::ToolResult), len(&wire["input"][1]) + 1);
    assert_eq!(
        bytes.get(Category::Other),
        len(&wire) - len(&wire["input"][0]) - len(&wire["input"][1]) - 1
    );
}

#[test]
fn split_strings_charge_escaped_pieces_and_keep_separators_outside() {
    let (a, b) = ("sys \"q\"\n", "agents 日本");
    let wire = json!({"system":format!("{a}\n\n{b}")});
    let labels = vec![(
        "/system".into(),
        Label::Split(vec![
            (Category::System, escaped_len(a)),
            (Category::Instructions, escaped_len(b)),
        ]),
    )];
    let bytes = measure(&wire, &labels).unwrap();
    assert_eq!(bytes.get(Category::System), len(&json!(a)) - 2);
    assert_eq!(bytes.get(Category::Instructions), len(&json!(b)) - 2);
    assert_eq!(bytes.total, len(&wire));
    assert_eq!(
        bytes.get(Category::Other),
        "{\"system\":\"\"}".len() as u64 + "\\n\\n".len() as u64
    );
}

#[test]
fn unresolved_or_duplicate_labels_are_unknown_not_guessed() {
    let wire = json!({"input":[1]});
    assert!(measure(&wire, &[("/missing".into(), Label::Whole(Category::User))]).is_none());
    assert!(
        measure(
            &wire,
            &[
                ("/input".into(), Label::Whole(Category::User)),
                ("/input".into(), Label::Whole(Category::Notice)),
            ]
        )
        .is_none()
    );
}

#[test]
fn saved_values_round_trip_and_reject_inconsistent_documents() {
    let wire = json!({"a":"b"});
    let bytes = measure(&wire, &[("/a".into(), Label::Whole(Category::User))]).unwrap();
    let saved = bytes.to_value();
    assert_eq!(saved["unit"], "utf8_bytes");
    assert_eq!(saved["categories"].as_object().unwrap().len(), 13);
    assert_eq!(InputBytes::from_value(&saved), Some(bytes));
    let mut wrong = saved.clone();
    wrong["total"] = json!(1);
    assert_eq!(InputBytes::from_value(&wrong), None);
    let mut missing = saved;
    missing["categories"]
        .as_object_mut()
        .unwrap()
        .remove("user");
    assert_eq!(InputBytes::from_value(&missing), None);
    assert_eq!(InputBytes::from_value(&Value::Null), None);
}

#[test]
fn history_items_are_classified_by_node_kind_then_shape() {
    let user = json!({"role":"user","content":"x"});
    let output = json!({"type":"function_call_output","call_id":"c","output":"o"});
    assert_eq!(
        Category::of_history("message", false, &user),
        Category::User
    );
    assert_eq!(
        Category::of_history("context", false, &user),
        Category::Context
    );
    assert_eq!(
        Category::of_history("summary", false, &user),
        Category::Summary
    );
    assert_eq!(
        Category::of_history("event", false, &user),
        Category::Notice
    );
    assert_eq!(
        Category::of_history("tool_result", false, &output),
        Category::ToolResult
    );
    assert_eq!(
        Category::of_history("spill", true, &output),
        Category::SpillPlaceholder
    );
    assert_eq!(
        Category::of_history(
            "assistant_response",
            false,
            &json!({"type":"function_call","call_id":"c","name":"shell","arguments":"{}"})
        ),
        Category::ToolCall
    );
    assert_eq!(
        Category::of_history("assistant_response", false, &json!({"type":"reasoning"})),
        Category::Reasoning
    );
    assert_eq!(
        Category::of_history(
            "assistant_response",
            false,
            &crate::model::opaque::wrap(
                "anthropic_messages",
                "thinking",
                json!({"type":"thinking"})
            )
        ),
        Category::Reasoning
    );
    assert_eq!(
        Category::of_history("message", false, &json!({"role":"developer","content":"n"})),
        Category::Notice
    );
}

fn text(role: &str, text: &str) -> Value {
    let kind = if role == "assistant" {
        "output_text"
    } else {
        "input_text"
    };
    json!({"role":role,"content":[{"type":kind,"text":text}]})
}

/// A synthetic conversation with every source, in canonical (Responses) form.
fn conversation() -> (Value, Vec<ItemTag>) {
    let attachment = json!({"type":"input_text","text":"attached file 日本語\n\"body\""});
    let image = json!({"type":"input_image","image_url":"data:image/png;base64,AAAA"});
    let items = vec![
        (
            text("system", "You are Shellboy \"x\"\n"),
            ItemTag::new(Category::System),
        ),
        (
            text("developer", "Working instructions 日本"),
            ItemTag::new(Category::Instructions),
        ),
        (
            text("user", "earlier question"),
            ItemTag::new(Category::User),
        ),
        (
            text("developer", "summary of earlier work"),
            ItemTag::new(Category::Summary),
        ),
        (
            text("developer", "Shellboy context v1: thread=t cwd=/w"),
            ItemTag::new(Category::Context),
        ),
        (
            json!({"role":"user","content":[{"type":"input_text","text":"look"},attachment,image]}),
            ItemTag {
                category: Category::User,
                parts: vec![(1, Category::Attachment), (2, Category::Attachment)],
            },
        ),
        (
            json!({"type":"reasoning","summary":[],"encrypted_content":"ENC"}),
            ItemTag::new(Category::Reasoning),
        ),
        (
            crate::model::opaque::wrap(
                "anthropic_messages",
                "thinking",
                json!({"type":"thinking","thinking":"考え","signature":"sig"}),
            ),
            ItemTag::new(Category::Reasoning),
        ),
        (
            text("assistant", "I will run it"),
            ItemTag::new(Category::Assistant),
        ),
        (
            json!({"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"command\":\"ls\"}"}),
            ItemTag::new(Category::ToolCall),
        ),
        (
            json!({"type":"function_call","call_id":"c2","name":"shell","arguments":"{\"command\":\"pwd\"}"}),
            ItemTag::new(Category::ToolCall),
        ),
        (
            json!({"type":"function_call_output","call_id":"c1","output":"file\n\"a\""}),
            ItemTag::new(Category::ToolResult),
        ),
        (
            json!({"type":"function_call_output","call_id":"c2","output":"{\"omitted\":true}"}),
            ItemTag::new(Category::SpillPlaceholder),
        ),
        (
            text("developer", "event: shell ended"),
            ItemTag::new(Category::Notice),
        ),
        (
            json!({"role":"developer","content":[{"type":"input_text","text":"detached attachment"}]}),
            ItemTag::new(Category::Attachment),
        ),
        (
            text("developer", "attachment updates"),
            ItemTag::new(Category::Notice),
        ),
    ];
    let (input, tags): (Vec<_>, Vec<_>) = items.into_iter().unzip();
    let request = crate::model::response_request("fixture", input, Some("high"), true);
    (request, tags)
}

fn client(api: Api, caching: bool, role: ContextRole) -> HttpModelClient {
    let mut extra = serde_json::Map::new();
    extra.insert("temperature".into(), json!(0.5));
    HttpModelClient::new(Connection {
        api,
        base_url: "http://127.0.0.1:1".into(),
        prompt_caching: caching,
        context_role: role,
        extra_body: extra,
        ..Connection::default()
    })
    .unwrap()
}

/// Every dialect, with and without cache hints and role mapping: accounting
/// leaves the body byte-identical, and the categories sum to its length.
#[test]
fn every_dialect_partitions_the_exact_sent_body() {
    let (request, tags) = conversation();
    for api in [Api::Responses, Api::ChatCompletions, Api::AnthropicMessages] {
        for caching in [false, true] {
            for role in [ContextRole::Developer, ContextRole::User] {
                let client = client(api, caching, role);
                let (wire, _) = client
                    .wire_request(request.clone(), Some("thread-1"))
                    .unwrap();
                let plain = serde_json::to_vec(&wire).unwrap();
                let bytes = client
                    .input_bytes(request.clone(), &tags, Some("thread-1"))
                    .unwrap()
                    .unwrap_or_else(|| panic!("{api:?} {caching} {role:?}"));
                assert_eq!(bytes.total, plain.len() as u64, "{api:?} {caching}");
                assert_eq!(
                    Category::ALL.iter().map(|c| bytes.get(*c)).sum::<u64>(),
                    bytes.total
                );
                assert!(!String::from_utf8(plain).unwrap().contains(MARK));
                for category in Category::ALL {
                    let dropped =
                        matches!((api, category), (Api::ChatCompletions, Category::Reasoning));
                    assert_eq!(bytes.get(category) == 0, dropped, "{api:?} {category:?}");
                }
            }
        }
    }
}

#[test]
fn responses_counts_each_kept_item_and_drops_other_dialects_blocks() {
    let (request, tags) = conversation();
    let client = client(Api::Responses, false, ContextRole::Developer);
    let (wire, _) = client.wire_request(request.clone(), None).unwrap();
    let bytes = client.input_bytes(request, &tags, None).unwrap().unwrap();
    let input = wire["input"].as_array().unwrap();
    // The Anthropic thinking block is not sent; only the Responses reasoning.
    assert_eq!(input.len(), 15);
    assert_eq!(bytes.get(Category::Reasoning), len(&input[6]) + 1);
    assert_eq!(bytes.get(Category::System), len(&input[0]));
    let user_item = &input[5];
    let parts = user_item["content"].as_array().unwrap();
    assert_eq!(
        bytes.get(Category::Attachment),
        len(&parts[1]) + 1 + len(&parts[2]) + 1 + len(&input[13]) + 1
    );
    assert_eq!(
        bytes.get(Category::User),
        len(&input[2]) + 1 + len(user_item) + 1 - (len(&parts[1]) + len(&parts[2]) + 2)
    );
}

#[test]
fn chat_splits_merged_assistant_messages_and_drops_reasoning() {
    let (request, tags) = conversation();
    let client = client(Api::ChatCompletions, false, ContextRole::Developer);
    let (wire, _) = client.wire_request(request.clone(), None).unwrap();
    let bytes = client.input_bytes(request, &tags, None).unwrap().unwrap();
    let messages = wire["messages"].as_array().unwrap();
    let merged = messages.iter().find(|m| m["role"] == "assistant").unwrap();
    // The assistant text and both calls became one message; the calls keep
    // their own source, the message framing stays with the assistant item.
    let calls = merged["tool_calls"].as_array().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(
        bytes.get(Category::ToolCall),
        len(&calls[0]) + len(&calls[1]) + 1
    );
    assert_eq!(bytes.get(Category::Reasoning), 0);
    let tool = messages
        .iter()
        .find(|m| m["role"] == "tool" && m["tool_call_id"] == "c1")
        .unwrap();
    assert_eq!(bytes.get(Category::ToolResult), len(tool) + 1);
}

#[test]
fn messages_moves_leading_system_and_keeps_each_source_where_it_lands() {
    let (request, tags) = conversation();
    for caching in [false, true] {
        let client = client(Api::AnthropicMessages, caching, ContextRole::Developer);
        let (wire, _) = client.wire_request(request.clone(), None).unwrap();
        let bytes = client
            .input_bytes(request.clone(), &tags, None)
            .unwrap()
            .unwrap();
        // The leading system and developer items were joined into `system`.
        let system = if caching {
            wire["system"][0]["text"].as_str().unwrap()
        } else {
            wire["system"].as_str().unwrap()
        };
        assert_eq!(
            system,
            "You are Shellboy \"x\"\n\n\nWorking instructions 日本"
        );
        assert_eq!(
            bytes.get(Category::System),
            escaped_len("You are Shellboy \"x\"\n")
        );
        assert_eq!(
            bytes.get(Category::Instructions),
            escaped_len("Working instructions 日本")
        );
        // Mid-conversation developer items became user content blocks but
        // still count as their own source. The thinking block starts the
        // assistant message, so it also carries that message's framing; the
        // merged text and tool calls that follow keep their own sources.
        let assistant = wire["messages"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["role"] == "assistant")
            .unwrap();
        let blocks = assistant["content"].as_array().unwrap();
        assert_eq!(blocks[0]["type"], "thinking");
        let followers = blocks[1..].iter().map(|b| len(b) + 1).sum::<u64>();
        assert_eq!(
            bytes.get(Category::Reasoning),
            1 + len(assistant) - followers
        );
        assert_eq!(
            bytes.get(Category::ToolCall),
            blocks
                .iter()
                .filter(|b| b["type"] == "tool_use")
                .map(|b| len(b) + 1)
                .sum::<u64>()
        );
    }
    // Cache markers are configuration: only `other` changes with them.
    let off = client(Api::AnthropicMessages, false, ContextRole::Developer)
        .input_bytes(request.clone(), &tags, None)
        .unwrap()
        .unwrap();
    let on = client(Api::AnthropicMessages, true, ContextRole::Developer)
        .input_bytes(request, &tags, None)
        .unwrap()
        .unwrap();
    for category in Category::ALL {
        if category != Category::Other {
            assert_eq!(off.get(category), on.get(category), "{category:?}");
        }
    }
    assert!(on.get(Category::Other) > off.get(Category::Other));
}

#[test]
fn mismatched_tags_are_unknown() {
    let (request, mut tags) = conversation();
    tags.pop();
    let client = client(Api::Responses, false, ContextRole::Developer);
    assert_eq!(client.input_bytes(request, &tags, None).unwrap(), None);
}
