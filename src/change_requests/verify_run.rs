//! `cr runs upload`: a `.verify/<app>/<run-id>/` run folder as a
//! `verify-run/1` document and the artifacts it names.
//!
//! The folder holds `scenarios.md`, the scenario set the run graded, copied
//! byte for byte, and `run.md`, whose `verify-run-notes/1` YAML front
//! matter carries the results.
//! The set digest is always the SHA-256 of `scenarios.md`, never a digest
//! `run.md` states.

use super::markdown::{self, scenario_id, Invalid, Problems};
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path, PathBuf};

/// The `format` of `run.md`'s front matter.
pub const NOTES_FORMAT: &str = "verify-run-notes/1";

/// A scenario's or feature's `result` in `run.md`.
pub const RESULTS: [&str; 3] = ["passed", "failed", "not_run"];

/// The lengths of a full commit SHA: SHA-1 and SHA-256 object names.
pub const COMMIT_LENGTHS: [usize; 2] = [40, 64];

pub const MAX_ARTIFACT_BYTES: u64 = 50 * 1024 * 1024;
pub const MAX_ARTIFACTS: usize = 200;
// The server's `verify-run/1` limits (goodflows_web `VerifyRuns.Document`).
pub const MAX_RESULTS: usize = 200;
pub const MAX_FEATURES: usize = 500;
pub const MAX_DRIFT: usize = 100;
pub const MAX_CHANGED_FILES: usize = 10_000;
pub const MAX_FEATURE_ID_BYTES: usize = 64;
pub const MAX_TEXT_BYTES: usize = 1_024;
pub const MAX_RESULT_TEXT_BYTES: usize = 4_096;
pub const MAX_PATH_BYTES: usize = 4_096;
pub const MAX_BRANCH_BYTES: usize = 256;

/// Namespace for run IDs derived from a run folder that names none.
const RUN_ID_NAMESPACE: uuid::Uuid = uuid::uuid!("0b6f1c2e-6a8d-5c3e-9f41-7d2a4e8b9c10");

pub struct Artifact {
    pub id: String,
    pub file: PathBuf,
    pub size: u64,
    pub sha256: String,
    pub content_type: &'static str,
}

pub struct Run {
    pub document: Value,
    pub artifacts: Vec<Artifact>,
}

pub fn read(folder: &Path) -> Result<Run> {
    let shown = folder.display().to_string();
    let mut problems = Problems::default();
    let set = problems.take(scenarios(folder, &shown));

    let notes_file = format!("{shown}/run.md");
    let fields = problems
        .take(
            std::fs::read_to_string(folder.join("run.md"))
                .with_context(|| format!("{shown} has no run.md"))
                .and_then(|notes| {
                    let front = markdown::split(&notes).ok_or_else(|| {
                        anyhow!("{notes_file} must start with YAML front matter between --- lines")
                    })?;
                    Ok((
                        markdown::refuse_anchors(front.yaml, &notes_file),
                        markdown::parse(front.yaml, &notes_file),
                    ))
                }),
        )
        .and_then(|(anchors, fields)| {
            problems.take(anchors);
            problems.take(fields)
        });
    let Some(fields) = fields else {
        return Err(Invalid(problems.0).into());
    };
    if fields["format"] != NOTES_FORMAT {
        problems.push(format!(
            "{notes_file}: the front matter needs format: \"{NOTES_FORMAT}\""
        ));
    }
    // The digest is always computed from scenarios.md; a stated one must agree.
    if let (Some(stated), Some((_, digest))) = (fields["set"]["sha256"].as_str(), &set) {
        if stated != digest {
            problems.push(format!("{notes_file}: set.sha256 is {stated}, but scenarios.md has SHA-256 {digest}; the run folder's copy of the set changed"));
        }
    }
    let field = |key: &str| &fields[key];

    let commit = required(&fields, "commit", &notes_file, &mut problems).and_then(|value| {
        let commit = value.as_str().filter(|c| {
            COMMIT_LENGTHS.contains(&c.len())
                && c.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        });
        if commit.is_none() {
            problems.push(format!(
                "{notes_file}: commit must be the full commit SHA in lowercase hex"
            ));
        }
        commit
    });
    let branch = text_or_null(field("branch"));
    if let Some(b) = branch.as_str() {
        if b.len() > MAX_BRANCH_BYTES || has_control(b) {
            problems.push(format!("{notes_file}: branch must be at most {MAX_BRANCH_BYTES} bytes without control characters"));
        }
    }
    let dirty = required(&fields, "dirty", &notes_file, &mut problems).and_then(|value| {
        let dirty = value.as_bool();
        if dirty.is_none() {
            problems.push(format!("{notes_file}: dirty must be true or false"));
        }
        dirty
    });

    let mut artifacts: Vec<Artifact> = Vec::new();
    let mut by_path: BTreeMap<PathBuf, String> = BTreeMap::new();
    let mut results = Vec::new();
    let mut seen = BTreeSet::new();
    let listed = required(&fields, "results", &notes_file, &mut problems).and_then(|value| {
        if value.as_array().is_none() {
            problems.push(format!("{notes_file}: results must be a list"));
        }
        value.as_array()
    });
    let names = folder_names(folder);
    for (index, result) in listed.into_iter().flatten().enumerate() {
        let Some(id) = scenario_id(&result["id"]) else {
            problems.push(format!(
                "{notes_file}: results[{index}] needs a scenario id"
            ));
            continue;
        };
        if !seen.insert(id.clone()) {
            problems.push(format!(
                "{notes_file}: scenario {id} has more than one result"
            ));
            continue;
        }
        let outcome = match result["result"].as_str() {
            Some(r) if RESULTS.contains(&r) => r,
            _ => {
                problems.push(format!(
                    "{notes_file}: scenario {id}: result must be passed, failed or not_run"
                ));
                ""
            }
        };
        let mut evidence = Vec::new();
        for path in result["evidence"].as_array().into_iter().flatten() {
            let Some(path) = path.as_str() else {
                problems.push(format!(
                    "{notes_file}: scenario {id}: evidence must list file paths"
                ));
                continue;
            };
            let Some(relative) = evidence_path(path, names.as_ref()) else {
                problems.push(format!("{notes_file}: scenario {id}: evidence {path:?} must be a path inside the run folder"));
                continue;
            };
            let artifact_id = match by_path.get(&relative) {
                Some(artifact_id) => artifact_id.clone(),
                None => {
                    let Some(artifact) = problems.take(artifact(folder, &relative, &artifacts))
                    else {
                        continue;
                    };
                    by_path.insert(relative, artifact.id.clone());
                    let artifact_id = artifact.id.clone();
                    artifacts.push(artifact);
                    artifact_id
                }
            };
            evidence.push(json!(artifact_id));
        }
        let failed_step = text_or_null(&result["failed_step"]);
        let reason = text_or_null(&result["reason"]);
        for (key, value) in [("failed_step", &failed_step), ("reason", &reason)] {
            if value
                .as_str()
                .is_some_and(|s| s.len() > MAX_RESULT_TEXT_BYTES)
            {
                problems.push(format!("{notes_file}: scenario {id}: {key} must be at most {MAX_RESULT_TEXT_BYTES} bytes"));
            }
        }
        results.push(json!({
            "id": id,
            "result": outcome,
            "failed_step": failed_step,
            "reason": reason,
            "evidence": evidence,
        }));
    }
    if results.len() > MAX_RESULTS {
        problems.push(format!(
            "{notes_file}: results holds {} entries; a run may hold at most {MAX_RESULTS}",
            results.len()
        ));
    }
    if let (Some((set, _)), Some(_)) = (&set, listed) {
        let missing: Vec<_> = set.ids.iter().filter(|id| !seen.contains(*id)).collect();
        let extra: Vec<_> = seen.iter().filter(|id| !set.ids.contains(id)).collect();
        if !missing.is_empty() || !extra.is_empty() {
            problems.push(format!(
                "{notes_file}: results must hold exactly the scenarios of scenarios.md (missing: {}; not in the set: {})",
                list(&missing),
                list(&extra)
            ));
        }
    }
    if artifacts.len() > MAX_ARTIFACTS {
        problems.push(format!(
            "{notes_file}: the evidence names {} files; a run may upload at most {MAX_ARTIFACTS}",
            artifacts.len()
        ));
    }

    let folder_name = folder
        .canonicalize()
        .ok()
        .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .unwrap_or_else(|| shown.clone());
    let run_id = match field("run_id") {
        Value::Null => Some(
            uuid::Uuid::new_v5(
                &RUN_ID_NAMESPACE,
                format!("{folder_name}\n{}", commit.unwrap_or_default()).as_bytes(),
            )
            .to_string(),
        ),
        value => {
            let id = value
                .as_str()
                .filter(|id| crate::fsutil::canonical_uuid(id))
                .map(str::to_owned);
            if id.is_none() {
                problems.push(format!(
                    "{notes_file}: run_id must be a lowercase canonical UUID"
                ));
            }
            id
        }
    };
    let started_at = required(&fields, "started_at", &notes_file, &mut problems)
        .and_then(|value| problems.take(time(value, "started_at", &notes_file)));
    let finished_at = required(&fields, "finished_at", &notes_file, &mut problems)
        .and_then(|value| problems.take(time(value, "finished_at", &notes_file)));
    let changed_files = changed_files(field("changed_files"), &notes_file, &mut problems);
    let features = features(field("features"), &notes_file, &mut problems);
    let drift = drift(field("drift"), &notes_file, &mut problems);
    problems.check()?;
    let (
        Some((set, digest)),
        Some(commit),
        Some(dirty),
        Some(run_id),
        Some(started_at),
        Some(finished_at),
    ) = (set, commit, dirty, run_id, started_at, finished_at)
    else {
        unreachable!("every missing value is a recorded problem");
    };
    let mut document = json!({
        "format": "verify-run/1",
        "run_id": run_id,
        "started_at": started_at,
        "finished_at": finished_at,
        "source": {
            "commit": commit,
            "branch": branch,
            "dirty": dirty,
            "changed_files": changed_files,
        },
        "scenario_set": {"digest": digest, "source": set.source},
        "results": results,
        "features": features,
        "drift": drift,
        "artifacts": artifacts.iter().map(|a| json!({
            "id": a.id,
            "path": a.file.to_string_lossy(),
            "content_type": a.content_type,
            "size": a.size,
            "sha256": a.sha256,
        })).collect::<Vec<_>>(),
    });
    for key in ["merge_result", "links", "workflow_run_id"] {
        if let Some(value) = fields.get(key) {
            document[key] = value.clone();
        }
    }
    Ok(Run {
        document,
        artifacts,
    })
}

/// The run folder's `scenarios.md` as a scenario set, with its SHA-256.
fn scenarios(folder: &Path, shown: &str) -> Result<(markdown::ScenarioSet, String)> {
    let bytes = std::fs::read(folder.join("scenarios.md")).with_context(|| {
        format!("{shown} has no scenarios.md, the copy of the scenario set the run graded")
    })?;
    let digest = markdown::sha256_hex(&bytes);
    let text =
        String::from_utf8(bytes).map_err(|_| anyhow!("{shown}/scenarios.md is not UTF-8"))?;
    let set = markdown::scenario_set(&text, &format!("{shown}/scenarios.md"))?;
    Ok((set, digest))
}

/// The front matter's `key`, or `None` with the missing key recorded.
fn required<'a>(
    fields: &'a Value,
    key: &str,
    file: &str,
    problems: &mut Problems,
) -> Option<&'a Value> {
    match &fields[key] {
        Value::Null => {
            problems.push(format!("{file}: the front matter needs {key}"));
            None
        }
        value => Some(value),
    }
}

fn list(items: &[&String]) -> String {
    if items.is_empty() {
        "none".to_owned()
    } else {
        items
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn text_or_null(value: &Value) -> Value {
    match value {
        Value::String(s) if !s.is_empty() => json!(s),
        _ => Value::Null,
    }
}

fn list_or_empty<'a>(
    value: &'a Value,
    key: &str,
    file: &str,
    max: usize,
    problems: &mut Problems,
) -> &'a [Value] {
    let items = match value {
        Value::Null => &[][..],
        Value::Array(items) => items.as_slice(),
        _ => {
            problems.push(format!("{file}: {key} must be a list"));
            &[][..]
        }
    };
    if items.len() > max {
        problems.push(format!(
            "{file}: {key} holds {} entries; a run may hold at most {max}",
            items.len()
        ));
    }
    items
}

/// A Unicode control character, as the server's `@control` matches them.
fn has_control(s: &str) -> bool {
    s.chars()
        .any(|c| matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}'))
}

/// Each feature as exactly `{id, entry_point, result}`; other keys are dropped.
fn features(value: &Value, file: &str, problems: &mut Problems) -> Value {
    let mut features = Vec::new();
    for (index, feature) in list_or_empty(value, "features", file, MAX_FEATURES, problems)
        .iter()
        .enumerate()
    {
        if !feature.is_object() {
            problems.push(format!(
                "{file}: features[{index}] must be a mapping with id, entry_point and result"
            ));
            continue;
        }
        let Some(id) = feature["id"].as_str() else {
            problems.push(format!("{file}: features[{index}] needs a string id"));
            continue;
        };
        if id.trim().is_empty() || id.len() > MAX_FEATURE_ID_BYTES || has_control(id) {
            problems.push(format!("{file}: features[{index}]: id must be 1 to {MAX_FEATURE_ID_BYTES} bytes, not blank, without control characters"));
        }
        let entry_point = match &feature["entry_point"] {
            Value::Null => Value::Null,
            Value::String(s) if s.len() <= MAX_TEXT_BYTES => json!(s),
            Value::String(_) => {
                problems.push(format!(
                    "{file}: features[{index}] ({id}): entry_point must be at most {MAX_TEXT_BYTES} bytes"
                ));
                Value::Null
            }
            _ => {
                problems.push(format!(
                    "{file}: features[{index}] ({id}): entry_point must be a string"
                ));
                Value::Null
            }
        };
        let result = match feature["result"].as_str() {
            Some(r) if RESULTS.contains(&r) => r,
            _ => {
                problems.push(format!(
                    "{file}: features[{index}] ({id}): result must be passed, failed or not_run"
                ));
                ""
            }
        };
        features.push(json!({"id": id, "entry_point": entry_point, "result": result}));
    }
    Value::Array(features)
}

fn drift(value: &Value, file: &str, problems: &mut Problems) -> Value {
    let mut drift = Vec::new();
    for (index, item) in list_or_empty(value, "drift", file, MAX_DRIFT, problems)
        .iter()
        .enumerate()
    {
        match item.as_str() {
            Some(s) if s.len() <= MAX_TEXT_BYTES => drift.push(json!(s)),
            _ => problems.push(format!(
                "{file}: drift[{index}] must be a string of at most {MAX_TEXT_BYTES} bytes"
            )),
        }
    }
    Value::Array(drift)
}

fn changed_files(value: &Value, file: &str, problems: &mut Problems) -> Value {
    let mut paths = Vec::new();
    for (index, item) in list_or_empty(value, "changed_files", file, MAX_CHANGED_FILES, problems)
        .iter()
        .enumerate()
    {
        match item.as_str() {
            Some(p)
                if (1..=MAX_PATH_BYTES).contains(&p.len())
                    && !p.starts_with('/')
                    && !has_control(p) =>
            {
                paths.push(json!(p))
            }
            _ => problems.push(format!("{file}: changed_files[{index}] must be a relative path of 1 to {MAX_PATH_BYTES} bytes without control characters")),
        }
    }
    Value::Array(paths)
}

/// An RFC 3339 time as written; YAML may also hold it unquoted.
fn time(value: &Value, key: &str, file: &str) -> Result<Value> {
    match value.as_str() {
        Some(t) if rfc3339(t) => Ok(json!(t)),
        _ => bail!("{file}: {key} must be an RFC 3339 time such as 2026-11-19T14:30:00Z"),
    }
}

/// The server's shape: `YYYY-MM-DDTHH:MM:SS`, up to 9 fraction digits,
/// then `Z` or `±HH:MM`.
fn rfc3339(t: &str) -> bool {
    fn digits(s: &[u8]) -> bool {
        s.iter().all(u8::is_ascii_digit)
    }
    let b = t.as_bytes();
    if b.len() < 20
        || !(digits(&b[0..4])
            && b[4] == b'-'
            && digits(&b[5..7])
            && b[7] == b'-'
            && digits(&b[8..10])
            && b[10] == b'T'
            && digits(&b[11..13])
            && b[13] == b':'
            && digits(&b[14..16])
            && b[16] == b':'
            && digits(&b[17..19]))
    {
        return false;
    }
    let mut rest = &b[19..];
    if rest.first() == Some(&b'.') {
        let n = rest[1..].iter().take_while(|c| c.is_ascii_digit()).count();
        if !(1..=9).contains(&n) {
            return false;
        }
        rest = &rest[1 + n..];
    }
    rest == b"Z"
        || (rest.len() == 6
            && matches!(rest[0], b'+' | b'-')
            && digits(&rest[1..3])
            && rest[3] == b':'
            && digits(&rest[4..6]))
}

/// The run folder's last two names, `<app>` and `<run-id>`, which a
/// `.verify/<app>/<run-id>/` evidence prefix must match.
fn folder_names(folder: &Path) -> Option<[std::ffi::OsString; 2]> {
    let folder = folder.canonicalize().ok()?;
    let run = folder.file_name()?.to_owned();
    let app = folder.parent()?.file_name()?.to_owned();
    Some([app, run])
}

/// A relative path inside the run folder. Evidence may name the file as
/// `.verify/<app>/<run-id>/<file>` when `<app>/<run-id>` is this folder
/// (`names`), or relative to the folder; only the part after the run
/// folder counts.
fn evidence_path(path: &str, names: Option<&[std::ffi::OsString; 2]>) -> Option<PathBuf> {
    let path = Path::new(path);
    let components: Vec<_> = path.components().collect();
    let relative: PathBuf = match components.as_slice() {
        [Component::Normal(first), Component::Normal(app), Component::Normal(run), _, ..]
            if *first == ".verify" =>
        {
            let [ours_app, ours_run] = names?;
            if app != ours_app || run != ours_run {
                return None;
            }
            components[3..].iter().collect()
        }
        _ => path.to_path_buf(),
    };
    let fine = relative
        .components()
        .all(|c| matches!(c, Component::Normal(_)));
    (fine && relative.components().next().is_some()).then_some(relative)
}

fn artifact(folder: &Path, relative: &Path, existing: &[Artifact]) -> Result<Artifact> {
    let file = folder.join(relative);
    let shown = file.display();
    let metadata = std::fs::symlink_metadata(&file)
        .with_context(|| format!("evidence file {shown} does not exist"))?;
    if !metadata.is_file() {
        bail!("evidence {shown} is not a regular file");
    }
    let size = metadata.len();
    if size == 0 || size > MAX_ARTIFACT_BYTES {
        bail!("evidence file {shown} must hold 1 byte to 50 MiB");
    }
    let name = relative.to_string_lossy();
    if name.len() > MAX_TEXT_BYTES || has_control(&name) {
        bail!("evidence {shown}: the path inside the run folder must be at most {MAX_TEXT_BYTES} bytes without control characters");
    }
    let bytes = std::fs::read(&file).with_context(|| format!("could not read {shown}"))?;
    let mut base: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '-'
            }
        })
        .collect();
    if !base.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        base.insert(0, 'a');
    }
    base.truncate(60);
    let mut id = base.clone();
    let mut n = 2;
    while existing.iter().any(|a| a.id == id) {
        id = format!("{base}-{n}");
        n += 1;
    }
    Ok(Artifact {
        id,
        file: relative.to_path_buf(),
        size,
        sha256: markdown::sha256_hex(&bytes),
        content_type: content_type(relative),
    })
}

fn content_type(path: &Path) -> &'static str {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    match extension.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        "webm" => "video/webm",
        "mp4" => "video/mp4",
        "json" => "application/json",
        "html" | "htm" => "text/html",
        "md" => "text/markdown",
        "txt" | "log" => "text/plain",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SET: &str = "---\nformat: qa-scenarios/2\nsource: intent\nchange: add-login\nspecs: uncommitted\nreview: null\nscenarios:\n  - {id: 1, name: Sign in, priority: must}\n---\n# QA: add-login\n\n## Scenarios\n\n### 1. Sign in (Must)\n\n1. Click **Sign in**.\n";

    /// Reads a run folder whose `run.md` front matter ends with `extra`.
    fn read_with(extra: &str) -> Result<Value> {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("scenarios.md"), SET).unwrap();
        std::fs::write(
            dir.path().join("run.md"),
            format!("---\nformat: \"verify-run-notes/1\"\ncommit: {}\ndirty: false\nstarted_at: 2026-11-19T14:30:00Z\nfinished_at: 2026-11-19T14:35:00Z\nresults:\n  - {{id: 1, result: passed}}\n{extra}---\n", "a".repeat(40)),
        )
        .unwrap();
        read(dir.path()).map(|run| run.document)
    }

    #[test]
    fn a_feature_without_entry_point_gets_null() {
        let document = read_with("features:\n  - {id: sign-in, result: passed}\n").unwrap();
        assert_eq!(
            document["features"],
            json!([{"id": "sign-in", "entry_point": null, "result": "passed"}])
        );
    }

    #[test]
    fn a_feature_keeps_only_id_entry_point_and_result() {
        let document = read_with(
            "features:\n  - {id: sign-in, entry_point: /login, result: failed, notes: flaky, owner: web}\n",
        )
        .unwrap();
        assert_eq!(
            document["features"],
            json!([{"id": "sign-in", "entry_point": "/login", "result": "failed"}])
        );
    }

    #[test]
    fn a_feature_with_a_bad_result_or_no_id_is_refused() {
        let error = read_with("features:\n  - {id: sign-in, result: skipped}\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains(
                "run.md: features[0] (sign-in): result must be passed, failed or not_run"
            ),
            "{error}"
        );
        let error = read_with("features:\n  - {result: passed}\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("run.md: features[0] needs a string id"),
            "{error}"
        );
    }

    #[test]
    fn run_notes_refuse_anchors_and_aliases() {
        let error = read_with("drift: [&d \"R&D\", *d]\n")
            .unwrap_err()
            .to_string();
        assert!(
            error.contains("run.md: YAML anchors and aliases are not supported"),
            "{error}"
        );
    }

    #[test]
    fn drift_and_changed_files_match_the_server_limits() {
        let error = read_with(&format!("drift: [{}]\n", "x".repeat(1_025)))
            .unwrap_err()
            .to_string();
        assert!(error.contains("drift[0]"), "{error}");
        let error = read_with("changed_files: [src/a.rs, /etc/passwd]\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("changed_files[1]"), "{error}");
        let document = read_with("changed_files: [src/a.rs]\ndrift: [renamed button]\n").unwrap();
        assert_eq!(document["source"]["changed_files"], json!(["src/a.rs"]));
        assert_eq!(document["drift"], json!(["renamed button"]));
    }

    #[test]
    fn times_follow_the_server_shape() {
        for good in ["2026-11-19T14:30:00Z", "2026-11-19T14:30:00.123+02:00"] {
            assert!(rfc3339(good), "{good}");
        }
        for bad in [
            "2026-11-19T14:30:00",
            "2026-11-19T14:30:00.Z",
            "2026-11-19 14:30:00Z",
            "2026-11-19T14:30:00+0200",
        ] {
            assert!(!rfc3339(bad), "{bad}");
        }
    }

    #[test]
    fn evidence_paths_stay_inside_the_run_folder() {
        let names = [
            std::ffi::OsString::from("web"),
            std::ffi::OsString::from("20261119T143000Z-a1b2c3d"),
        ];
        assert_eq!(
            evidence_path("01.png", Some(&names)),
            Some(PathBuf::from("01.png"))
        );
        assert_eq!(
            evidence_path(
                ".verify/web/20261119T143000Z-a1b2c3d/shots/01.png",
                Some(&names)
            ),
            Some(PathBuf::from("shots/01.png"))
        );
        for bad in ["../secret", "/etc/passwd", "a/../../b", "", "."] {
            assert_eq!(evidence_path(bad, Some(&names)), None, "{bad}");
        }
        // A prefix naming another app or run points outside this folder.
        for other in [
            ".verify/web/run-9/shots/01.png",
            ".verify/api/20261119T143000Z-a1b2c3d/shots/01.png",
        ] {
            assert_eq!(evidence_path(other, Some(&names)), None, "{other}");
        }
    }
}
