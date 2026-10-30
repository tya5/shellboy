//! Canonical human input order; UI markers never enter this representation.
use super::*;

#[derive(Clone, Debug)]
pub(crate) struct DraftContent {
    pub text: String,
    pub positions: Vec<usize>,
}

impl DraftContent {
    pub fn appended(text: String, files: usize) -> Self {
        let positions = vec![text.len(); files];
        Self { text, positions }
    }
    pub fn validate(&self, files: usize) -> Result<(), ApiError> {
        if self.positions.len() != files
            || self.positions.windows(2).any(|p| p[0] > p[1])
            || self
                .positions
                .iter()
                .any(|&p| !self.text.is_char_boundary(p))
        {
            return Err(ApiError::new(
                ErrorCode::InvalidArgument,
                "Draft attachment positions must match files in UTF-8 text order",
            ));
        }
        Ok(())
    }
    /// Every request records its canonical positions, including append-only input.
    pub fn fingerprint_positions(&self, value: &mut Value) {
        value["attachment_positions"] = json!(self.positions);
    }
    /// Allocate final references before the source node is inserted.
    pub fn new_source_elements(&self, files: usize) -> Result<Vec<Value>, ApiError> {
        self.validate(files)?;
        let attachments = (0..files).map(|_| id()).collect::<Vec<_>>();
        self.elements(&attachments)
    }
    pub fn elements(&self, attachments: &[String]) -> Result<Vec<Value>, ApiError> {
        self.validate(attachments.len())?;
        let mut elements = Vec::new();
        let mut start = 0;
        for (&position, attachment) in self.positions.iter().zip(attachments) {
            if start < position {
                elements.push(json!({"type":"input_text","text":self.text[start..position]}));
            }
            elements.push(json!({"type":"attachment_ref","attachment_id":attachment}));
            start = position;
        }
        if start < self.text.len() || elements.is_empty() {
            elements.push(json!({"type":"input_text","text":self.text[start..]}));
        }
        Ok(elements)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_elements_keep_unicode_text_and_adjacent_references_in_order() {
        let content = DraftContent {
            text: "前後終".into(),
            positions: vec![0, 3, 3, 6],
        };
        assert_eq!(
            content
                .elements(&["first", "a", "b", "c"].map(str::to_owned))
                .unwrap(),
            vec![
                json!({"type":"attachment_ref","attachment_id":"first"}),
                json!({"type":"input_text","text":"前"}),
                json!({"type":"attachment_ref","attachment_id":"a"}),
                json!({"type":"attachment_ref","attachment_id":"b"}),
                json!({"type":"input_text","text":"後"}),
                json!({"type":"attachment_ref","attachment_id":"c"}),
                json!({"type":"input_text","text":"終"}),
            ]
        );
        for positions in [vec![1], vec![10], vec![3, 0], vec![]] {
            let files = positions.len().max(1);
            assert!(
                DraftContent {
                    text: "前後".into(),
                    positions
                }
                .validate(files)
                .is_err()
            );
        }
    }

    #[test]
    fn append_fingerprint_includes_positions_and_empty_attachment_has_no_text_part() {
        let mut request = json!({"text":"before after"});
        DraftContent::appended("before after".into(), 2).fingerprint_positions(&mut request);
        assert_eq!(request["attachment_positions"], json!([12, 12]));
        DraftContent {
            text: "before after".into(),
            positions: vec![6, 6],
        }
        .fingerprint_positions(&mut request);
        assert_eq!(request["attachment_positions"], json!([6, 6]));
        let content = DraftContent::appended(String::new(), 1);
        let elements = content.new_source_elements(1).unwrap();
        assert_eq!(elements.len(), 1);
        assert_eq!(elements[0]["type"], "attachment_ref");
        assert!(elements[0]["attachment_id"].as_str().is_some());
        let plain_empty = DraftContent::appended(String::new(), 0)
            .new_source_elements(0)
            .unwrap();
        assert_eq!(plain_empty, vec![json!({"type":"input_text","text":""})]);
    }
    #[tokio::test]
    async fn ordered_acceptance_replays_same_receipt_and_position_conflicts_add_no_sources() {
        let root = tempfile::tempdir().unwrap();
        let config = root.path().join("config.toml");
        std::fs::write(&config,"default_connection='fixture'\ndefault_model='fixture'\n[connections.fixture]\nbase_url='http://127.0.0.1:9/v1'\n").unwrap();
        let owner = crate::app::DataOwner::start(
            Some(config),
            Some(root.path().join("data")),
            root.path().into(),
        )
        .await
        .unwrap();
        let control = crate::control::Control::new(&owner, root.path().into());
        let operation = format!("{}:ordered-acceptance", control.store.instance_id);
        let args = crate::protocol::ThreadCreateArgs {
            agents: None,
            connection: None,
            cwd: None,
            model: None,
            effort: None,
            parent_id: None,
        };
        let content = DraftContent {
            text: "前後".into(),
            positions: vec![3],
        };
        let files = vec![PlannedAttachment {
            path: root.path().join("a.txt"),
            kind: AttachmentKind::File,
            observed_bytes: Some(1),
            managed_id: None,
        }];
        let first = control
            .store
            .send_draft_content(
                operation.clone(),
                args.clone(),
                None,
                content.clone(),
                files.clone(),
                crate::control::db::DraftSendOptions::default(),
            )
            .await
            .unwrap();
        let replay = control
            .store
            .send_draft_content(
                operation.clone(),
                args.clone(),
                None,
                content,
                files.clone(),
                crate::control::db::DraftSendOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(first.result, replay.result);
        let conflict = control
            .store
            .send_draft_content(
                operation.clone(),
                args.clone(),
                None,
                DraftContent {
                    text: "前後".into(),
                    positions: vec![0],
                },
                files.clone(),
                crate::control::db::DraftSendOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(conflict.code, ErrorCode::OperationConflict);
        let invalid = control
            .store
            .send_draft_content(
                format!("{}:invalid-order", control.store.instance_id),
                args,
                None,
                DraftContent {
                    text: "前後".into(),
                    positions: vec![1],
                },
                files,
                crate::control::db::DraftSendOptions::default(),
            )
            .await
            .unwrap_err();
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
        control
            .store
            .database
            .call(move |db| {
                for table in [
                    "thread",
                    "node",
                    "attachment_ref",
                    "receipt",
                    "operation_receipt",
                ] {
                    assert_eq!(
                        db.query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r
                            .get::<_, i64>(0))?,
                        1,
                        "{table}"
                    );
                }
                let body: String =
                    db.query_row("SELECT payload_json FROM node", [], |r| r.get(0))?;
                let payload: Value = serde_json::from_str(&body).unwrap();
                assert_eq!(payload["content"][0]["text"], "前");
                assert_eq!(payload["content"][1]["type"], "attachment_ref");
                assert_eq!(payload["content"][2]["text"], "後");
                Ok(())
            })
            .await
            .unwrap();
        drop(control);
        owner.shutdown().await.unwrap();
    }
}
