//! Controller-side result collection: validate a remote bundle against its
//! inventory in private sibling staging, then publish without replacement.

use super::agent::{safe_relative, MAX_BUNDLE_BYTES, MAX_BUNDLE_FILES};
use super::control::sha256_hex;
use anyhow::{bail, Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
struct BundleFile {
    path: String,
    size: u64,
    sha256: String,
    remote_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct BundleManifest {
    schema: u32,
    run_id: String,
    complete: bool,
    phase: String,
    remote_root: String,
    files: Vec<BundleFile>,
}

pub struct Collected {
    pub dir: PathBuf,
    pub complete: bool,
    pub phase: String,
    pub files: usize,
    /// An identical final bundle was already published.
    pub existing: bool,
}

/// The most bytes the controller reads from one export: the bundle limit
/// plus slack for the inventory (at most 16 MiB), tar headers and padding
/// (about 1 KiB per file, twice that with long names) and gzip framing.
pub const MAX_EXPORT_BYTES: u64 = MAX_BUNDLE_BYTES + 64 * 1024 * 1024;

/// Read `reader` to its end, failing as soon as it exceeds `limit` bytes or
/// delivers nothing for `idle` (a stalled download).
pub async fn read_bounded(
    reader: impl tokio::io::AsyncRead + Unpin,
    limit: u64,
    idle: std::time::Duration,
) -> Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;
    let mut bytes = Vec::new();
    let mut reader = reader.take(limit + 1);
    let mut chunk = vec![0u8; 64 * 1024];
    loop {
        let read = tokio::time::timeout(idle, reader.read(&mut chunk))
            .await
            .map_err(|_| {
                super::transport::Unreachable(format!(
                    "the result bundle download stalled: no data for {}s",
                    idle.as_secs()
                ))
            })?
            .context("read the result bundle")?;
        if read == 0 {
            break;
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
    if bytes.len() as u64 > limit {
        bail!(
            "result bundle exceeds the collection limits ({MAX_BUNDLE_FILES} files, {MAX_BUNDLE_BYTES} bytes); stopped reading after {limit} bytes"
        );
    }
    Ok(bytes)
}

/// Validate `bytes` (a gzip tar from `export`) for `run_id` and publish it
/// under `base`: `result/` when complete, a new `partial-<ms>/` otherwise.
pub fn publish(base: &Path, run_id: &str, bytes: &[u8]) -> Result<Collected> {
    crate::fsutil::dir_builder(true).create(base)?;
    // One collection at a time per bundle directory: under this lock every
    // `.collect-*` staging directory is a leftover of a killed collection.
    let _serial = lock_dir(base)?;
    sweep_staging(base);
    let staging = tempfile::Builder::new()
        .prefix(".collect-")
        .tempdir_in(base)
        .context("create private collection staging")?;
    let manifest = extract(bytes, run_id, staging.path())?;
    let target = if manifest.complete {
        base.join("result")
    } else {
        base.join(format!("partial-{}", super::control::now_ms()))
    };
    if target.exists() {
        if manifest.complete && same_content(&target, &manifest)? {
            return Ok(Collected {
                dir: target,
                complete: true,
                phase: manifest.phase,
                files: manifest.files.len(),
                existing: true,
            });
        }
        bail!(
            "{} already exists with different content; not overwriting it",
            target.display()
        );
    }
    let staged = staging.keep();
    std::fs::rename(&staged, &target).with_context(|| format!("publish {}", target.display()))?;
    Ok(Collected {
        dir: target,
        complete: manifest.complete,
        phase: manifest.phase,
        files: manifest.files.len(),
        existing: false,
    })
}

/// An exclusive `flock` on the directory itself, released on drop.
fn lock_dir(dir: &Path) -> Result<std::fs::File> {
    let file = std::fs::File::open(dir).with_context(|| format!("open {}", dir.display()))?;
    // SAFETY: a blocking flock on a descriptor this function owns.
    if unsafe { libc::flock(std::os::fd::AsRawFd::as_raw_fd(&file), libc::LOCK_EX) } != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("lock {}", dir.display()));
    }
    Ok(file)
}

/// Remove private staging directories left by a collection that was killed.
fn sweep_staging(base: &Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let stale = entry
            .file_name()
            .to_str()
            .is_some_and(|n| n.starts_with(".collect-"))
            && entry.file_type().is_ok_and(|t| t.is_dir());
        if stale {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

fn same_content(dir: &Path, manifest: &BundleManifest) -> Result<bool> {
    for file in &manifest.files {
        match std::fs::read(dir.join(&file.path)) {
            Ok(bytes) if sha256_hex(&bytes) == file.sha256 => {}
            _ => return Ok(false),
        }
    }
    Ok(true)
}

fn extract(bytes: &[u8], run_id: &str, dest: &Path) -> Result<BundleManifest> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut entries = archive.entries().context("read result bundle")?;
    let mut first = entries
        .next()
        .context("result bundle is empty")?
        .context("read result bundle")?;
    if first.path_bytes().as_ref() != b"bundle.json" || first.size() > 16 * 1024 * 1024 {
        bail!("result bundle does not start with its inventory");
    }
    let mut text = Vec::new();
    first.read_to_end(&mut text)?;
    let manifest: BundleManifest =
        serde_json::from_slice(&text).context("invalid result bundle inventory")?;
    if manifest.schema != 1 || manifest.run_id != run_id {
        bail!("result bundle is for a different run or schema");
    }
    // The collection manifest maps each remote path (as saved events and
    // history name it) to its bundle-relative path; it is kept in the bundle.
    if !Path::new(&manifest.remote_root).is_absolute() {
        bail!("result bundle inventory has no absolute remote root");
    }
    for file in &manifest.files {
        if file.path == "bundle.json"
            || file.remote_path != Path::new(&manifest.remote_root).join(&file.path)
        {
            bail!(
                "result bundle inventory maps {:?} inconsistently",
                file.path
            );
        }
    }
    let mut inventory = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(dest.join("bundle.json"))
        .context("write the collection manifest")?;
    std::io::Write::write_all(&mut inventory, &text)?;
    inventory.sync_all()?;
    // Finite limits before any allocation or extraction.
    let total: u64 = manifest.files.iter().map(|f| f.size).sum();
    if manifest.files.len() > MAX_BUNDLE_FILES || total > MAX_BUNDLE_BYTES {
        bail!(
            "result bundle exceeds the collection limits ({MAX_BUNDLE_FILES} files, {MAX_BUNDLE_BYTES} bytes)"
        );
    }
    let mut expected: BTreeMap<&str, &BundleFile> = BTreeMap::new();
    for file in &manifest.files {
        if !safe_relative(file.path.as_bytes()) || expected.insert(&file.path, file).is_some() {
            bail!("result bundle inventory has an unsafe or repeated path");
        }
    }
    let mut seen = 0;
    for entry in entries {
        let mut entry = entry.context("result bundle is truncated or corrupt")?;
        if entry.header().entry_type() != tar::EntryType::Regular {
            bail!("result bundle contains a link or special entry");
        }
        let path = String::from_utf8(entry.path_bytes().into_owned())
            .context("result bundle path is not UTF-8")?;
        let file = expected
            .remove(path.as_str())
            .with_context(|| format!("result bundle entry {path:?} is not in its inventory"))?;
        if entry.size() != file.size {
            bail!("result bundle entry {path:?} has the wrong size");
        }
        let target = dest.join(&path);
        if let Some(parent) = target.parent() {
            crate::fsutil::dir_builder(true).create(parent)?;
            if std::fs::symlink_metadata(parent).is_ok_and(|m| !m.is_dir()) {
                bail!("result bundle path {path:?} is not contained");
            }
        }
        let mut content = Vec::with_capacity(file.size as usize);
        (&mut entry).take(file.size).read_to_end(&mut content)?;
        if content.len() as u64 != file.size || sha256_hex(&content) != file.sha256 {
            bail!("result bundle entry {path:?} does not match its hash");
        }
        let mut out = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&target)
            .with_context(|| format!("write {path:?}"))?;
        std::io::Write::write_all(&mut out, &content)?;
        out.sync_all()?;
        seen += 1;
    }
    if !expected.is_empty() {
        bail!(
            "result bundle is truncated: {} inventory files missing",
            expected.len()
        );
    }
    let _ = seen;
    Ok(manifest)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bundle(manifest: serde_json::Value, entries: &[(&str, &[u8], tar::EntryType)]) -> Vec<u8> {
        let gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        let mut builder = tar::Builder::new(gz);
        let text = serde_json::to_vec(&manifest).unwrap();
        let mut entries_all: Vec<(&str, &[u8], tar::EntryType)> =
            vec![("bundle.json", &text, tar::EntryType::Regular)];
        entries_all.extend_from_slice(entries);
        for (path, bytes, kind) in entries_all {
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(kind);
            header.set_size(bytes.len() as u64);
            header.set_mode(0o600);
            // Bypass tar's own path sanitizing so hostile names reach the reader.
            let name = header.as_old_mut().name.as_mut();
            name[..path.len()].copy_from_slice(path.as_bytes());
            header.set_cksum();
            builder.append(&header, bytes).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    const RUN: &str = "0b0e2f55-6c1f-4f4d-9a59-3c3c3f0c1a10";

    const ROOT: &str = "/home/sprite/.goodflows/runs/0b0e2f55-6c1f-4f4d-9a59-3c3c3f0c1a10";

    fn manifest(complete: bool, files: &[(&str, &[u8])]) -> serde_json::Value {
        json!({"schema": 1, "run_id": RUN, "complete": complete, "phase": if complete {"completed"} else {"running"},
            "remote_root": ROOT,
            "files": files.iter().map(|(p, b)| json!({"path": p, "size": b.len(), "sha256": sha256_hex(b),
                "remote_path": format!("{ROOT}/{p}")})).collect::<Vec<_>>()})
    }

    #[tokio::test]
    async fn an_oversized_export_stream_is_cut_off_at_the_ceiling() {
        use tokio::io::AsyncReadExt;
        // An endless stream: reading stops right after the ceiling.
        let idle = std::time::Duration::from_secs(5);
        let error = read_bounded(tokio::io::repeat(7), 4096, idle)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("collection limits"), "{error}");
        let exact = read_bounded(tokio::io::repeat(7).take(4096), 4096, idle)
            .await
            .unwrap();
        assert_eq!(exact.len(), 4096);
        const { assert!(MAX_EXPORT_BYTES > MAX_BUNDLE_BYTES) };
    }

    #[tokio::test]
    async fn a_stalled_export_stream_fails_as_unreachable() {
        use tokio::io::AsyncWriteExt;
        // The writer stays open but sends nothing after its first bytes.
        let (reader, mut writer) = tokio::io::duplex(64);
        writer.write_all(b"partial").await.unwrap();
        let error = read_bounded(reader, 4096, std::time::Duration::from_millis(300))
            .await
            .unwrap_err();
        assert!(
            error.is::<super::super::transport::Unreachable>(),
            "{error}"
        );
        assert!(error.to_string().contains("stalled"), "{error}");
        drop(writer);
    }

    #[test]
    fn final_collection_is_idempotent_and_conflicts_fail() {
        let dir = tempfile::tempdir().unwrap();
        let good = bundle(
            manifest(true, &[("results/a.txt", b"one")]),
            &[("results/a.txt", b"one", tar::EntryType::Regular)],
        );
        let first = publish(dir.path(), RUN, &good).unwrap();
        assert!(first.complete && !first.existing);
        // The bundle keeps its collection manifest: remote path -> bundle path.
        let kept: serde_json::Value =
            serde_json::from_slice(&std::fs::read(first.dir.join("bundle.json")).unwrap()).unwrap();
        assert_eq!(kept["remote_root"], ROOT);
        assert_eq!(kept["files"][0]["path"], "results/a.txt");
        assert_eq!(
            kept["files"][0]["remote_path"],
            format!("{ROOT}/results/a.txt")
        );
        assert_eq!(
            std::fs::read(first.dir.join("results/a.txt")).unwrap(),
            b"one"
        );
        assert!(publish(dir.path(), RUN, &good).unwrap().existing);
        let other = bundle(
            manifest(true, &[("results/a.txt", b"two")]),
            &[("results/a.txt", b"two", tar::EntryType::Regular)],
        );
        assert!(publish(dir.path(), RUN, &other).is_err());
        assert_eq!(
            std::fs::read(first.dir.join("results/a.txt")).unwrap(),
            b"one"
        );
    }

    #[test]
    fn a_killed_collections_staging_is_swept_by_the_next_one() {
        let dir = tempfile::tempdir().unwrap();
        let leftover = dir.path().join(".collect-killed");
        std::fs::create_dir_all(leftover.join("results")).unwrap();
        std::fs::write(leftover.join("results/partial.txt"), b"half").unwrap();
        let good = bundle(
            manifest(true, &[("results/a.txt", b"one")]),
            &[("results/a.txt", b"one", tar::EntryType::Regular)],
        );
        publish(dir.path(), RUN, &good).unwrap();
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["result"]);
        // A failed collection cleans up its own staging too.
        assert!(publish(dir.path(), RUN, &good[..good.len() / 2]).is_err());
        let names: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["result"]);
    }

    #[test]
    fn partial_snapshots_never_replace_each_other_or_the_final_bundle() {
        let dir = tempfile::tempdir().unwrap();
        let partial = bundle(
            manifest(false, &[("events.jsonl", b"x")]),
            &[("events.jsonl", b"x", tar::EntryType::Regular)],
        );
        let p = publish(dir.path(), RUN, &partial).unwrap();
        assert!(
            !p.complete
                && p.dir
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .starts_with("partial-")
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
        let complete = bundle(
            manifest(true, &[("events.jsonl", b"xy")]),
            &[("events.jsonl", b"xy", tar::EntryType::Regular)],
        );
        let c = publish(dir.path(), RUN, &complete).unwrap();
        assert_eq!(std::fs::read(p.dir.join("events.jsonl")).unwrap(), b"x");
        assert_eq!(std::fs::read(c.dir.join("events.jsonl")).unwrap(), b"xy");
    }

    #[test]
    fn hostile_or_damaged_bundles_publish_nothing() {
        let outside = tempfile::tempdir().unwrap();
        let cases = vec![
            // traversal and absolute names
            bundle(
                manifest(true, &[("../../outside", b"x")]),
                &[("../../outside", b"x", tar::EntryType::Regular)],
            ),
            bundle(
                manifest(true, &[("/abs", b"x")]),
                &[("/abs", b"x", tar::EntryType::Regular)],
            ),
            // a link entry redirecting extraction
            bundle(
                manifest(true, &[("a", b"")]),
                &[("a", b"", tar::EntryType::Symlink)],
            ),
            // hash mismatch
            bundle(
                json!({"schema":1,"run_id":RUN,"complete":true,"phase":"completed","remote_root":ROOT,"files":[{"path":"a","size":1,"sha256":"00","remote_path":format!("{ROOT}/a")}]}),
                &[("a", b"x", tar::EntryType::Regular)],
            ),
            // missing file (truncated)
            bundle(
                manifest(true, &[("a", b"x"), ("b", b"y")]),
                &[("a", b"x", tar::EntryType::Regular)],
            ),
            // oversize inventory
            bundle(
                json!({"schema":1,"run_id":RUN,"complete":true,"phase":"completed","remote_root":ROOT,"files":[{"path":"a","size":MAX_BUNDLE_BYTES + 1,"sha256":"00","remote_path":format!("{ROOT}/a")}]}),
                &[],
            ),
            // a remote path that does not map to its bundle path
            bundle(
                json!({"schema":1,"run_id":RUN,"complete":true,"phase":"completed","remote_root":ROOT,
                    "files":[{"path":"a","size":1,"sha256":sha256_hex(b"x"),"remote_path":"/etc/passwd"}]}),
                &[("a", b"x", tar::EntryType::Regular)],
            ),
            // an inventory entry that would shadow the collection manifest
            bundle(
                manifest(true, &[("bundle.json", b"x")]),
                &[("bundle.json", b"x", tar::EntryType::Regular)],
            ),
            // a different run
            bundle(
                json!({"schema":1,"run_id":"x","complete":true,"phase":"completed","remote_root":ROOT,"files":[]}),
                &[],
            ),
        ];
        for (index, bytes) in cases.into_iter().enumerate() {
            let dir = tempfile::tempdir().unwrap();
            assert!(publish(dir.path(), RUN, &bytes).is_err(), "case {index}");
            assert!(!dir.path().join("result").exists(), "case {index}");
        }
        // A byte-truncated download fails too.
        let good = bundle(
            manifest(true, &[("a", &[7u8; 50_000])]),
            &[("a", &[7u8; 50_000], tar::EntryType::Regular)],
        );
        let dir = tempfile::tempdir().unwrap();
        assert!(publish(dir.path(), RUN, &good[..good.len() / 2]).is_err());
        assert!(!dir.path().join("result").exists());
        assert_eq!(std::fs::read_dir(outside.path()).unwrap().count(), 0);
    }
}
