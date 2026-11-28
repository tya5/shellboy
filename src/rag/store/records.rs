//! Admission consumes producer observations, never original files.
use super::*;

pub(super) struct Record<'a> {
    pub reference: &'a str,
    pub material: RevisionInput<'a>,
}

pub(super) struct Admission {
    pub document: String,
    pub revision: String,
    pub created: bool,
}

pub(super) async fn admit(
    store: &mut Store,
    record: Record<'_>,
    profile: Option<&search::Profile>,
) -> Result<Admission> {
    if record.reference.is_empty() || record.reference.len() > 8192 {
        return Err(err(
            "invalid_argument",
            "Record ref must contain 1..=8192 bytes",
        ));
    }
    let Some(mut metadata) = record.material.provenance.as_object().cloned() else {
        return Err(err("invalid_argument", "Record metadata must be an object"));
    };
    metadata.insert("ref".into(), json!(record.reference));
    let provenance = Value::Object(metadata);
    let (document, revision, created) = prepare(
        store,
        RevisionInput {
            provenance: &provenance,
            ..record.material
        },
    )?;
    if let Some(profile) = profile {
        search::embed_revision(store, &revision, profile).await?;
    }
    publish(store, &document, &revision)?;
    Ok(Admission {
        document,
        revision,
        created,
    })
}

pub(super) fn retire(store: &mut Store, document: &str) -> Result<()> {
    let tx = store.db.transaction()?;
    if tx.execute(
        "UPDATE documents SET active_revision=NULL,status='missing',error=NULL WHERE id=?1 AND (active_revision IS NOT NULL OR status!='missing')",
        [document],
    )? != 0 {
        tx.execute("UPDATE meta SET value=CAST(value AS INTEGER)+1 WHERE key='generation'", [])?;
    }
    tx.commit()?;
    Ok(())
}

// Encode components, preserving only relative-path separators. Producer and
// reader use the same byte encoding; admission treats the result as opaque.
pub(in crate::rag) fn file_ref(source: &str, relative: &str) -> String {
    fn component(value: &str) -> String {
        use std::fmt::Write;
        let mut encoded = String::new();
        for byte in value.bytes() {
            if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
                encoded.push(char::from(byte));
            } else {
                write!(encoded, "%{byte:02X}").unwrap();
            }
        }
        encoded
    }
    let path = relative
        .split('/')
        .map(component)
        .collect::<Vec<_>>()
        .join("/");
    format!("file:{}/{path}", component(source))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn observations_do_not_require_originals_and_only_explicit_retirement_deactivates() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("rag.db");
        initialize(&path).unwrap();
        let mut store = open(&path, true).unwrap();
        let text = "indexed observation";
        let extracted = extract::Extracted {
            text: text.into(),
            blocks: vec![extract::Block {
                text: text.into(),
                location: Value::Null,
            }],
            extractor: "test-producer".into(),
        };
        let fingerprint = hash(b"original bytes, not the excerpt");
        let metadata = json!({"original_path": root.path().join("nonexistent")});
        let record = |key| Record {
            reference: key,
            material: RevisionInput {
                source: "@derived",
                key,
                source_hash: &fingerprint,
                extracted: &extracted,
                kind: "test",
                title: key,
                provenance: &metadata,
            },
        };
        let first = admit(&mut store, record("opaque:first"), None)
            .await
            .unwrap();
        let second = admit(&mut store, record("opaque:second"), None)
            .await
            .unwrap();
        let active = |s: &Store, id: &str| {
            s.db.query_row(
            "SELECT d.active_revision,r.source_hash,json_extract(r.provenance,'$.ref') FROM documents d LEFT JOIN revisions r ON d.active_revision=r.id WHERE d.id=?1",
            [id], |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, Option<String>>(1)?, row.get::<_, Option<String>>(2)?)),
        ).unwrap()
        };
        assert_eq!(
            active(&store, &first.document),
            (
                Some(first.revision),
                Some(fingerprint.clone()),
                Some("opaque:first".into())
            )
        );
        retire(&mut store, &first.document).unwrap();
        assert_eq!(active(&store, &first.document), (None, None, None));
        assert_eq!(active(&store, &second.document).0, Some(second.revision));
    }
}
