//! Read-only summary of the owner's bounded startup-stage diagnostic log.
use crate::{
    config::{Config, PathEnvironment},
    protocol::PROTOCOL_VERSION,
};
use clap::Args;
use semver::Version;
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    cmp::Ordering,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
};

const MAX_READ_BYTES: usize = 16 * 1024 * 1024;
const MAX_LOG_FILES: usize = 64;
const MAX_LAST: usize = 20;

#[derive(Debug, Clone, Args)]
pub struct StartupOptions {
    /// Number of recent launches to return (1..=20; default 5).
    #[arg(long, default_value_t = 5, value_parser = parse_last)]
    pub last: usize,
}

fn parse_last(value: &str) -> Result<usize, String> {
    let value = value
        .parse::<usize>()
        .map_err(|_| "expected a number from 1 to 20".to_owned())?;
    if (1..=MAX_LAST).contains(&value) {
        Ok(value)
    } else {
        Err("expected a number from 1 to 20".to_owned())
    }
}

#[derive(Debug, Serialize)]
struct Stage {
    at: String,
    stage: String,
    elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_version: Option<String>,
}
#[derive(Debug, Default, Serialize)]
struct Launch {
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
    stages: Vec<Stage>,
    total_elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_version: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    app_version: Option<String>,
    version_increased: Option<bool>,
    incomplete: bool,
}
#[derive(Debug)]
struct Record {
    at: String,
    stage: String,
    elapsed_ms: u64,
    migration_version: Option<u32>,
    app_version: Option<String>,
}

/// Resolve the regular CLI config/data-dir precedence and read logs off the caller thread.
pub async fn execute(
    options: StartupOptions,
    config: Option<PathBuf>,
    data_dir: Option<PathBuf>,
    cwd: &Path,
) -> Value {
    let cwd = cwd.to_path_buf();
    let result = tokio::task::spawn_blocking(move || {
        let loaded = Config::load_with_overrides(
            config.as_deref(),
            data_dir.as_deref(),
            &cwd,
            &PathEnvironment::current(),
        )
        .map_err(|_| ("config_invalid", "Cannot resolve startup data directory"))?;
        summarize(&loaded.data_dir, options.last)
            .map_err(|_| ("io_failed", "Cannot read startup diagnostics"))
    })
    .await
    .unwrap_or(Err(("io_failed", "Startup inspection worker failed")));
    match result {
        Ok(result) => json!({"protocol_version":PROTOCOL_VERSION,"ok":true,"result":result}),
        Err((code, message)) => {
            json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":{"code":code,"message":message}})
        }
    }
}

fn summarize(data_dir: &Path, last: usize) -> std::io::Result<Value> {
    let directory = data_dir.join("logs");
    let mut budget = MAX_READ_BYTES;
    let scan_limit = last.saturating_add(1);
    let mut hit_read_limit = false;
    let mut invalid_line_seen = false;
    let mut current_launch: Vec<Record> = Vec::new();
    let mut launches_newest_first = Vec::new();

    for index in 0..MAX_LOG_FILES {
        if budget == 0 {
            hit_read_limit = true;
            break;
        }
        let path = if index == 0 {
            directory.join("shellboy.log")
        } else {
            directory.join(format!("shellboy.log.{index}"))
        };
        let (lines, used, was_truncated) = match read_tail_lines(&path, budget) {
            Ok(Some(value)) => value,
            Ok(None) => continue,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        budget = budget.saturating_sub(used);
        hit_read_limit |= was_truncated;
        for line in lines {
            match parse_record(line) {
                Ok(Some(record)) => {
                    if record.stage == "configuration" {
                        current_launch.push(record);
                        current_launch.reverse();
                        launches_newest_first
                            .push(build_launch(std::mem::take(&mut current_launch)));
                        if launches_newest_first.len() >= scan_limit {
                            break;
                        }
                    } else {
                        current_launch.push(record);
                    }
                }
                Ok(None) => {}
                Err(()) => invalid_line_seen = true,
            }
        }
        if launches_newest_first.len() >= scan_limit {
            break;
        }
    }

    if !current_launch.is_empty() && launches_newest_first.len() < scan_limit {
        current_launch.reverse();
        launches_newest_first.push(build_launch(std::mem::take(&mut current_launch)));
    }
    launches_newest_first.reverse();
    for index in 1..launches_newest_first.len() {
        let previous = launches_newest_first[index - 1].app_version.as_deref();
        let current = launches_newest_first[index].app_version.as_deref();
        launches_newest_first[index].version_increased = previous
            .zip(current)
            .and_then(|(a, b)| version_increased(a, b));
    }
    if launches_newest_first.len() > last {
        launches_newest_first.remove(0);
    }
    let records_may_be_missing = true;
    Ok(json!({
        "launches": launches_newest_first,
        "records_may_be_missing": records_may_be_missing,
        "read_limit_reached": hit_read_limit,
        "invalid_lines_seen": invalid_line_seen,
    }))
}

type TailRead = (Vec<Vec<u8>>, usize, bool);

/// Read at most `budget` bytes from the end, returning lines newest-first.
fn read_tail_lines(path: &Path, budget: usize) -> std::io::Result<Option<TailRead>> {
    let mut file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let length = usize::try_from(file.metadata()?.len()).unwrap_or(usize::MAX);
    let count = length.min(budget);
    if count == 0 {
        return Ok(Some((Vec::new(), 0, false)));
    }
    let truncated = count < length;
    file.seek(SeekFrom::End(-(count as i64)))?;
    let mut bytes = Vec::with_capacity(count);
    file.take(count as u64).read_to_end(&mut bytes)?;
    if truncated {
        if let Some(newline) = bytes.iter().position(|byte| *byte == b'\n') {
            bytes.drain(..=newline);
        } else {
            return Ok(Some((Vec::new(), count, true)));
        }
    }
    let lines = bytes
        .split(|byte| *byte == b'\n')
        .rev()
        .filter(|line| !line.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    Ok(Some((lines, count, truncated)))
}

/// Parse only an allowlisted timestamp, stage, duration, and migration version.
fn parse_record(line: Vec<u8>) -> Result<Option<Record>, ()> {
    let value: Value = serde_json::from_slice(&line).map_err(|_| ())?;
    let Some(stage_value) = value.get("startup_stage") else {
        return if value.get("event").and_then(Value::as_str) == Some("startup_stage") {
            Err(())
        } else {
            Ok(None)
        };
    };
    let at = value.get("at").and_then(Value::as_str).ok_or(())?;
    chrono::DateTime::parse_from_rfc3339(at).map_err(|_| ())?;
    let details = stage_value.as_object().ok_or(())?;
    let stage = details.get("stage").and_then(Value::as_str).ok_or(())?;
    if !matches!(
        stage,
        "configuration"
            | "owner_lock"
            | "database"
            | "database_pragmas"
            | "database_migration"
            | "database_validate"
            | "database_foreign_keys"
            | "database_quick_check"
            | "interrupted_work"
            | "control"
            | "endpoint"
            | "closing"
    ) {
        return Err(());
    }
    let elapsed_ms = details
        .get("elapsed_ms")
        .and_then(Value::as_u64)
        .ok_or(())?;
    let app_version = details
        .get("app_version")
        .and_then(Value::as_str)
        .and_then(validated_app_version);
    let migration_version = details
        .get("migration_version")
        .and_then(Value::as_u64)
        .and_then(|value| u32::try_from(value).ok());
    Ok(Some(Record {
        at: at.to_owned(),
        stage: stage.to_owned(),
        elapsed_ms,
        migration_version,
        app_version,
    }))
}

fn validated_app_version(value: &str) -> Option<String> {
    if value.len() > 64 || !value.is_ascii() {
        return None;
    }
    Version::parse(value).ok()?;
    Some(value.to_owned())
}

fn version_increased(previous: &str, current: &str) -> Option<bool> {
    let previous = Version::parse(previous).ok()?;
    let current = Version::parse(current).ok()?;
    Some(current.cmp_precedence(&previous) == Ordering::Greater)
}

fn build_launch(records: Vec<Record>) -> Launch {
    let started_at = records
        .iter()
        .find(|record| record.stage == "configuration")
        .map(|record| record.at.clone());
    let migration_version = records
        .iter()
        .filter_map(|record| record.migration_version)
        .max();
    let app_version = records.iter().find_map(|record| record.app_version.clone());
    let total_elapsed_ms = records
        .iter()
        .fold(0u64, |sum, record| sum.saturating_add(record.elapsed_ms));
    let incomplete = [
        "configuration",
        "owner_lock",
        "database",
        "interrupted_work",
        "control",
        "endpoint",
    ]
    .iter()
    .any(|required| !records.iter().any(|record| record.stage == *required));
    let stages = records
        .into_iter()
        .map(|record| Stage {
            at: record.at,
            stage: record.stage,
            elapsed_ms: record.elapsed_ms,
            migration_version: record.migration_version,
            app_version: record.app_version,
        })
        .collect();
    Launch {
        started_at,
        stages,
        total_elapsed_ms,
        migration_version,
        app_version,
        version_increased: None,
        incomplete,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    fn line(at: &str, stage: &str, elapsed: u64, migration_version: Option<u32>) -> String {
        line_with_app_version(at, stage, elapsed, migration_version, "0.7.2")
    }

    fn line_with_app_version(
        at: &str,
        stage: &str,
        elapsed: u64,
        migration_version: Option<u32>,
        app_version: &str,
    ) -> String {
        let mut row = json!({"at":at,"event":"startup_stage","startup_stage":{"stage":stage,"elapsed_ms":elapsed,"app_version":app_version}});
        if let Some(version) = migration_version {
            row["startup_stage"]["migration_version"] = json!(version);
        }
        row.to_string()
    }

    #[test]
    fn groups_launches_detects_migration_and_never_surfaces_other_fields() {
        let root = tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let rows = [
            line("2026-01-01T00:00:00Z", "configuration", 2, None).replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:00Z", "owner_lock", 0, None).replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:00Z", "database", 0, None).replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:01Z", "database_migration", 9, Some(24))
                .replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:01Z", "interrupted_work", 0, None).replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:01Z", "control", 0, None).replace("0.7.2", "0.7.1"),
            line("2026-01-01T00:00:02Z", "endpoint", 1, None).replace("0.7.2", "0.7.1"),
            line("2026-02-01T00:00:00Z", "configuration", 3, None),
            line("2026-02-01T00:00:00Z", "owner_lock", 0, None),
            line("2026-02-01T00:00:00Z", "database", 0, None),
            line("2026-02-01T00:00:01Z", "database_migration", 12, Some(25)),
            line("2026-02-01T00:00:01Z", "interrupted_work", 0, None),
            line("2026-02-01T00:00:01Z", "control", 0, None),
            line("2026-02-01T00:00:02Z", "endpoint", 2, None),
        ];
        fs::write(logs.join("shellboy.log"), rows.join("\n") + "\n").unwrap();
        let result = summarize(root.path(), 5).unwrap();
        assert_eq!(result["launches"].as_array().unwrap().len(), 2);
        assert_eq!(result["launches"][0]["migration_version"], 24);
        assert_eq!(result["launches"][1]["migration_version"], 25);
        assert_eq!(result["launches"][0]["version_increased"], Value::Null);
        assert_eq!(result["launches"][1]["version_increased"], true);
        let newest_only = summarize(root.path(), 1).unwrap();
        assert_eq!(newest_only["launches"].as_array().unwrap().len(), 1);
        assert_eq!(newest_only["launches"][0]["version_increased"], true);
        assert_eq!(result["launches"][0]["total_elapsed_ms"], 12);
        assert_eq!(result["launches"][1]["incomplete"], false);
        assert!(result["launches"][0].get("ids").is_none());
    }

    #[test]
    fn compares_prereleases_and_ignores_build_metadata_per_semver() {
        let root = tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        let rows = [
            // Input: 0.7.2-rc.1 -> 0.7.2; final release is a version increase.
            line_with_app_version("2026-01-01T00:00:00Z", "configuration", 1, None, "0.7.2-rc.1"),
            line_with_app_version("2026-01-01T00:00:01Z", "configuration", 1, None, "0.7.2"),
            // Reverse direction is not an increase.
            line_with_app_version("2026-01-01T00:00:02Z", "configuration", 1, None, "0.7.2-rc.1"),
            // Build metadata does not affect SemVer precedence.
            line_with_app_version("2026-01-01T00:00:03Z", "configuration", 1, None, "0.7.2+ci.1"),
            line_with_app_version("2026-01-01T00:00:04Z", "configuration", 1, None, "0.7.2+ci.2"),
        ];
        fs::write(logs.join("shellboy.log"), rows.join("\n") + "\n").unwrap();

        let launches = summarize(root.path(), 5).unwrap();
        let launches = launches["launches"].as_array().unwrap();
        assert_eq!(launches[1]["version_increased"], true);
        assert_eq!(launches[2]["version_increased"], false);
        assert_eq!(launches[4]["version_increased"], false);
    }

    #[test]
    fn spans_rotated_files_and_marks_missing_and_corrupt_data() {
        let root = tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("shellboy.log.1"),
            line("2026-03-01T00:00:00Z", "configuration", 4, None) + "\n{bad json\n",
        )
        .unwrap();
        fs::write(
            logs.join("shellboy.log"),
            line("2026-03-01T00:00:01Z", "endpoint", 5, None) + "\n",
        )
        .unwrap();
        let result = summarize(root.path(), 5).unwrap();
        assert_eq!(result["launches"].as_array().unwrap().len(), 1);
        assert_eq!(result["launches"][0]["total_elapsed_ms"], 9);
        assert_eq!(result["invalid_lines_seen"], true);
        assert_eq!(result["records_may_be_missing"], true);
    }

    #[test]
    fn incomplete_launch_is_not_completed_with_zero_stages_and_missing_log_is_empty() {
        let root = tempdir().unwrap();
        let logs = root.path().join("logs");
        fs::create_dir_all(&logs).unwrap();
        fs::write(
            logs.join("shellboy.log"),
            line("2026-04-01T00:00:00Z", "configuration", 7, None)
                + "\n"
                + &line("2026-04-01T00:00:01Z", "endpoint", 1, None)
                + "\n",
        )
        .unwrap();
        let result = summarize(root.path(), 5).unwrap();
        assert_eq!(result["launches"][0]["total_elapsed_ms"], 8);
        assert_eq!(result["launches"][0]["incomplete"], true);
        fs::remove_file(logs.join("shellboy.log")).unwrap();
        assert_eq!(
            summarize(root.path(), 5).unwrap()["launches"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn startup_stage_field_is_authoritative_and_malformed_startup_event_is_flagged() {
        let without_event = json!({"at":"2026-01-01T00:00:00Z","startup_stage":{"stage":"configuration","elapsed_ms":1}});
        assert!(
            parse_record(serde_json::to_vec(&without_event).unwrap())
                .unwrap()
                .is_some()
        );
        let malformed = json!({"event":"startup_stage","at":"2026-01-01T00:00:00Z"});
        assert!(parse_record(serde_json::to_vec(&malformed).unwrap()).is_err());
    }

    #[test]
    fn malformed_application_version_is_never_echoed() {
        assert!(validated_app_version("0.7.2").is_some());
        assert!(validated_app_version("0.7.2-beta.1").is_some());
        assert!(validated_app_version("secret-header-value").is_none());
    }

    #[test]
    fn bounded_tail_marks_cutoff_and_discards_partial_first_line() {
        let root = tempdir().unwrap();
        let file = root.path().join("log");
        fs::write(&file, "old-record\nnew-record\n").unwrap();
        let (lines, used, truncated) = read_tail_lines(&file, 12).unwrap().unwrap();
        assert_eq!(used, 12);
        assert!(truncated);
        assert_eq!(lines[0], b"new-record");
    }
}
