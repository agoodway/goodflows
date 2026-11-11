//! `goodflows reports upload <run-directory>`: replay frozen snapshots of a
//! finalized invocation without a checkout, config, jobs or Git.
//!
//! Everything read from the run directory is untrusted: the manifest is
//! validated with the same rules as the config it came from, file locations are
//! derived (never read) from it, and every snapshot is re-validated against the
//! contract right before it is sent.

use super::publish::RunAck;
use super::publish::{self, Ack, Limits};
use super::{
    deliver, emit, manifest_version, protect_process, read_run_marker, run_contract,
    send_run_record, settle_run_record, Head, Item, JobDirs, Manifest, ManifestPartition,
    ManifestState, Rejection, RunRejection, MANIFEST, MANIFEST_VERSION,
    PARTITIONED_MANIFEST_VERSION, RUN_RECORD, RUN_RECORD_ACK, RUN_RECORD_MANIFEST_VERSION,
    RUN_RECORD_REJECTED,
};
use crate::config::{self, Partition};
use crate::fsutil::{canonical_uuid, real_dir};
use crate::output::Output;
use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
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
    let supported = u64::from(MANIFEST_VERSION)..=u64::from(RUN_RECORD_MANIFEST_VERSION);
    if !version.is_some_and(|v| supported.contains(&v)) {
        bail!(
            "unsupported manifest version {} in {} (this goodflows reads versions {MANIFEST_VERSION} to {RUN_RECORD_MANIFEST_VERSION})",
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
    // A run record is listed exactly when the manifest is version 3, and
    // otherwise partition entries exist exactly when it is version 2, so an
    // older goodflows never meets either under a version it claims to read.
    if manifest.version != manifest_version(&manifest.jobs, manifest.run_record.is_some()) {
        bail!(
            "manifest version {} requires {}",
            manifest.version,
            match manifest.version {
                MANIFEST_VERSION => "no partition entries and no run record",
                PARTITIONED_MANIFEST_VERSION =>
                    "at least one job with partition entries and no run record",
                _ => "a run record",
            }
        );
    }
    if manifest
        .run_record
        .is_some_and(|record| record.schema_version != 1)
    {
        bail!("run record schema_version must be 1");
    }
    // Per job name: `None` for an unpartitioned entry, or the instance count
    // and the indexes seen so far.
    let mut names: HashMap<&str, Option<(u32, HashSet<u32>)>> = HashMap::new();
    for job in &manifest.jobs {
        if !config::valid_report_name(&job.name) {
            bail!("job names must be 1-64 UTF-8 bytes without control characters");
        }
        match (names.get_mut(job.name.as_str()), job.partition) {
            (None, None) => {
                names.insert(&job.name, None);
            }
            (None, Some(partition)) => {
                valid_partition(partition)?;
                names.insert(
                    &job.name,
                    Some((partition.count, HashSet::from([partition.index]))),
                );
            }
            (Some(None), None) => bail!("duplicate job name"),
            (Some(None), Some(_)) | (Some(Some(_)), None) => {
                bail!("job has both partitioned and unpartitioned entries")
            }
            (Some(Some((count, indexes))), Some(partition)) => {
                valid_partition(partition)?;
                if partition.count != *count {
                    bail!("partition entries of one job disagree on the partition count");
                }
                if !indexes.insert(partition.index) {
                    bail!("duplicate partition index");
                }
            }
        }
    }
    Ok(())
}

fn valid_partition(partition: ManifestPartition) -> Result<()> {
    if !(1..=config::MAX_PARTITIONS).contains(&partition.count) {
        bail!(
            "partition count must be from 1 to {}",
            config::MAX_PARTITIONS
        );
    }
    if !(1..=partition.count).contains(&partition.index) {
        bail!("partition index must be from 1 to its count");
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
    // report_id is unique across every entry (instance) of one job, as on the
    // server. As in collection, an id listed more than once refuses every copy.
    let mut job_ids: HashMap<(&str, &str), usize> = HashMap::new();
    for job in &manifest.jobs {
        for report in job.collection.iter().flat_map(|c| &c.reports) {
            *job_ids
                .entry((job.name.as_str(), report.report_id.as_str()))
                .or_default() += 1;
        }
    }
    for job in &manifest.jobs {
        let Some(collection) = &job.collection else {
            continue;
        };
        // One per rejected file; a leftover duplicate snapshot is not another.
        invalid += super::collect::rejected_files(&collection.rejected);
        // Locations derive from the job name and partition, never from a
        // manifest-supplied path.
        let partition = job.instance();
        let dir_matches = job.dir == config::path_component(&job.name);
        let dirs = JobDirs::new(run_dir, &job.dir, partition);
        let mut indexes = HashSet::new();
        for report in &collection.reports {
            let pending = |reason: &str, http_status: Option<u16>| {
                Partition::tag(
                    partition,
                    json!({"type":"report_upload_pending", "run_id":run_id, "job":job.name, "report_id":report.report_id, "reason":reason, "http_status":http_status}),
                )
            };
            if !dir_matches
                || !super::contract::valid_identifier(&report.report_id)
                || !indexes.insert(report.index)
                || job_ids[&(job.name.as_str(), report.report_id.as_str())] > 1
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
                partition,
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
                    partition: partition.map(|p| p.index.to_string()),
                    partition_count: partition.map(|p| u64::from(p.count)),
                },
            });
        }
    }
    let read = |name: &str| std::env::var(name).ok().filter(|v| !v.is_empty());
    let credentials = || {
        let key = read(&manifest.token_env);
        if key.is_some() {
            protect_process();
        }
        publish::resolve(
            read(&manifest.url_env).as_deref(),
            key.as_deref(),
            &manifest.project,
        )
    };
    // Shared by the reports and the run record, so one budget covers both.
    let mut publisher = None;
    let mut pending = 0;
    if !items.is_empty() {
        let delivery = deliver(
            run_id,
            items,
            credentials(),
            &mut publisher,
            limits,
            cancel.clone(),
            output,
        )
        .await;
        acknowledged += delivery.uploaded;
        pending = delivery.pending;
        rejected += delivery.rejected;
        invalid += delivery.invalid;
    }
    // The run record follows the reports.
    let run_record = match manifest.run_record {
        None => None,
        Some(_) => {
            let event = |reason: &str, http_status: Option<u16>| json!({"type":"run_upload_pending", "run_id":run_id, "reason":reason, "http_status":http_status});
            let state = match load_run_record(run_dir, &manifest) {
                None => {
                    emit(output, event("invalid_record", None)).await;
                    "invalid"
                }
                Some(_)
                    if read_run_marker::<RunAck>(run_dir, RUN_RECORD_ACK)
                        .is_some_and(|ack| ack.run_id == run_id) =>
                {
                    "acknowledged"
                }
                Some(body) => {
                    let rejection = read_run_marker::<RunRejection>(run_dir, RUN_RECORD_REJECTED)
                        .filter(|marker| marker.run_id == run_id);
                    if let Some(marker) = rejection {
                        // The server refused these exact bytes; resending cannot succeed.
                        emit(
                            output,
                            event("rejected_by_server", Some(marker.http_status)),
                        )
                        .await;
                        "rejected"
                    } else {
                        let outcome = send_run_record(
                            &mut publisher,
                            credentials(),
                            limits,
                            &body,
                            run_id,
                            cancel.clone(),
                        )
                        .await;
                        settle_run_record(run_dir, run_id, outcome, output).await
                    }
                }
            };
            Some(state)
        }
    };
    let success = pending == 0
        && rejected == 0
        && invalid == 0
        && run_record.is_none_or(|state| state == "acknowledged");
    output
        .emit(json!({"type":"reports_upload_summary", "run_id":run_id, "acknowledged":acknowledged, "pending":pending, "rejected":rejected, "invalid":invalid, "run_record":run_record, "success":success}))
        .await?;
    let signal = *cancel.borrow();
    Ok(signal
        .map(|s| (128 + s) as u8)
        .unwrap_or(if success { 0 } else { 1 }))
}

/// Read a listed run record for replay: never through a
/// symlink, bounded to one byte over the body limit, re-validated against
/// the contract, and bound to the manifest's run, workflow and environment.
fn load_run_record(run_dir: &Path, manifest: &Manifest) -> Option<Vec<u8>> {
    real_dir(run_dir).ok()?;
    let limit = run_contract::MAX_BODY_BYTES as u64 + 1;
    let body = super::collect::read_bounded(&run_dir.join(RUN_RECORD), limit).ok()?;
    let value: Value = serde_json::from_slice(&body).ok()?;
    run_contract::validate(&value, body.len()).ok()?;
    let bound = value["run_id"] == manifest.workflow_run_id.as_str()
        && value["workflow"] == manifest.workflow.as_str()
        && value.get("environment").and_then(Value::as_str) == manifest.environment.as_deref();
    bound.then_some(body)
}
