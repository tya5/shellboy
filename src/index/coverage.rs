//! Read-only current source coverage and range comparisons.
use super::*;

#[derive(Clone, Debug)]
pub(super) struct CoverageJob {
    pub(super) kind: String,
    pub(super) hash: String,
    pub(super) start: usize,
    pub(super) end: usize,
    pub(super) state: String,
    pub(super) witness: Option<Witness>,
    pub(super) chunk: bool,
}
pub(super) use crate::protocol::IndexCoverageCounts as CoverageCounts;
impl IndexService {
    pub(super) async fn coverage_jobs(
        &self,
        source: &Source,
        space: Option<String>,
    ) -> Result<Vec<CoverageJob>, ApiError> {
        let embedding_chunk_sql = "SELECT j.body_kind,j.source_hash,j.span_start,j.span_end,j.state,j.authorized_watermark,EXISTS(SELECT 1 FROM embedding_chunk c WHERE c.job_id=j.id AND c.space_id=j.space_id AND c.source_node_id=j.source_node_id AND c.source_revision=j.source_revision AND c.content_hash=j.source_hash AND c.span_start=j.span_start AND c.span_end=j.span_end) FROM embedding_job j WHERE j.space_id=?1 AND j.source_node_id=?2 ORDER BY j.span_start,j.span_end";
        let app_meta_sql = "SELECT value FROM app_meta WHERE key LIKE ?1 AND json_extract(value,'$.witness.node_id')=?2";
        let witness = source.witness.clone();
        let key = self.profile_key.clone();
        let rows = self
            .database
            .call(move |db| {
                if !witness_valid(db, &witness, "canonical")? {
                    return Ok(Vec::new());
                }
                let mut rows = Vec::new();
                if let Some(space) = space {
                    let mut statement = db.prepare(embedding_chunk_sql)?;
                    rows = statement
                        .query_map(params![space, witness.node_id], |r| {
                            Ok((
                                r.get::<_, String>(0)?,
                                r.get::<_, String>(1)?,
                                r.get::<_, i64>(2)? as usize,
                                r.get::<_, i64>(3)? as usize,
                                r.get::<_, String>(4)?,
                                r.get::<_, Option<String>>(5)?,
                                r.get::<_, bool>(6)?,
                            ))
                        })?
                        .collect::<Result<Vec<_>, _>>()?;
                } else if let Some(key) = key {
                    let mut statement = db.prepare(app_meta_sql)?;
                    for row in statement.query_map(
                        params![IndexKeys::new(&key).bootstrap_pattern(), witness.node_id],
                        |r| r.get::<_, String>(0),
                    )? {
                        let work: Work = decode_json(&row?)?;
                        if group_current(db, IndexKeys::new(&key), &work.group)? {
                            rows.push((
                                work.body_kind,
                                hash(work.segment.text.as_bytes()),
                                work.segment.start,
                                work.segment.end,
                                work.state,
                                Some(json!({ "witness":work.witness} ).to_string()),
                                false,
                            ));
                        }
                    }
                }
                Ok(rows)
            })
            .await
            .map_err(db_error)?;
        let mut jobs = rows
            .into_iter()
            .map(
                |(kind, hash, start, end, state, watermark, chunk)| CoverageJob {
                    kind,
                    hash,
                    start,
                    end,
                    state,
                    witness: watermark.as_deref().and_then(Witness::from_watermark),
                    chunk,
                },
            )
            .collect::<Vec<_>>();
        jobs.sort_by_key(|job| (job.start, job.end));
        Ok(jobs)
    }
}
/// Retain only job endpoints rather than a machine word per source character.
pub(super) fn job_offsets(
    text: &str,
    base: usize,
    kind: &str,
    jobs: &[CoverageJob],
) -> std::collections::HashMap<usize, usize> {
    let last = base + text.chars().count();
    let first = jobs.partition_point(|job| job.start < base);
    let mut needed = jobs[first..]
        .iter()
        .take_while(|job| job.start <= last)
        .filter(|job| job.kind == kind && job.end <= last)
        .flat_map(|job| [job.start, job.end])
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .peekable();
    let mut offsets = std::collections::HashMap::new();
    for (index, byte) in text
        .char_indices()
        .map(|(byte, _)| byte)
        .chain(std::iter::once(text.len()))
        .enumerate()
    {
        if needed.peek().is_some_and(|wanted| *wanted == base + index) {
            offsets.insert(base + index, byte);
            needed.next();
        }
        if needed.peek().is_none() {
            break;
        }
    }
    offsets
}
pub(super) fn covered_ranges(
    kind: &str,
    text: &str,
    base: usize,
    jobs: &[CoverageJob],
) -> Vec<(usize, usize)> {
    let offsets = job_offsets(text, base, kind, jobs);
    let mut ranges = Vec::new();
    let first = jobs.partition_point(|job| job.start < base);
    let last = base + text.chars().count();
    for job in jobs[first..].iter().take_while(|job| job.start <= last) {
        if job.kind != kind || job.state != "done" || !job.chunk || job.start >= job.end {
            continue;
        }
        let Some((start, end)) = offsets.get(&job.start).zip(offsets.get(&job.end)) else {
            continue;
        };
        if hash(&text.as_bytes()[*start..*end]) == job.hash {
            ranges.push((job.start, job.end));
        }
    }
    ranges.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (start, end) in ranges {
        if let Some(last) = merged.last_mut()
            && start <= last.1
        {
            last.1 = last.1.max(end);
        } else {
            merged.push((start, end));
        }
    }
    merged
}
pub(super) fn range_covered(ranges: &[(usize, usize)], segment: &Segment) -> bool {
    let index = ranges.partition_point(|(_, end)| *end < segment.end);
    ranges
        .get(index)
        .is_some_and(|(start, end)| *start <= segment.start && *end >= segment.end)
}

/// One classifier shared by canonical CLI and bounded saved observations.
pub(super) fn classify_bodies(
    current_witness: &Witness,
    raw_expected: bool,
    bodies: Vec<(String, String)>,
    jobs: Vec<CoverageJob>,
    split_chars: usize,
    coverage: &mut CoverageCounts,
) {
    let visible = bodies
        .iter()
        .map(|(kind, text)| {
            let offsets = job_offsets(text, 0, kind, &jobs);
            (kind.as_str(), (text.as_str(), offsets))
        })
        .collect::<std::collections::HashMap<_, _>>();
    let mut current = Vec::new();
    for job in jobs {
        let text_matches = (job.kind == "tool_raw" && current_witness.raw.is_some())
            || visible
                .get(job.kind.as_str())
                .is_some_and(|(text, offsets)| {
                    job.start < job.end
                        && offsets
                            .get(&job.start)
                            .zip(offsets.get(&job.end))
                            .is_some_and(|(start, end)| {
                                hash(&text.as_bytes()[*start..*end]) == job.hash
                            })
                });
        if !job
            .witness
            .as_ref()
            .is_some_and(|witness| witness.matches_body(current_witness, &job.kind))
            || !text_matches
        {
            coverage.stale_segments += 1;
        } else {
            current.push(job);
        }
    }
    let done = current
        .iter()
        .filter(|job| job.state == "done" && job.chunk)
        .count() as u64;
    let pending = current
        .iter()
        .filter(|job| matches!(job.state.as_str(), "pending" | "running"))
        .count() as u64;
    let failed = current
        .iter()
        .filter(|job| job.state == "failed" || (job.state == "done" && !job.chunk))
        .count() as u64;
    coverage.done_segments += done;
    coverage.pending_segments += pending;
    coverage.failed_segments += failed;
    let raw_uncertain = raw_expected;
    let mut expected = 0;
    let mut missing = 0;
    for (kind, body) in &bodies {
        let ranges = covered_ranges(kind, body, 0, &current);
        for segment in split_text(body, split_chars, 0) {
            expected += 1;
            if !range_covered(&ranges, &segment) {
                missing += 1;
            }
        }
    }
    if expected == 0 && !raw_uncertain {
        coverage.not_indexable_sources += 1;
        return;
    }
    coverage.eligible_sources += 1;
    if done == 0 {
        coverage.not_indexed_sources += 1;
    } else if missing > 0 || pending > 0 || failed > 0 {
        coverage.partially_indexed_sources += 1;
    } else if raw_uncertain {
        coverage.coverage_unverified_sources += 1;
    } else {
        coverage.fully_indexed_sources += 1;
    }
}
