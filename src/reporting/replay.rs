//! `goodflows reports upload <run-directory>`: replay frozen snapshots of a
//! finalized invocation without a checkout, config, jobs or Git.
//!
//! Everything read from the run directory is untrusted: the manifest is
//! validated with the same rules as the config it came from, file locations are
//! derived (never read) from it, and every snapshot is re-validated against the
//! contract right before it is sent.

use super::publish::{self, Ack, Limits};
use super::{
    canonical_uuid, deliver, emit, protect_process, Head, Item, JobDirs, Manifest, ManifestState,
    Rejection, MANIFEST, MANIFEST_VERSION,
};
use crate::config;
use crate::output::Output;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;
use tokio::sync::watch;

/// Manifests are small; this bounds reading an unexpected file.
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

pub async fn upload(
    run_dir: &Path,
    output: &mut Output,
    cancel: watch::Receiver<Option<i32>>,
    budget: Option<Duration>,
) -> Result<u8> {
    let limits = Limits::default();
    let limits = budget.map_or(limits, |budget| limits.with_budget(budget));
    upload_with(run_dir, output, cancel, limits).await
}

/// Read and validate a saved manifest. Any rule violation refuses the whole run.
pub fn load_manifest(run_dir: &Path) -> Result<Manifest> {
    let path = run_dir.join(MANIFEST);
    let content = super::collect::read_bounded(&path, MAX_MANIFEST_BYTES)
        .map_err(anyhow::Error::msg)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let value: Value = serde_json::from_slice(&content)
        .with_context(|| format!("invalid manifest {}", path.display()))?;
    // Check the version before the schema, which another version may not share.
    let version = value.get("version").and_then(Value::as_u64);
    if version != Some(u64::from(MANIFEST_VERSION)) {
        bail!(
            "unsupported manifest version {} in {} (this goodflows reads version {MANIFEST_VERSION})",
            version.map_or("missing".into(), |v| v.to_string()),
            path.display()
        );
    }
    let manifest: Manifest = serde_json::from_value(value)
        .with_context(|| format!("invalid manifest {}", path.display()))?;
    if manifest.state != ManifestState::Finalized {
        bail!(
            "run {} was not finalized (state: {}); raw reports are kept in place and are not replayed",
            manifest.workflow_run_id.escape_debug(),
            serde_json::to_value(manifest.state)?.as_str().unwrap_or("unknown")
        );
    }
    validate(&manifest).with_context(|| format!("invalid manifest {}", path.display()))?;
    Ok(manifest)
}

fn validate(manifest: &Manifest) -> Result<()> {
    if !canonical_uuid(&manifest.workflow_run_id) {
        bail!("workflow_run_id is not a lowercase canonical UUID");
    }
    if !config::valid_project_slug(&manifest.project) {
        bail!("project must be 1-64 bytes matching [a-z0-9][a-z0-9_-]*");
    }
    // The env names choose which of this process's variables become the
    // endpoint and the Bearer key, so they follow the config rules exactly.
    config::validate_credential_env_names(&manifest.url_env, &manifest.token_env)?;
    if !config::valid_report_name(&manifest.workflow) {
        bail!("workflow name is invalid");
    }
    if manifest
        .environment
        .as_deref()
        .is_some_and(|e| !config::valid_report_name(e))
    {
        bail!("environment name is invalid");
    }
    let mut names = HashSet::new();
    for job in &manifest.jobs {
        if !config::valid_report_name(&job.name) {
            bail!("job names must be 1-64 UTF-8 bytes without control characters");
        }
        if !names.insert(job.name.as_str()) {
            bail!("duplicate job name");
        }
    }
    Ok(())
}

pub async fn upload_with(
    run_dir: &Path,
    output: &mut Output,
    cancel: watch::Receiver<Option<i32>>,
    limits: Limits,
) -> Result<u8> {
    let manifest = load_manifest(run_dir)?;
    *output = output.with_workflow(&manifest.workflow);
    if let Some(environment) = &manifest.environment {
        *output = output.with_environment(environment);
    }
    let run_id = manifest.workflow_run_id.as_str();
    let mut acknowledged = 0;
    let mut rejected = 0;
    // Collection rejections count as invalid: those reports were never frozen.
    let mut invalid: usize = 0;
    let mut items = Vec::new();
    for job in &manifest.jobs {
        let Some(collection) = &job.collection else {
            continue;
        };
        invalid += collection.rejected.values().sum::<usize>();
        // Locations derive from the job name, never from a manifest-supplied path.
        let dir_matches = job.dir == config::path_component(&job.name);
        let dirs = JobDirs::new(run_dir, &job.dir);
        let (mut indexes, mut ids) = (HashSet::new(), HashSet::new());
        for report in &collection.reports {
            let pending = |reason: &str, http_status: Option<u16>| json!({"type":"report_upload_pending", "run_id":run_id, "job":job.name, "report_id":report.report_id, "reason":reason, "http_status":http_status});
            if !dir_matches
                || !super::collect::valid_identifier(&report.report_id)
                || !indexes.insert(report.index)
                || !ids.insert(report.report_id.as_str())
            {
                invalid += 1;
                emit(output, pending("invalid_snapshot", None)).await;
                continue;
            }
            if dirs
                .read_marker::<Ack>(&dirs.ack(report.index))
                .is_some_and(|ack| ack.report_id == report.report_id)
            {
                acknowledged += 1;
                continue;
            }
            if let Some(marker) = dirs
                .read_marker::<Rejection>(&dirs.rejection(report.index))
                .filter(|marker| marker.report_id == report.report_id)
            {
                // The server refused these exact bytes; resending cannot succeed.
                rejected += 1;
                emit(
                    output,
                    pending("rejected_by_server", Some(marker.http_status)),
                )
                .await;
                continue;
            }
            items.push(Item {
                job: job.name.clone(),
                report_id: report.report_id.clone(),
                index: report.index,
                dirs: dirs.clone(),
                expected: Head {
                    workflow_run_id: manifest.workflow_run_id.clone(),
                    workflow: manifest.workflow.clone(),
                    environment: manifest.environment.clone(),
                    job: job.name.clone(),
                    job_status: job.status.clone().unwrap_or_default(),
                    source: manifest.source.clone(),
                    report_id: report.report_id.clone(),
                },
            });
        }
    }
    let mut pending = 0;
    if !items.is_empty() {
        let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
        let key = read(&manifest.token_env);
        if key.is_some() {
            protect_process();
        }
        let credentials = publish::resolve(
            read(&manifest.url_env).as_deref(),
            key.as_deref(),
            &manifest.project,
        );
        let delivery = deliver(run_id, items, credentials, limits, cancel.clone(), output).await;
        acknowledged += delivery.uploaded;
        pending = delivery.pending;
        rejected += delivery.rejected;
        invalid += delivery.invalid;
    }
    let success = pending == 0 && rejected == 0 && invalid == 0;
    output
        .emit(json!({"type":"reports_upload_summary", "run_id":run_id, "acknowledged":acknowledged, "pending":pending, "rejected":rejected, "invalid":invalid, "success":success}))
        .await?;
    let signal = *cancel.borrow();
    Ok(signal
        .map(|s| (128 + s) as u8)
        .unwrap_or(if success { 0 } else { 1 }))
}
