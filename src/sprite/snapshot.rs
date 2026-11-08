//! Current-working-tree snapshot for Sprite runs.
//!
//! The Git repository containing the selected config is enumerated with
//! `git ls-files --cached --others --exclude-standard`; current file contents
//! (not HEAD) are copied once into a private immutable staging directory, and
//! the manifest and archive derive from those copied bytes.

use super::control::{ManifestEntry, Workdirs};
use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::OsStr;
use std::io::{Read, Write};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Encode raw path bytes losslessly as a JSON-safe string: valid UTF-8 is kept
/// except `%`, which becomes `%25`; every invalid byte becomes `%XX`.
pub fn encode_path(bytes: &[u8]) -> String {
    let mut out = String::new();
    for chunk in bytes.utf8_chunks() {
        for c in chunk.valid().chars() {
            if c == '%' {
                out.push_str("%25");
            } else {
                out.push(c);
            }
        }
        for b in chunk.invalid() {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub fn decode_path(encoded: &str) -> Result<Vec<u8>> {
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = encoded.get(i + 1..i + 3).context("truncated path escape")?;
            out.push(u8::from_str_radix(hex, 16).context("invalid path escape")?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

/// Why a Git-listed path is left out. Shown with the path, never contents.
pub fn exclusion_reason(path: &[u8]) -> Option<&'static str> {
    let components: Vec<&[u8]> = path.split(|b| *b == b'/').collect();
    for component in &components {
        match *component {
            b".git" => return Some("git metadata"),
            b".amp" => return Some("controller agent state"),
            b".sprite" => return Some("Sprite CLI context"),
            b".goodflows" => return Some("GoodFlows controller state"),
            b".ssh" | b".aws" | b".gnupg" => return Some("credential directory"),
            _ => {}
        }
    }
    let name = components.last().copied().unwrap_or_default();
    let lower = name.to_ascii_lowercase();
    let starts = |p: &[u8]| lower.starts_with(p);
    let ends = |s: &[u8]| lower.ends_with(s);
    if lower == b".env" || starts(b".env.") {
        return Some("environment file");
    }
    if matches!(
        lower.as_slice(),
        b".netrc" | b".npmrc" | b".pypirc" | b".git-credentials"
    ) || starts(b"credentials")
        || starts(b"secrets.")
    {
        return Some("credential file");
    }
    let key_names = [
        &b"id_rsa"[..],
        b"id_dsa",
        b"id_ecdsa",
        b"id_ed25519",
        b"id_ecdsa_sk",
        b"id_ed25519_sk",
    ];
    if key_names.contains(&lower.as_slice())
        || [
            &b".pem"[..],
            b".key",
            b".p12",
            b".pfx",
            b".jks",
            b".keystore",
        ]
        .iter()
        .any(|e| ends(e))
    {
        return Some("private key file");
    }
    None
}

fn private_key_content(bytes: &[u8]) -> bool {
    let marker = b"-----BEGIN ";
    bytes.windows(marker.len()).enumerate().any(|(i, w)| {
        w == marker
            && bytes[i..]
                .iter()
                .take(80)
                .collect::<Vec<_>>()
                .windows(16)
                .any(|w| w.iter().map(|b| **b).collect::<Vec<_>>() == b"PRIVATE KEY-----")
    })
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
}

pub struct Snapshot {
    pub config_path: String,
    /// The config file exactly as copied into the snapshot.
    pub config_text: String,
    pub manifest: Vec<ManifestEntry>,
    pub excluded: Vec<(String, &'static str)>,
    pub archive: PathBuf,
    pub archive_sha256: String,
    pub archive_bytes: u64,
    pub workdirs: Workdirs,
    /// Private staging directory; removed on drop.
    _staging: tempfile::TempDir,
}

fn git(root: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .context("failed to run git (required to snapshot the working tree)")?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(output.stdout)
}

/// The canonical Git root containing `dir`, or an explicit limitation.
pub fn git_root(dir: &Path) -> Result<PathBuf> {
    let output = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .context("failed to run git (required to snapshot the working tree)")?;
    if !output.status.success() {
        bail!(
            "Sprite runs snapshot a Git working tree, but {} is not inside one",
            dir.display()
        );
    }
    let text = String::from_utf8(output.stdout).context("Git root is not UTF-8")?;
    Path::new(text.trim_end_matches('\n'))
        .canonicalize()
        .context("resolve Git root")
}

fn relative(root: &Path, path: &Path, what: &str) -> Result<String> {
    let rel = path.strip_prefix(root).map_err(|_| {
        anyhow::anyhow!(
            "{what} {} is outside the Git root {}; Sprite runs cannot map it",
            path.display(),
            root.display()
        )
    })?;
    Ok(encode_path(rel.as_os_str().as_bytes()))
}

/// `path` with `.` and `..` applied lexically, without following symlinks.
fn lexical(path: &Path) -> PathBuf {
    use std::path::Component;
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                out.pop();
            }
            Component::CurDir => {}
            other => out.push(other),
        }
    }
    out
}

/// Map every declared workdir to a path relative to the source root.
pub fn map_workdirs(root: &Path, config: &crate::config::Config) -> Result<Workdirs> {
    let mut workdirs = Workdirs::default();
    // Config::load resolves (and only top-level jobs use) the top-level
    // workdir when there are top-level jobs; otherwise workflows already
    // carry their resolved copies and the remote config drops it.
    if let Some(dir) = config
        .settings
        .workdir
        .as_ref()
        .filter(|_| !config.jobs.is_empty())
    {
        workdirs.settings = Some(relative(root, dir, "settings workdir")?);
    }
    for (name, workflow) in &config.workflows {
        if let Some(dir) = &workflow.settings.workdir {
            workdirs.workflows.insert(
                name.clone(),
                relative(root, dir, &format!("workflow '{name}' workdir"))?,
            );
        }
    }
    for (name, environment) in &config.environments {
        if let Some(dir) = &environment.settings.workdir {
            workdirs.environments.insert(
                name.clone(),
                relative(root, dir, &format!("environment '{name}' workdir"))?,
            );
        }
    }
    Ok(workdirs)
}

/// Inputs that make a snapshot fail rather than archive credentials.
pub struct Sensitive {
    /// Controller credential and forwarded values checked against file bytes.
    pub values: Vec<Vec<u8>>,
}

struct Staged {
    rel: Vec<u8>,
    staged: PathBuf,
    size: u64,
    sha256: String,
    executable: bool,
}

/// Copy one regular file into staging, failing if it changes during the copy.
fn copy_regular(source: &Path, dest: &Path) -> Result<(u64, String, bool, Vec<u8>)> {
    let mut file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags_nofollow()
        .open(source)
        .with_context(|| format!("read {}", source.display()))?;
    let before = file.metadata()?;
    if !before.is_file() {
        bail!("{} is not a regular file", source.display());
    }
    super::testhook::pause(&format!(
        "snapshot-copy-{}",
        encode_path(source.file_name().unwrap_or_default().as_bytes())
    ));
    let mut bytes = Vec::with_capacity(before.len() as usize);
    file.read_to_end(&mut bytes)?;
    let after = std::fs::symlink_metadata(source)?;
    if changed(&before, &after, bytes.len() as u64) {
        bail!(
            "{} changed while it was being copied; rerun when the working tree is quiet",
            source.display()
        );
    }
    let executable = before.permissions().mode() & 0o111 != 0;
    write_staged(dest, &bytes, executable)?;
    Ok((
        bytes.len() as u64,
        format!("{:x}", Sha256::digest(&bytes)),
        executable,
        bytes,
    ))
}

/// A copy is torn when the file's identity, size or modification time moved
/// while it was read, or when the bytes read differ from the size seen first.
fn changed(before: &std::fs::Metadata, after: &std::fs::Metadata, read: u64) -> bool {
    after.len() != before.len()
        || after.mtime() != before.mtime()
        || after.mtime_nsec() != before.mtime_nsec()
        || after.ino() != before.ino()
        || after.dev() != before.dev()
        || read != before.len()
}

/// A path that is both a file and a parent directory cannot be extracted.
fn check_collisions<'a>(paths: impl Iterator<Item = &'a Vec<u8>>) -> Result<()> {
    let keys: Vec<&Vec<u8>> = paths.collect();
    for (i, a) in keys.iter().enumerate() {
        // Sorted order puts `a/...` after `a` but possibly after `a-b` too, so
        // scan forward while the prefix still matches.
        for b in &keys[i + 1..] {
            if !b.starts_with(a) {
                break;
            }
            if b.get(a.len()) == Some(&b'/') {
                bail!(
                    "path collision: {} is both a file and a directory",
                    encode_path(a)
                );
            }
        }
    }
    Ok(())
}

trait NoFollow {
    fn custom_flags_nofollow(&mut self) -> &mut Self;
}

impl NoFollow for std::fs::OpenOptions {
    fn custom_flags_nofollow(&mut self) -> &mut Self {
        std::os::unix::fs::OpenOptionsExt::custom_flags(self, libc::O_NOFOLLOW)
    }
}

fn write_staged(dest: &Path, bytes: &[u8], executable: bool) -> Result<()> {
    if let Some(parent) = dest.parent() {
        crate::fsutil::dir_builder(true).create(parent)?;
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    // Staged copies are read-only: the snapshot is immutable once taken.
    std::fs::set_permissions(
        dest,
        std::fs::Permissions::from_mode(if executable { 0o500 } else { 0o400 }),
    )?;
    Ok(())
}

/// Take the snapshot and build its gzip tar archive.
pub fn take(
    root: &Path,
    config_file: &Path,
    workdirs: Workdirs,
    hooks: &[(&str, &str)],
    hook_workdir: &Path,
    sensitive: &Sensitive,
) -> Result<Snapshot> {
    let config_path = relative(root, config_file, "config")?;
    // Submodule gitlinks are not file content.
    let stage = git(root, &["ls-files", "-z", "--stage"])?;
    for record in stage.split(|b| *b == 0).filter(|r| !r.is_empty()) {
        if record.starts_with(b"160000 ") {
            let path = record.split(|b| *b == b'\t').nth(1).unwrap_or_default();
            bail!(
                "submodule {} is not supported by Sprite runs; vendor its files or run locally",
                encode_path(path)
            );
        }
    }
    let listed = git(
        root,
        &[
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ],
    )?;
    let paths: BTreeSet<Vec<u8>> = listed
        .split(|b| *b == 0)
        .filter(|p| !p.is_empty())
        .map(<[u8]>::to_vec)
        .collect();
    let staging = tempfile::Builder::new()
        .prefix("goodflows-sprite-")
        .tempdir()
        .context("create private snapshot staging directory")?;
    std::fs::set_permissions(staging.path(), std::fs::Permissions::from_mode(0o700))?;
    let files_dir = staging.path().join("files");
    crate::fsutil::dir_builder(false).create(&files_dir)?;
    let mut excluded = Vec::new();
    let mut staged: BTreeMap<Vec<u8>, Staged> = BTreeMap::new();
    let mut links: Vec<(Vec<u8>, PathBuf)> = Vec::new();
    for rel in &paths {
        if let Some(reason) = exclusion_reason(rel) {
            excluded.push((encode_path(rel), reason));
            continue;
        }
        let source = root.join(OsStr::from_bytes(rel));
        let meta = match std::fs::symlink_metadata(&source) {
            Ok(meta) => meta,
            // Deleted in the working tree: absent from the snapshot.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(error).with_context(|| format!("inspect {}", source.display()))
            }
        };
        let file_type = meta.file_type();
        if file_type.is_symlink() {
            links.push((rel.clone(), source));
        } else if file_type.is_file() {
            let dest = files_dir.join(OsStr::from_bytes(rel));
            let (size, sha256, executable, bytes) = copy_regular(&source, &dest)?;
            let reason = if private_key_content(&bytes) {
                Some("private key content")
            } else if sensitive.values.iter().any(|value| contains(&bytes, value)) {
                Some("contains a controller or forwarded credential value")
            } else {
                None
            };
            if let Some(reason) = reason {
                let _ = std::fs::set_permissions(&dest, std::fs::Permissions::from_mode(0o600));
                std::fs::remove_file(&dest)?;
                excluded.push((encode_path(rel), reason));
                continue;
            }
            staged.insert(
                rel.clone(),
                Staged {
                    rel: rel.clone(),
                    staged: dest,
                    size,
                    sha256,
                    executable,
                },
            );
        } else if file_type.is_dir() {
            // A nested repository or similar listed as a directory.
            bail!(
                "{} is a directory listed by Git (nested repository?); not supported by Sprite runs",
                encode_path(rel)
            );
        } else {
            bail!(
                "{} is a special file (fifo, socket or device); Sprite runs copy only regular files",
                encode_path(rel)
            );
        }
    }
    // Internal symlinks become copies of eligible in-root regular files.
    for (rel, source) in links {
        let target = source.canonicalize().map_err(|_| {
            anyhow::anyhow!(
                "symlink {} is unresolved or cyclic; Sprite runs cannot materialize it",
                encode_path(&rel)
            )
        })?;
        let Ok(target_rel) = target.strip_prefix(root) else {
            bail!(
                "symlink {} points outside the source root; its target is not read",
                encode_path(&rel)
            );
        };
        let target_rel = target_rel.as_os_str().as_bytes().to_vec();
        let Some(target_entry) = staged.get(&target_rel) else {
            bail!(
                "symlink {} points to {}, which is not an eligible snapshot file",
                encode_path(&rel),
                encode_path(&target_rel)
            );
        };
        let bytes = std::fs::read(&target_entry.staged)?;
        let dest = files_dir.join(OsStr::from_bytes(&rel));
        let (size, sha256, executable) = (
            target_entry.size,
            target_entry.sha256.clone(),
            target_entry.executable,
        );
        write_staged(&dest, &bytes, executable)?;
        staged.insert(
            rel.clone(),
            Staged {
                rel,
                staged: dest,
                size,
                sha256,
                executable,
            },
        );
    }
    check_collisions(staged.keys())?;
    let excluded_set: HashMap<String, &'static str> = excluded.iter().cloned().collect();
    let Some(config_entry) = staged.get(&decode_path(&config_path)?) else {
        let reason = excluded_set
            .get(&config_path)
            .copied()
            .unwrap_or("not listed by Git or ignored");
        bail!("config {config_path} is not part of the snapshot ({reason})");
    };
    // The remote run's config is the snapshot's copy, never a later re-read.
    let config_text = String::from_utf8(std::fs::read(&config_entry.staged)?)
        .with_context(|| format!("config {config_path} is not UTF-8"))?;
    // Path-based hook inputs must be delivered too.
    for (field, command) in hooks {
        for word in shell_words::split(command).unwrap_or_default() {
            if word.starts_with('-') || word.starts_with('/') || !word.contains('/') {
                continue;
            }
            // `./x` and `../x` name manifest paths only once normalized
            // against the hook's workdir; a path that leaves the source root
            // is never delivered.
            let path = lexical(&hook_workdir.join(&word));
            let Ok(rel) = path.strip_prefix(root) else {
                if path.is_file() {
                    bail!(
                        "[sprite] {field} uses {}, which is outside the source root, so it is not part of the snapshot",
                        encode_path(word.as_bytes())
                    );
                }
                continue;
            };
            let rel_bytes = rel.as_os_str().as_bytes().to_vec();
            if path.is_file() && !staged.contains_key(&rel_bytes) {
                let encoded = encode_path(&rel_bytes);
                let reason = excluded_set
                    .get(&encoded)
                    .copied()
                    .unwrap_or("not listed by Git or ignored");
                bail!(
                    "[sprite] {field} uses {encoded}, which is not part of the snapshot ({reason})"
                );
            }
        }
    }
    // Every declared workdir must exist in the delivered tree.
    let mut dirs: Vec<&String> = workdirs.settings.iter().collect();
    dirs.extend(workdirs.workflows.values());
    dirs.extend(workdirs.environments.values());
    for dir in dirs {
        if dir.is_empty() {
            continue;
        }
        let prefix = decode_path(dir)?;
        if !staged
            .keys()
            .any(|k| k.starts_with(&prefix) && k.get(prefix.len()) == Some(&b'/'))
        {
            bail!("workdir {dir} contains no snapshot files, so it would not exist remotely");
        }
    }
    let manifest: Vec<ManifestEntry> = staged
        .values()
        .map(|s| ManifestEntry {
            path: encode_path(&s.rel),
            size: s.size,
            sha256: s.sha256.clone(),
            executable: s.executable,
        })
        .collect();
    let archive = staging.path().join("source.tar.gz");
    let (archive_sha256, archive_bytes) = write_archive(&archive, staged.values())?;
    Ok(Snapshot {
        config_path,
        config_text,
        manifest,
        excluded,
        archive,
        archive_sha256,
        archive_bytes,
        workdirs,
        _staging: staging,
    })
}

struct HashingWriter<W> {
    inner: W,
    hasher: Sha256,
    bytes: u64,
}

impl<W: Write> Write for HashingWriter<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn write_archive<'a>(
    path: &Path,
    files: impl Iterator<Item = &'a Staged>,
) -> Result<(String, u64)> {
    let out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    let writer = HashingWriter {
        inner: out,
        hasher: Sha256::new(),
        bytes: 0,
    };
    let gz = flate2::write::GzEncoder::new(writer, flate2::Compression::default());
    let mut builder = tar::Builder::new(gz);
    for file in files {
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Regular);
        header.set_size(file.size);
        header.set_mode(if file.executable { 0o755 } else { 0o644 });
        header.set_mtime(0);
        header.set_uid(0);
        header.set_gid(0);
        let mut input = std::fs::File::open(&file.staged)?;
        builder.append_data(&mut header, OsStr::from_bytes(&file.rel), &mut input)?;
    }
    let gz = builder.into_inner()?;
    let mut writer = gz.finish()?;
    writer.flush()?;
    writer.inner.sync_all()?;
    Ok((format!("{:x}", writer.hasher.finalize()), writer.bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_encoding_round_trips_arbitrary_bytes() {
        for raw in [
            &b"plain/file.txt"[..],
            b"with space/and%percent",
            b"bad\xff\xfebytes",
            "ünïcode".as_bytes(),
        ] {
            assert_eq!(decode_path(&encode_path(raw)).unwrap(), raw);
        }
        assert_eq!(encode_path(b"a%b"), "a%25b");
    }

    #[test]
    fn credential_and_metadata_paths_are_excluded() {
        for path in [
            &b".env"[..],
            b"app/.env.local",
            b".git/config",
            b"sub/.goodflows/x",
            b".amp/in/a",
            b"deploy/id_ed25519",
            b"certs/server.pem",
            b"config/credentials.json",
            b"config/secrets.yml",
            b"home/.ssh/config",
            b".npmrc",
        ] {
            assert!(
                exclusion_reason(path).is_some(),
                "{}",
                String::from_utf8_lossy(path)
            );
        }
        for path in [
            &b"src/main.rs"[..],
            b"environment.md",
            b"id_ed25519.pub",
            b"docs/keys.md",
        ] {
            assert!(
                exclusion_reason(path).is_none(),
                "{}",
                String::from_utf8_lossy(path)
            );
        }
    }

    #[test]
    fn file_and_directory_collisions_are_rejected() {
        let ok: Vec<Vec<u8>> = vec![b"a".to_vec(), b"a-b".to_vec(), b"ab/c".to_vec()];
        check_collisions(ok.iter()).unwrap();
        // `a` sorts before `a-b`, which sorts before `a/c`.
        let bad: Vec<Vec<u8>> = vec![b"a".to_vec(), b"a-b".to_vec(), b"a/c".to_vec()];
        assert!(check_collisions(bad.iter())
            .unwrap_err()
            .to_string()
            .contains("path collision: a"));
    }

    #[test]
    fn torn_copies_are_detected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"one").unwrap();
        let before = std::fs::metadata(&path).unwrap();
        assert!(!changed(&before, &std::fs::metadata(&path).unwrap(), 3));
        // Fewer bytes read than the size first observed.
        assert!(changed(&before, &before, 2));
        std::fs::write(&path, b"longer").unwrap();
        assert!(changed(&before, &std::fs::metadata(&path).unwrap(), 3));
        // Replaced by a different file of the same size.
        std::fs::remove_file(&path).unwrap();
        std::fs::write(dir.path().join("keep"), b"x").unwrap();
        std::fs::write(&path, b"one").unwrap();
        let after = std::fs::metadata(&path).unwrap();
        assert!(changed(&before, &after, 3) || after.ino() == before.ino());
    }

    #[test]
    fn private_key_content_is_recognized() {
        // Assembled at runtime so this source file is not itself excluded
        // from snapshots as private key content (GoodFlows dogfoods itself).
        let pem = |kind: &str| ["-----BEGIN", " ", kind, "-----"].concat();
        assert!(private_key_content(
            format!("x\n{}\nabc", pem("OPENSSH PRIVATE KEY")).as_bytes()
        ));
        assert!(private_key_content(pem("RSA PRIVATE KEY").as_bytes()));
        assert!(!private_key_content(pem("CERTIFICATE").as_bytes()));
        assert!(!private_key_content(include_bytes!("snapshot.rs")));
    }
}
