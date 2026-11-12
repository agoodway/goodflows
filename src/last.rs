//! The last-run record: every local run's `summary` events, kept beside the
//! config so `goodflows last` can show them again and `goodflows run
//! --failed` can rerun what did not pass. Unlike the opt-in history file it
//! is always written, it keeps only the latest run, and it holds the failure
//! output tails that history never stores.

use crate::config::ConfigError;
use crate::output::Output;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The runner's skip reason for a job whose dependency did not pass.
const DEPENDENCY_SKIP: &str = "dependency did not pass";

/// `.goodflows/last/<config file name>.json` beside the config file.
pub fn path(config_file: &Path) -> PathBuf {
    let mut name = config_file.file_name().unwrap_or_default().to_os_string();
    name.push(".json");
    config_file
        .parent()
        .unwrap_or(Path::new("."))
        .join(".goodflows/last")
        .join(name)
}

#[derive(Deserialize)]
struct Record {
    /// The explicit `--config` the run was given, which its rerun hint keeps.
    #[serde(default)]
    config: Option<String>,
    /// Every invocation's summary in the order the run emitted them: called
    /// workflows first, the root last.
    summaries: Vec<Summary>,
}

#[derive(Serialize, Deserialize)]
struct Summary {
    workflow: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    environment: Option<String>,
    /// Set when the run calls workflows, so its events carry invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation: Option<Invocation>,
    /// A signal arrived before the summary.
    interrupted: bool,
    /// The `summary` event before `Output` adds the context above.
    event: Value,
}

#[derive(Serialize, Deserialize)]
struct Invocation {
    id: String,
    /// The parent run id and caller job of a called workflow.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    parent: Option<(String, String)>,
}

/// Collects one local run's summaries; the root invocation writes them.
pub struct Recorder {
    paths: Vec<PathBuf>,
    summaries: Mutex<Vec<Summary>>,
}

impl Recorder {
    pub fn new(config_file: &Path) -> Self {
        Self {
            paths: vec![path(config_file)],
            summaries: Mutex::default(),
        }
    }

    /// Also write the record to `path`: a detached run keeps its own copy,
    /// which a later run cannot replace.
    pub fn also(mut self, path: PathBuf) -> Self {
        self.paths.push(path);
        self
    }

    /// Keep one invocation's `summary` event with the context `Output` adds
    /// to it: `invocation` is its id and any parent run and caller job.
    pub fn add(
        &self,
        workflow: &str,
        environment: Option<&str>,
        invocation: Option<(&str, Option<(&str, &str)>)>,
        interrupted: bool,
        event: Value,
    ) {
        let summary = Summary {
            workflow: workflow.into(),
            environment: environment.map(Into::into),
            invocation: invocation.map(|(id, parent)| Invocation {
                id: id.into(),
                parent: parent.map(|(run, job)| (run.into(), job.into())),
            }),
            interrupted,
            event,
        };
        self.summaries
            .lock()
            .expect("last run mutex poisoned")
            .push(summary);
    }

    /// Replace the record with the summaries kept so far. The root calls
    /// this after adding its own summary, which is last.
    pub fn write(&self, config: Option<&str>) -> Result<()> {
        let summaries = self.summaries.lock().expect("last run mutex poisoned");
        let mut bytes = serde_json::to_vec(&json!({
            "schema_version": 1,
            "config": config,
            "summaries": &*summaries,
        }))?;
        bytes.push(b'\n');
        // Every path is written even when an earlier one fails; the first
        // failure is returned.
        let write = |path: &PathBuf| -> Result<()> {
            let dir = path.parent().context("last run path has no parent")?;
            crate::fsutil::create_state_dir(dir)
                .with_context(|| format!("create {}", dir.display()))?;
            crate::fsutil::write_atomic(path, &bytes, true)
                .with_context(|| format!("write {}", path.display()))
        };
        let results: Vec<_> = self.paths.iter().map(write).collect();
        results.into_iter().collect()
    }
}

/// The record for `config_file`, with at least the root summary.
fn read(config_file: &Path) -> Result<Record> {
    let path = path(config_file);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let mut error = ConfigError::new(
                "no_last_run",
                format!(
                    "no previous local run of {} (looked for {})",
                    config_file.display(),
                    path.display()
                ),
            );
            error.hint = Some("run `goodflows run` first".into());
            return Err(error.into());
        }
        Err(error) => {
            return Err(error).with_context(|| format!("read {}", path.display()));
        }
    };
    parse(&path, &bytes)
}

fn parse(path: &Path, bytes: &[u8]) -> Result<Record> {
    let record: Record =
        serde_json::from_slice(bytes).with_context(|| format!("parse {}", path.display()))?;
    anyhow::ensure!(
        !record.summaries.is_empty(),
        "{} holds no run summary",
        path.display()
    );
    Ok(record)
}

/// `goodflows last`: emit the last run's summaries again, exactly as the run
/// emitted them, so text mode prints the same reports and failure details.
pub async fn show(config_file: &Path, output: &Output) -> Result<u8> {
    emit(read(config_file)?, output).await?;
    Ok(0)
}

/// Emit the summaries of the record a [`Recorder`] wrote at `path`, as
/// [`show`] does.
pub async fn replay(path: &Path, output: &Output) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    emit(parse(path, &bytes)?, output).await
}

async fn emit(record: Record, output: &Output) -> Result<()> {
    let root = record.summaries.len() - 1;
    for (index, summary) in record.summaries.into_iter().enumerate() {
        let mut output = output.with_workflow(&summary.workflow);
        if let Some(environment) = &summary.environment {
            output = output.with_environment(environment);
        }
        if let Some(invocation) = &summary.invocation {
            let parent = invocation
                .parent
                .as_ref()
                .map(|(run, job)| (run.as_str(), job.as_str()));
            output = output.with_invocation(&invocation.id, parent);
        }
        // Only the root report offers a rerun, which keeps `--config`.
        let config = record.config.as_deref().filter(|_| index == root);
        output
            .emit_summary(summary.event, config, None, summary.interrupted)
            .await?;
    }
    Ok(())
}

/// What `goodflows run --failed` reruns: the last run's workflow and
/// environment, and the jobs its `Rerun failed:` hint names.
#[derive(Debug)]
pub struct Failed {
    pub workflow: String,
    pub environment: Option<String>,
    pub jobs: Vec<String>,
}

/// The last run's failed selection. A `workflow` or `environment` the
/// caller named must match the last run's.
pub fn failed(
    config_file: &Path,
    workflow: Option<&str>,
    environment: Option<&str>,
) -> Result<Failed> {
    let record = read(config_file)?;
    let root = record
        .summaries
        .last()
        .expect("read keeps the root summary");
    let last = describe(&root.workflow, root.environment.as_deref());
    if workflow.is_some_and(|name| name != root.workflow)
        || environment.is_some_and(|name| Some(name) != root.environment.as_deref())
    {
        let asked = describe(workflow.unwrap_or(&root.workflow), environment);
        return Err(ConfigError::new(
            "no_last_run",
            format!("no previous local run of {asked}; the last run was {last}"),
        )
        .into());
    }
    let jobs = rerun_jobs(&root.event);
    if jobs.is_empty() {
        let outcome = root.event["outcome"].as_str().unwrap_or("unknown");
        return Err(ConfigError::new(
            "no_failed_jobs",
            format!("the last run of {last} has no failed jobs to rerun (outcome: {outcome})"),
        )
        .into());
    }
    Ok(Failed {
        workflow: root.workflow.clone(),
        environment: root.environment.clone(),
        jobs,
    })
}

fn describe(workflow: &str, environment: Option<&str>) -> String {
    match environment {
        Some(environment) => format!("workflow '{workflow}' with --env {environment}"),
        None => format!("workflow '{workflow}'"),
    }
}

/// The jobs a root report's `Rerun failed:` hint names, in result order (the
/// selection in `src/output/report.rs`): when any job failed or timed out,
/// each such job, plus each job whose instances were all skipped and one of
/// them because a dependency did not pass. `--jobs` adds their prerequisites.
fn rerun_jobs(summary: &Value) -> Vec<String> {
    let results = summary["results"].as_array().map_or(&[][..], Vec::as_slice);
    let status = |result: &Value| result["status"].as_str().unwrap_or_default().to_owned();
    // Results are in configured job order, then partition index, so runs of
    // one name are one configured job.
    let jobs: Vec<_> = results
        .chunk_by(|a, b| a["name"] == b["name"])
        .map(|group| {
            let failed = group
                .iter()
                .any(|r| matches!(status(r).as_str(), "failed" | "timed_out"));
            let dependency_skipped = group.iter().all(|r| status(r) == "skipped")
                && group.iter().any(|r| r["error"] == DEPENDENCY_SKIP);
            (
                group[0]["name"].as_str().unwrap_or_default(),
                failed,
                dependency_skipped,
            )
        })
        .collect();
    if !jobs.iter().any(|(_, failed, _)| *failed) {
        return Vec::new();
    }
    jobs.into_iter()
        .filter(|(_, failed, skipped)| *failed || *skipped)
        .map(|(name, _, _)| name.to_owned())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result(name: &str, status: &str, error: Option<&str>) -> Value {
        json!({"name": name, "status": status, "error": error})
    }

    #[test]
    fn rerun_jobs_follow_the_rerun_hint() {
        let summary = json!({"results": [
            result("early", "failed", Some("exited with 1")),
            result("ok", "passed", None),
            // One failed partition fails the job.
            result("test", "passed", None),
            result("test", "timed_out", Some("timed out")),
            // Skipped only because a dependency failed: rerun.
            result("deploy", "skipped", Some(DEPENDENCY_SKIP)),
            // Skipped by fail-fast or a false condition: not rerun.
            result("late", "skipped", Some("fail_fast: previous job failed")),
            result("cond", "skipped", Some("condition is false")),
            // A cancelled job is not rerun.
            result("slow", "cancelled", Some("cancelled")),
        ]});
        assert_eq!(rerun_jobs(&summary), ["early", "test", "deploy"]);
        // No failure, no rerun, even with a dependency skip.
        let cancelled = json!({"results": [
            result("a", "cancelled", None),
            result("b", "skipped", Some(DEPENDENCY_SKIP)),
        ]});
        assert!(rerun_jobs(&cancelled).is_empty());
    }
}
