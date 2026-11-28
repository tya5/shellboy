//! Admission consumes producer observations, never original files.
use super::*;

pub(super) struct Record<'a> {
    pub reference: &'a str,
    pub reader_args: &'a [&'a str],
    pub material: RevisionInput<'a>,
}

pub(super) struct Admission {
    pub document: String,
    pub revision: String,
    pub created: bool,
}

pub(super) async fn admit_batch<'a>(
    store: &mut Store,
    records: impl IntoIterator<Item = Record<'a>>,
    profile: Option<&search::Profile>,
) -> Result<Vec<Result<Admission>>> {
    const MAX_RECORDS: usize = 32;
    let records: Vec<_> = records.into_iter().take(MAX_RECORDS + 1).collect();
    if records.len() > MAX_RECORDS {
        return Err(err("input_limit", "Record batch exceeds 32 records"));
    }
    let mut outcomes = Vec::with_capacity(records.len());
    for record in records {
        outcomes.push(admit(store, record, profile).await);
    }
    Ok(outcomes)
}

async fn admit(
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
    if record.material.provenance.to_string().len() > 64 * 1024 {
        return Err(err("input_limit", "Record metadata exceeds 64 KiB"));
    }
    let reader_args = serde_json::json!(record.reader_args).to_string();
    if reader_args.len() > 64 * 1024 {
        return Err(err("input_limit", "Record reader exceeds 64 KiB"));
    }
    if record.material.extracted.text.len() > MAX_INPUT {
        return Err(err("input_limit", "Extracted text exceeds 16 MiB"));
    }
    if let Some(profile) = profile {
        search::embed_material(store, profile, record.material.extracted).await?;
    }
    let tx = store.db.transaction()?;
    let (document, revision, created) =
        prepare(&tx, record.material, record.reference, &reader_args)?;
    activate(&tx, &document, &revision)?;
    tx.commit()?;
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
            reader_args: &["fixture"],
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
        let invalid_metadata = json!({"too_large": "x".repeat(64 * 1024)});
        let mut invalid = record("opaque:rejected");
        invalid.material.provenance = &invalid_metadata;
        let mut outcomes = admit_batch(
            &mut store,
            [record("opaque:first"), invalid, record("opaque:second")],
            None,
        )
        .await
        .unwrap()
        .into_iter();
        let first = outcomes.next().unwrap().unwrap();
        assert!(matches!(outcomes.next(), Some(Err(error)) if error.code == "input_limit"));
        outcomes.next().unwrap().unwrap();
        let visible = |store: &Store| {
            let mut titles: Vec<_> = search::material_hits(store, "@derived", text)
                .unwrap()
                .into_iter()
                .map(|hit| hit.title)
                .collect();
            titles.sort();
            titles
        };
        assert_eq!(visible(&store), ["opaque:first", "opaque:second"]);
        retire(&mut store, &first.document).unwrap();
        assert_eq!(visible(&store), ["opaque:second"]);
    }
}
