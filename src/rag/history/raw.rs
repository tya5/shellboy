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
