//! Read-only retained shell output decoding.
use super::*;

fn walk(
    db: &Connection,
    witness: &Value,
    mut visit: impl FnMut(&str, usize) -> Result<()>,
) -> Result<()> {
    let execution = witness["execution"]
        .as_str()
        .ok_or_else(|| err("revision_unavailable", "Invalid raw witness"))?;
    let generation = witness["generation"]
        .as_i64()
        .ok_or_else(|| err("revision_unavailable", "Invalid raw generation"))?;
    let saved = witness["saved_sequence"]
        .as_i64()
        .ok_or_else(|| err("revision_unavailable", "Invalid raw sequence"))?;
    let label = witness["encoding"]
        .as_str()
        .ok_or_else(|| err("revision_unavailable", "Invalid raw encoding"))?;
    let valid = crate::history::RawWitness {
        execution: execution.into(),
        generation,
        saved_sequence: saved,
        encoding: label.into(),
    }
    .is_current(db)?;
    if !valid {
        return Err(err(
            "revision_unavailable",
            "Raw output was collected or its source witness changed",
        ));
    }
    let encoding = encoding_rs::Encoding::for_label(label.as_bytes())
        .ok_or_else(|| err("revision_unavailable", "Raw encoding is unsupported"))?;
    let mut decoders = crate::control::output::Decoders::new(encoding);
    // Incremental BLOB I/O avoids materializing even a legacy giant chunk.
    const PAGE: usize = 64 * 1024;
    let mut bytes = vec![0; PAGE];
    let mut q = db
        .prepare("SELECT rowid,stream FROM output_chunk WHERE execution_id=?1 ORDER BY sequence")?;
    let mut rows = q.query([execution])?;
    while let Some(row) = rows.next()? {
        let rowid: i64 = row.get(0)?;
        let stream: String = row.get(1)?;
        let blob = db.blob_open("main", "output_chunk", "bytes", rowid, true)?;
        for offset in (0..blob.len()).step_by(PAGE) {
            let length = PAGE.min(blob.len() - offset);
            blob.read_at_exact(&mut bytes[..length], offset)?;
            let (text, _) = decoders.decode(&stream, &bytes[..length], false);
            visit(&text, length)?;
        }
    }
    let (text, _) = decoders.finish(crate::control::output::StreamSelection::Both);
    visit(&text, 0)
}

pub(super) fn raw_text(db: &Connection, witness: &Value) -> Result<String> {
    let range = witness
        .get("range")
        .map(|v| {
            let start = v["start"].as_u64().and_then(|n| usize::try_from(n).ok());
            let end = v["end"].as_u64().and_then(|n| usize::try_from(n).ok());
            match (start, end) {
                (Some(start), Some(end)) if start <= end && end - start <= WINDOW => {
                    Ok((start, end))
                }
                _ => Err(err("revision_unavailable", "Invalid raw text range")),
            }
        })
        .transpose()?;
    let mut text = String::new();
    let mut bytes = 0usize;
    let mut position = 0usize;
    walk(db, witness, |part, raw_bytes| {
        bytes = bytes.saturating_add(raw_bytes);
        if let Some((start, end)) = range {
            let count = part.chars().count();
            text.extend(
                part.chars()
                    .skip(start.saturating_sub(position))
                    .take(end.saturating_sub(position.max(start))),
            );
            position += count;
        } else {
            if bytes > MAX_INPUT {
                return Err(err(
                    "input_limit",
                    "Raw output exceeds bounded retrieval limit",
                ));
            }
            text.push_str(part);
            if text.len() > MAX_INPUT {
                return Err(err(
                    "input_limit",
                    "Decoded output exceeds bounded retrieval limit",
                ));
            }
        }
        Ok(())
    })?;
    if range.is_some_and(|(_, end)| position < end) {
        return Err(err(
            "revision_unavailable",
            "Raw text range is no longer available",
        ));
    }
    Ok(text)
}

/// Verify complete indexed spans with one decoder walk per saved raw witness.
/// Each result retains at most WINDOW characters, independently of span size.
pub(super) fn semantic_batch(
    db: &Connection,
    requests: &[(Value, &crate::index::SourceSpan)],
) -> Result<Vec<(String, Value)>> {
    semantic_batch_from(requests, |witness, visit| walk(db, witness, visit))
}

type RawVisitor<'a> = dyn FnMut(&str, usize) -> Result<()> + 'a;

fn semantic_batch_from(
    requests: &[(Value, &crate::index::SourceSpan)],
    mut read: impl FnMut(&Value, &mut RawVisitor<'_>) -> Result<()>,
) -> Result<Vec<(String, Value)>> {
    use sha2::{Digest, Sha256};
    use std::collections::BTreeMap;

    // This request-local map is bounded by the existing KNN candidate limit.
    // It holds witness/indices only; no raw body or long-lived cache is added.
    let mut groups: BTreeMap<String, Vec<usize>> = BTreeMap::new();
    for (index, (witness, span)) in requests.iter().enumerate() {
        if span.start > span.end {
            return Err(err("revision_unavailable", "Invalid indexed raw range"));
        }
        groups.entry(witness.to_string()).or_default().push(index);
    }
    let mut results: Vec<Option<(String, Value)>> = std::iter::repeat_with(|| None)
        .take(requests.len())
        .collect();
    for indices in groups.into_values() {
        let witness = &requests[indices[0]].0;
        let mut states: Vec<_> = indices
            .iter()
            .map(|&index| (index, Sha256::new(), String::new(), 0usize))
            .collect();
        let mut position = 0usize;
        let mut whole = Some(String::new());
        read(witness, &mut |part, _| {
            // Decode each page once, and share its character-to-byte offsets.
            // Work on a span only where it overlaps this page. Hashing genuinely
            // overlapping indexed text remains necessary for each source hash.
            let mut offsets: Vec<_> = part.char_indices().map(|(offset, _)| offset).collect();
            let count = offsets.len();
            offsets.push(part.len());
            if let Some(body) = &mut whole {
                if position.saturating_add(count) <= WINDOW {
                    body.push_str(part);
                } else {
                    whole = None;
                }
            }
            for (index, digest, selected, retained) in &mut states {
                let span = requests[*index].1;
                let start = span.start.saturating_sub(position).min(count);
                let end = span.end.saturating_sub(position).min(count);
                if start >= end {
                    continue;
                }
                digest.update(&part.as_bytes()[offsets[start]..offsets[end]]);
                let keep = (end - start).min(WINDOW - *retained);
                selected.push_str(&part[offsets[start]..offsets[start + keep]]);
                *retained += keep;
            }
            position = position.saturating_add(count);
            Ok(())
        })?;
        for (index, digest, selected, retained) in states {
            let span = requests[index].1;
            let actual = digest
                .finalize()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            if position < span.end || actual != span.content_hash {
                return Err(err("revision_unavailable", "Indexed source range changed"));
            }
            results[index] = Some(if let Some(body) = &whole {
                (body.clone(), witness.clone())
            } else {
                let mut witness = witness.clone();
                witness["range"] = json!({"start":span.start,"end":span.start+retained});
                (selected, witness)
            });
        }
    }
    Ok(results
        .into_iter()
        .map(|result| result.expect("each raw span was verified"))
        .collect())
}

const WINDOW: usize = 64 * 1024;

pub(super) struct Match {
    pub body: String,
    pub score: f64,
    pub witness: Value,
}

pub(super) fn keyword(db: &Connection, witness: &Value, query: &str) -> Result<Option<Match>> {
    if query.trim().is_empty() {
        return Ok(None);
    }
    // The query byte limit also bounds its Unicode lowercase expansion/overlap.
    let overlap = query.to_lowercase().chars().count().min(WINDOW - 1);
    let mut buffer = String::new();
    let mut start = 0usize;
    let mut best: Option<Match> = None;
    let mut inspect = |body: &str, start: usize| {
        if let Some(score) = search::keyword_score(body, query)
            && best.as_ref().is_none_or(|found| score > found.score)
        {
            let mut witness = witness.clone();
            witness["range"] = json!({"start":start,"end":start+body.chars().count()});
            best = Some(Match {
                body: body.into(),
                score,
                witness,
            });
        }
    };
    let mut raw_bytes = 0usize;
    let mut characters = 0usize;
    walk(db, witness, |part, bytes| {
        raw_bytes = raw_bytes.saturating_add(bytes);
        characters += part.chars().count();
        buffer.push_str(part);
        while buffer.chars().count() >= WINDOW {
            let end = buffer
                .char_indices()
                .nth(WINDOW)
                .map_or(buffer.len(), |(i, _)| i);
            inspect(&buffer[..end], start);
            let advance = WINDOW - overlap;
            let cut = buffer
                .char_indices()
                .nth(advance)
                .map_or(buffer.len(), |(i, _)| i);
            buffer.drain(..cut);
            start += advance;
        }
        Ok(())
    })?;
    if !buffer.is_empty() {
        inspect(&buffer, start);
    }
    // Preserve existing small full-body references, including hybrid fusion
    // with semantic hits. Large raw uses an explicitly bounded range instead.
    if let Some(found) = &mut best
        && raw_bytes <= MAX_INPUT
        && found.body.chars().count() == characters
    {
        found
            .witness
            .as_object_mut()
            .expect("raw witness")
            .remove("range");
    }
    Ok(best)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_spans_share_one_walk_per_witness_and_keep_ranked_ranges() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT,retention_generation INTEGER,saved_sequence INTEGER,output_encoding TEXT,state TEXT);
        CREATE TABLE app_meta(key TEXT,value TEXT);
        CREATE TABLE output_chunk(execution_id TEXT,sequence INTEGER,stream TEXT,bytes BLOB);
        INSERT INTO shell_execution VALUES('large',0,1,'UTF-8','exited'),('small',0,1,'UTF-8','exited');").unwrap();
        // The Unicode character crosses a 64KiB BLOB page boundary. More
        // than 100 selected spans must still read this legacy BLOB only once.
        let text = format!("{}あ{}", "x".repeat(WINDOW - 1), "tail界".repeat(WINDOW));
        let small = "別の出力";
        for (execution, text) in [("large", text.as_str()), ("small", small)] {
            db.execute(
                "INSERT INTO output_chunk VALUES(?1,0,'stdout',?2)",
                params![execution, text.as_bytes()],
            )
            .unwrap();
        }
        let large_witness =
            json!({"execution":"large","generation":0,"saved_sequence":1,"encoding":"UTF-8"});
        let small_witness =
            json!({"execution":"small","generation":0,"saved_sequence":1,"encoding":"UTF-8"});
        let characters: Vec<_> = text.chars().collect();
        let mut spans = Vec::new();
        for index in 0..200 {
            let (start, end) = match index % 4 {
                0 => (WINDOW - 2, WINDOW + 5),
                1 => (WINDOW, characters.len()),
                2 => (characters.len() - 30, characters.len()),
                _ => (WINDOW + 1, WINDOW + 1),
            };
            let selected: String = characters[start..end].iter().collect();
            spans.push(crate::index::SourceSpan {
                source_revision: 1,
                content_hash: hash(selected.as_bytes()),
                body_kind: "tool_raw".into(),
                start,
                end,
            });
        }
        let small_span = crate::index::SourceSpan {
            source_revision: 1,
            content_hash: hash("の".as_bytes()),
            body_kind: "tool_raw".into(),
            start: 1,
            end: 2,
        };
        let mut requests: Vec<_> = spans
            .iter()
            .map(|span| (large_witness.clone(), span))
            .collect();
        requests.insert(2, (small_witness.clone(), &small_span));
        let mut walks = 0usize;
        let mut scanned_bytes = 0usize;
        let found = semantic_batch_from(&requests, |witness, visit| {
            walks += 1;
            walk(&db, witness, |part, bytes| {
                scanned_bytes += bytes;
                visit(part, bytes)
            })
        })
        .unwrap();
        assert_eq!(walks, 2, "one walk per distinct witness, not per span");
        assert_eq!(scanned_bytes, text.len() + small.len());
        assert_eq!(found.len(), requests.len());
        for ((witness, span), (body, actual_witness)) in requests.iter().zip(&found) {
            if *witness == small_witness {
                assert_eq!(body, small);
                assert_eq!(
                    *actual_witness, small_witness,
                    "small body keeps full reference"
                );
            } else {
                let end = span.end.min(span.start + WINDOW);
                assert_eq!(
                    *body,
                    characters[span.start..end].iter().collect::<String>()
                );
                assert_eq!(
                    actual_witness["range"],
                    json!({"start":span.start,"end":end})
                );
                assert!(body.chars().count() <= WINDOW);
            }
        }
        assert_eq!(raw_text(&db, &found[0].1).unwrap(), found[0].0);
        assert_eq!(raw_text(&db, &found[1].1).unwrap(), found[1].0);
        // A corrupt hash outside the returned prefix fails the whole scope;
        // successful siblings must not mask source corruption.
        let changed = crate::index::SourceSpan {
            content_hash: hash(b"wrong"),
            ..spans[1].clone()
        };
        requests.push((large_witness.clone(), &changed));
        assert_eq!(
            semantic_batch(&db, &requests).unwrap_err().code,
            "revision_unavailable"
        );
        db.execute(
            "UPDATE shell_execution SET retention_generation=1 WHERE id='large'",
            [],
        )
        .unwrap();
        assert_eq!(
            semantic_batch(&db, &[(large_witness, &spans[0])])
                .unwrap_err()
                .code,
            "revision_unavailable"
        );
    }

    fn semantic(
        db: &Connection,
        witness: &Value,
        span: &crate::index::SourceSpan,
    ) -> Result<(String, Value)> {
        Ok(semantic_batch(db, &[(witness.clone(), span)])?.remove(0))
    }

    #[test]
    fn shared_decoder_preserves_interleaved_streams_eof_and_legacy_full_references() {
        let db = Connection::open_in_memory().unwrap();
        db.execute_batch("CREATE TABLE shell_execution(id TEXT,retention_generation INTEGER,saved_sequence INTEGER,output_encoding TEXT,state TEXT);
        CREATE TABLE app_meta(key TEXT,value TEXT);
        CREATE TABLE output_chunk(execution_id TEXT,sequence INTEGER,stream TEXT,bytes BLOB);
        INSERT INTO shell_execution VALUES('e',0,4,'UTF-8','exited');
        INSERT INTO output_chunk VALUES('e',0,'stdout',x'E381');
        INSERT INTO output_chunk VALUES('e',1,'stderr',x'42');
        INSERT INTO output_chunk VALUES('e',2,'stdout',x'82');
        INSERT INTO output_chunk VALUES('e',3,'stdout',x'E3');").unwrap();
        let witness = json!({"execution":"e","generation":0,"saved_sequence":4,"encoding":"UTF-8"});
        assert_eq!(raw_text(&db, &witness).unwrap(), "Bあ�");
        let found = keyword(&db, &witness, "bあ").unwrap().unwrap();
        assert_eq!(found.body, "Bあ�");
        assert_eq!(
            found.witness, witness,
            "small full-body reference remains identical"
        );
        assert_eq!(raw_text(&db, &found.witness).unwrap(), found.body);
        let span = crate::index::SourceSpan {
            source_revision: 1,
            content_hash: hash("あ�".as_bytes()),
            body_kind: "tool_raw".into(),
            start: 1,
            end: 3,
        };
        let (body, selected_witness) = semantic(&db, &witness, &span).unwrap();
        assert_eq!(body, "Bあ�", "small raw retains full-body references");
        assert_eq!(selected_witness, witness);
        let missing = crate::index::SourceSpan {
            end: 4,
            ..span.clone()
        };
        assert_eq!(
            semantic(&db, &witness, &missing).unwrap_err().code,
            "revision_unavailable"
        );
        let changed = crate::index::SourceSpan {
            content_hash: hash(b"wrong"),
            ..span
        };
        assert_eq!(
            semantic(&db, &witness, &changed).unwrap_err().code,
            "revision_unavailable"
        );
        assert!(keyword(&db, &witness, "").unwrap().is_none());
        let mut invalid = found.witness;
        invalid["range"] = json!({"start":0,"end":WINDOW+1});
        assert_eq!(
            raw_text(&db, &invalid).unwrap_err().code,
            "revision_unavailable"
        );
        db.execute("INSERT INTO app_meta VALUES('raw_tombstone.e','true')", [])
            .unwrap();
        assert_eq!(
            raw_text(&db, &witness).unwrap_err().code,
            "revision_unavailable"
        );
    }
}
