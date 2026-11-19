//! Cache archives: a gzip tar whose first entry is a JSON header, with the
//! declared workdir paths under `workdir/` and the `~/` paths under `home/`.
//!
//! The writer refuses special files and symlinks that leave their root (the
//! workdir, or the declared `~/` directory). The extractor checks the
//! archive's size and sha256, then every entry, before it writes anything;
//! it unpacks into a staging directory and swaps the staged paths into place.

use super::glob;
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::ffi::OsString;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// The largest compressed entry goodflows saves.
pub const MAX_ENTRY_BYTES: u64 = 5_000_000_000;
const HEADER_NAME: &str = "goodflows-cache.json";

/// What an archive holds, and where it goes back.
pub struct Declared<'a> {
    pub workdir: &'a Path,
    /// The home directory of the user running the job; required only for
    /// `~/` paths.
    pub home: Option<&'a Path>,
    /// The cache's `paths` as declared.
    pub paths: &'a [String],
}

#[derive(Serialize, Deserialize)]
struct Header {
    format: u32,
    key: String,
    partial: String,
    paths: Vec<String>,
    size: u64,
}

/// A written archive.
#[derive(Debug, Clone)]
pub struct Written {
    pub bytes: u64,
    pub sha256: String,
    /// When parts were requested: the part size and the base64 MD5 of each
    /// part-sized slice of the archive, in order, the last one shorter.
    pub part_md5: Option<(u64, Vec<String>)>,
}

/// The archive grew past its cap; nothing usable was written.
#[derive(Debug)]
pub struct TooLarge;

impl std::fmt::Display for TooLarge {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "the archive exceeds {MAX_ENTRY_BYTES} bytes compressed")
    }
}

impl std::error::Error for TooLarge {}

/// The work was abandoned because its time bound ran out.
#[derive(Debug)]
pub struct Aborted;

impl std::fmt::Display for Aborted {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("abandoned at the cache's time limit")
    }
}

impl std::error::Error for Aborted {}

/// Fail with [`Aborted`] once `abort` is set.
pub fn check_abort(abort: &AtomicBool) -> Result<()> {
    check(abort)
}

fn check(abort: &AtomicBool) -> Result<()> {
    if abort.load(Ordering::Relaxed) {
        return Err(Aborted.into());
    }
    Ok(())
}

/// The declared `~/` directory's components below home.
fn home_parts(path: &str) -> Option<Vec<&str>> {
    path.strip_prefix("~/").map(glob::segments)
}

/// Whether a symlink at `entry` (components below its root) pointing to
/// `target` resolves inside `prefix` (components below the same root).
/// `is_link` names the other symlinks of the same root: the target may end
/// at one, but never pass through one, since a `..` after a link resolves
/// from wherever that link points, not from its text.
fn link_stays(
    entry: &[OsString],
    target: &Path,
    prefix: &[OsString],
    is_link: &dyn Fn(&[OsString]) -> bool,
) -> bool {
    if target.is_absolute() || target.as_os_str().is_empty() {
        return false;
    }
    let mut stack: Vec<OsString> = entry[..entry.len().saturating_sub(1)].to_vec();
    let mut descended = false;
    for component in target.components() {
        // The path so far is traversed as a directory.
        if !stack.is_empty() && is_link(&stack) {
            return false;
        }
        match component {
            Component::Normal(part) => {
                descended = true;
                stack.push(part.to_owned());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                // `..` only leads: after a name it would resolve from wherever
                // that name points, and on a case-insensitive file system `B`
                // can be the archived link `b`, which text comparison misses.
                if descended || stack.pop().is_none() {
                    return false;
                }
            }
            _ => return false,
        }
    }
    stack.len() >= prefix.len() && stack[..prefix.len()] == *prefix
}

/// Whether `name` (an archive path) or one of its ancestors is in `links`,
/// other than `name` itself.
fn under_link(name: &Path, links: &std::collections::HashSet<PathBuf>) -> bool {
    name.ancestors().skip(1).any(|a| links.contains(a))
}

/// `is_link` for [`link_stays`]: whether `root`/`parts` is in `links`.
fn link_test<'a>(
    root: &'a Path,
    links: &'a std::collections::HashSet<PathBuf>,
) -> impl Fn(&[OsString]) -> bool + 'a {
    move |parts| links.contains(&parts.iter().fold(root.to_path_buf(), |p, c| p.join(c)))
}

/// The first component of an archive path: `workdir` or `home`.
fn root_of(name: &Path) -> &Path {
    Path::new(
        name.components()
            .next()
            .map_or("".as_ref(), |c| c.as_os_str()),
    )
}

fn os_parts(path: &Path) -> Vec<OsString> {
    path.components()
        .map(|c| c.as_os_str().to_owned())
        .collect()
}

/// The MD5 of each `size`-byte slice of a stream, for multipart uploads.
struct PartMd5 {
    size: u64,
    current: md5::Md5,
    /// Bytes fed to `current`.
    filled: u64,
    done: Vec<String>,
}

impl PartMd5 {
    fn new(size: u64) -> Self {
        Self {
            size,
            current: md5::Md5::new(),
            filled: 0,
            done: Vec::new(),
        }
    }

    fn update(&mut self, mut bytes: &[u8]) {
        while !bytes.is_empty() {
            let take = (self.size - self.filled).min(bytes.len() as u64) as usize;
            self.current.update(&bytes[..take]);
            self.filled += take as u64;
            bytes = &bytes[take..];
            if self.filled == self.size {
                self.close();
            }
        }
    }

    fn close(&mut self) {
        use base64::Engine;
        let digest = std::mem::take(&mut self.current).finalize();
        self.done
            .push(base64::engine::general_purpose::STANDARD.encode(digest));
        self.filled = 0;
    }

    /// The part size and the digests, finishing a short last part.
    fn finish(mut self) -> (u64, Vec<String>) {
        if self.filled > 0 {
            self.close();
        }
        (self.size, self.done)
    }
}

/// Counts and hashes the compressed bytes, failing past the cap.
struct Counting<W> {
    inner: W,
    hasher: Sha256,
    parts: Option<PartMd5>,
    count: u64,
    cap: u64,
    over: Arc<AtomicBool>,
    abort: Arc<AtomicBool>,
}

impl<W: Write> Write for Counting<W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        if self.abort.load(Ordering::Relaxed) {
            return Err(std::io::Error::other(Aborted));
        }
        if self.count + buf.len() as u64 > self.cap {
            self.over.store(true, Ordering::Relaxed);
            return Err(std::io::Error::other(TooLarge));
        }
        let written = self.inner.write(buf)?;
        self.hasher.update(&buf[..written]);
        if let Some(parts) = &mut self.parts {
            parts.update(&buf[..written]);
        }
        self.count += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

/// Reads exactly `remaining` bytes, failing if the file shrank meanwhile.
struct Exact<R> {
    inner: R,
    remaining: u64,
}

impl<R: Read> Read for Exact<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            return Ok(0);
        }
        let len = buf.len().min(self.remaining as usize);
        let read = self.inner.read(&mut buf[..len])?;
        if read == 0 {
            return Err(std::io::Error::other(
                "a file changed while it was archived",
            ));
        }
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// What identifies one version of a file system object: any write,
/// truncation, replacement or added or removed directory entry changes it.
#[derive(PartialEq, Eq)]
struct Snapshot {
    dev: u64,
    ino: u64,
    size: u64,
    mtime: (i64, i64),
    ctime: (i64, i64),
}

impl Snapshot {
    fn of(meta: &std::fs::Metadata) -> Self {
        use std::os::unix::fs::MetadataExt;
        Self {
            dev: meta.dev(),
            ino: meta.ino(),
            size: meta.size(),
            mtime: (meta.mtime(), meta.mtime_nsec()),
            ctime: (meta.ctime(), meta.ctime_nsec()),
        }
    }
}

/// One file system object to archive.
struct Item {
    source: PathBuf,
    /// Path inside the archive, starting with `workdir` or `home`.
    name: PathBuf,
    /// Components below the root, and the prefix a symlink must stay in.
    below_root: Vec<OsString>,
    link_prefix: Vec<OsString>,
}

fn collect(
    item: Item,
    items: &mut Vec<Item>,
    uncompressed: &mut u64,
    abort: &AtomicBool,
) -> Result<()> {
    check(abort)?;
    let meta = std::fs::symlink_metadata(&item.source)
        .with_context(|| format!("cannot read {}", item.source.display()))?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        let target = std::fs::read_link(&item.source)?;
        if !link_stays(&item.below_root, &target, &item.link_prefix, &|_| false) {
            bail!(
                "{} is a symlink that points outside its root",
                item.name.display()
            );
        }
        items.push(item);
    } else if kind.is_file() {
        *uncompressed += meta.len();
        items.push(item);
    } else if kind.is_dir() {
        let mut names: Vec<_> = std::fs::read_dir(&item.source)?
            .map(|e| e.map(|e| e.file_name()))
            .collect::<std::io::Result<_>>()?;
        names.sort();
        let (source, name, below, prefix) = (
            item.source.clone(),
            item.name.clone(),
            item.below_root.clone(),
            item.link_prefix.clone(),
        );
        items.push(item);
        for child in names {
            let mut below_root = below.clone();
            below_root.push(child.clone());
            collect(
                Item {
                    source: source.join(&child),
                    name: name.join(&child),
                    below_root,
                    link_prefix: prefix.clone(),
                },
                items,
                uncompressed,
                abort,
            )?;
        }
    } else {
        bail!("{} is a special file", item.name.display());
    }
    Ok(())
}

/// The objects to archive for every declared path that exists.
fn items(declared: &Declared, abort: &AtomicBool) -> Result<(Vec<Item>, u64)> {
    let mut items = Vec::new();
    let mut uncompressed = 0;
    for path in declared.paths {
        if let Some(parts) = home_parts(path) {
            let home = declared.home.context("HOME is not set")?;
            let rel: PathBuf = parts.iter().collect();
            let source = home.join(&rel);
            if std::fs::symlink_metadata(&source).is_err() {
                continue;
            }
            let below = os_parts(&rel);
            collect(
                Item {
                    source,
                    name: Path::new("home").join(&rel),
                    below_root: below.clone(),
                    link_prefix: below,
                },
                &mut items,
                &mut uncompressed,
                abort,
            )?;
        } else {
            for rel in glob::expand(declared.workdir, path) {
                collect(
                    Item {
                        source: declared.workdir.join(&rel),
                        name: Path::new("workdir").join(&rel),
                        below_root: os_parts(&rel),
                        link_prefix: Vec::new(),
                    },
                    &mut items,
                    &mut uncompressed,
                    abort,
                )?;
            }
        }
    }
    // Overlapping patterns can name a path twice.
    let mut seen = std::collections::HashSet::new();
    items.retain(|item| seen.insert(item.name.clone()));
    Ok((items, uncompressed))
}

/// Archive the declared paths into the new file `dest`. Fails with
/// [`TooLarge`] past `cap` compressed bytes and with [`Aborted`] when
/// `abort` is set. With `parts`, it also computes the MD5 of each
/// `parts`-byte slice of the archive in the same pass.
pub fn write(
    dest: &Path,
    declared: &Declared,
    key: &str,
    partial: &str,
    cap: u64,
    parts: Option<u64>,
    abort: &Arc<AtomicBool>,
) -> Result<Written> {
    let (items, uncompressed) = items(declared, abort)?;
    let links: std::collections::HashSet<PathBuf> = items
        .iter()
        .filter(|i| std::fs::symlink_metadata(&i.source).is_ok_and(|m| m.is_symlink()))
        .map(|i| i.name.clone())
        .collect();
    let over = Arc::new(AtomicBool::new(false));
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)?;
    let counting = Counting {
        inner: std::io::BufWriter::new(file),
        hasher: Sha256::new(),
        parts: parts.filter(|size| *size > 0).map(PartMd5::new),
        count: 0,
        cap,
        over: over.clone(),
        abort: abort.clone(),
    };
    let result = (|| -> Result<Counting<_>> {
        let gz = flate2::write::GzEncoder::new(counting, flate2::Compression::default());
        let mut builder = tar::Builder::new(gz);
        builder.follow_symlinks(false);
        let header = serde_json::to_vec(&Header {
            format: super::key::FORMAT_VERSION,
            key: key.into(),
            partial: partial.into(),
            paths: declared.paths.to_vec(),
            size: uncompressed,
        })?;
        let mut entry = tar::Header::new_gnu();
        entry.set_size(header.len() as u64);
        entry.set_mode(0o644);
        entry.set_entry_type(tar::EntryType::Regular);
        builder.append_data(&mut entry, HEADER_NAME, header.as_slice())?;
        let mut archived = Vec::with_capacity(items.len());
        for item in &items {
            check(abort)?;
            if under_link(&item.name, &links) {
                bail!(
                    "{} is below a symlink it also archives",
                    item.name.display()
                );
            }
            let meta = std::fs::symlink_metadata(&item.source)?;
            archived.push(Snapshot::of(&meta));
            let mut header = tar::Header::new_gnu();
            header.set_metadata_in_mode(&meta, tar::HeaderMode::Complete);
            let kind = meta.file_type();
            if kind.is_symlink() {
                let target = std::fs::read_link(&item.source)?;
                let is_link = link_test(root_of(&item.name), &links);
                if !link_stays(&item.below_root, &target, &item.link_prefix, &is_link) {
                    bail!(
                        "{} is a symlink that points outside its root",
                        item.name.display()
                    );
                }
                header.set_size(0);
                builder.append_link(&mut header, &item.name, &target)?;
            } else if kind.is_dir() {
                header.set_size(0);
                builder.append_data(&mut header, &item.name, std::io::empty())?;
            } else if kind.is_file() {
                let file = std::fs::File::open(&item.source)?;
                let reader = Exact {
                    inner: file,
                    remaining: meta.len(),
                };
                builder.append_data(&mut header, &item.name, reader)?;
            } else {
                bail!("{} is a special file", item.name.display());
            }
        }
        // Another job in the workdir may write these paths meanwhile. A
        // tree that changed while it was read is not a snapshot of anything,
        // so it is never saved: every object must look as it did when it
        // was archived (a directory's times change when entries come or go).
        for (item, before) in items.iter().zip(&archived) {
            let now = std::fs::symlink_metadata(&item.source).ok();
            if now.as_ref().map(Snapshot::of).as_ref() != Some(before) {
                bail!(
                    "a file changed while it was archived: {}",
                    item.name.display()
                );
            }
        }
        let gz = builder.into_inner()?;
        let mut counting = gz.finish()?;
        counting.flush()?;
        Ok(counting)
    })();
    match result {
        Ok(counting) => {
            counting
                .inner
                .into_inner()
                .map_err(|e| e.into_error())?
                .sync_all()?;
            Ok(Written {
                bytes: counting.count,
                sha256: super::key_hex(counting.hasher.finalize()),
                part_md5: counting.parts.map(PartMd5::finish),
            })
        }
        Err(_) if over.load(Ordering::Relaxed) => Err(TooLarge.into()),
        Err(_) if abort.load(Ordering::Relaxed) => Err(Aborted.into()),
        Err(error) => Err(error),
    }
}

/// The size and sha256 of a file.
pub fn digest(path: &Path) -> Result<(u64, String)> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let bytes = std::io::copy(&mut file, &mut hasher)?;
    Ok((bytes, super::key_hex(hasher.finalize())))
}

fn utf8_parts(path: &Path) -> Option<Vec<&str>> {
    path.components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect()
}

/// Where an entry goes: its root and its components below the root.
enum Root {
    Workdir,
    Home,
}

/// Check one entry against the declared paths and return its root and the
/// path below it.
fn place(path: &Path, declared: &Declared) -> Result<(Root, PathBuf)> {
    if path.is_absolute() {
        bail!("the archive holds an absolute path");
    }
    if path
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("the archive holds a path with '..' or another special component");
    }
    let parts = utf8_parts(path).ok_or_else(|| anyhow!("the archive holds a non-UTF-8 path"))?;
    let (root, rest) = parts
        .split_first()
        .ok_or_else(|| anyhow!("the archive holds an empty path"))?;
    let rel: PathBuf = rest.iter().collect();
    let allowed = match *root {
        "workdir" => declared
            .paths
            .iter()
            .filter(|p| !p.starts_with("~/"))
            .any(|p| glob::within(&glob::segments(p), rest)),
        "home" => declared
            .paths
            .iter()
            .filter_map(|p| home_parts(p))
            .any(|declared| rest.len() >= declared.len() && rest[..declared.len()] == declared[..]),
        _ => false,
    };
    if !allowed {
        bail!(
            "the archive holds {}, which is outside the declared paths",
            crate::output::visible(&path.to_string_lossy())
        );
    }
    Ok((
        if *root == "workdir" {
            Root::Workdir
        } else {
            Root::Home
        },
        rel,
    ))
}

/// The prefix a symlink below `rel` in `root` must stay in.
fn link_prefix(root: &Root, rel: &Path, declared: &Declared) -> Vec<OsString> {
    match root {
        Root::Workdir => Vec::new(),
        Root::Home => {
            let parts = os_parts(rel);
            declared
                .paths
                .iter()
                .filter_map(|p| home_parts(p))
                .map(|d| d.iter().map(OsString::from).collect::<Vec<_>>())
                .filter(|d| parts.len() >= d.len() && parts[..d.len()] == d[..])
                .max_by_key(Vec::len)
                .unwrap_or_default()
        }
    }
}

fn remove_path(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// Verify `archive` against `size` and `sha256`, check every entry, then
/// replace each declared path the archive holds. Nothing outside a staging
/// directory is written unless every check passed.
pub fn extract(
    archive: &Path,
    size: u64,
    sha256: &str,
    declared: &Declared,
    abort: &AtomicBool,
) -> Result<()> {
    let (actual_size, actual_sha) = digest(archive)?;
    if actual_size != size || actual_sha != sha256 {
        bail!("the archive does not match its recorded size and sha256");
    }
    let open = || -> Result<tar::Archive<flate2::read::GzDecoder<std::fs::File>>> {
        let mut archive =
            tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(archive)?));
        // Permission bits only: setuid, setgid and sticky bits are dropped.
        archive.set_preserve_permissions(false);
        archive.set_preserve_mtime(true);
        archive.set_unpack_xattrs(false);
        Ok(archive)
    };
    // Check every entry before writing anything. Symlinks are known up
    // front, so an entry can be refused for passing through one wherever
    // it comes in the archive.
    struct Checked {
        path: PathBuf,
        kind: tar::EntryType,
        target: Option<PathBuf>,
    }
    let mut checked: Vec<Checked> = Vec::new();
    let mut tar = open()?;
    let mut first = true;
    for entry in tar.entries()? {
        check(abort)?;
        let mut entry = entry?;
        let path = entry.path()?.into_owned();
        if first {
            first = false;
            if path != Path::new(HEADER_NAME)
                || entry.header().entry_type() != tar::EntryType::Regular
            {
                bail!("the archive has no goodflows header");
            }
            let mut bytes = Vec::new();
            (&mut entry).take(1 << 20).read_to_end(&mut bytes)?;
            let header: Header =
                serde_json::from_slice(&bytes).context("the archive header is not valid")?;
            if header.format != super::key::FORMAT_VERSION {
                bail!("the archive has an unsupported format");
            }
            continue;
        }
        let kind = entry.header().entry_type();
        place(&path, declared)?;
        let target = match kind {
            tar::EntryType::Regular | tar::EntryType::Directory => None,
            tar::EntryType::Symlink => Some(
                entry
                    .link_name()?
                    .ok_or_else(|| anyhow!("a symlink has no target"))?
                    .into_owned(),
            ),
            _ => bail!("the archive holds an entry that is not a file, directory or symlink"),
        };
        checked.push(Checked { path, kind, target });
    }
    if first {
        bail!("the archive is empty");
    }
    let links: std::collections::HashSet<PathBuf> = checked
        .iter()
        .filter(|c| c.target.is_some())
        .map(|c| c.path.clone())
        .collect();
    let mut seen = std::collections::HashSet::new();
    let mut needs_home = false;
    for entry in &checked {
        let visible = || crate::output::visible(&entry.path.to_string_lossy()).into_owned();
        if !seen.insert(&entry.path) {
            bail!("the archive holds {} twice", visible());
        }
        if under_link(&entry.path, &links) {
            bail!(
                "the archive writes through a symlink it holds: {}",
                visible()
            );
        }
        let (root, rel) = place(&entry.path, declared)?;
        needs_home |= matches!(root, Root::Home);
        if let Some(target) = &entry.target {
            let is_link = link_test(root_of(&entry.path), &links);
            if !link_stays(
                &os_parts(&rel),
                target,
                &link_prefix(&root, &rel, declared),
                &is_link,
            ) {
                bail!(
                    "the archive holds a symlink that points outside its root: {}",
                    visible()
                );
            }
        }
    }
    let home = if needs_home {
        Some(declared.home.context("HOME is not set")?)
    } else {
        None
    };
    // Unpack into staging directories on the same file systems.
    let state = declared.workdir.join(".goodflows/cache-restore");
    crate::fsutil::create_state_dir(&state)?;
    let work_stage = tempfile::Builder::new()
        .prefix("restore-")
        .tempdir_in(&state)?;
    let home_stage = match home {
        Some(home) => Some(
            tempfile::Builder::new()
                .prefix(".goodflows-restore-")
                .tempdir_in(home)?,
        ),
        None => None,
    };
    let stage_of = |path: &Path| -> Result<PathBuf> {
        let (root, rel) = place(path, declared)?;
        let stage = match root {
            Root::Workdir => work_stage.path(),
            Root::Home => home_stage.as_ref().expect("home stage exists").path(),
        };
        Ok(stage.join(rel))
    };
    let mut dirs: Vec<(PathBuf, u32)> = Vec::new();
    let mut tar = open()?;
    // Read entries again; each must match the one checked above.
    for (index, entry) in tar.entries()?.enumerate() {
        check(abort)?;
        let mut entry = entry?;
        if index == 0 {
            continue;
        }
        let expected = checked
            .get(index - 1)
            .context("the archive changed while it was restored")?;
        let path = entry.path()?.into_owned();
        let kind = entry.header().entry_type();
        if path != expected.path || kind != expected.kind {
            bail!("the archive changed while it was restored");
        }
        // Symlinks come last, so nothing is written through one.
        if kind == tar::EntryType::Symlink {
            continue;
        }
        let dest = stage_of(&path)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        if kind == tar::EntryType::Directory {
            // Modes are applied last, so read-only directories still fill.
            std::fs::create_dir_all(&dest)?;
            dirs.push((dest, entry.header().mode().unwrap_or(0o755)));
        } else {
            entry.unpack(&dest)?;
        }
    }
    for entry in checked.iter().filter(|c| c.target.is_some()) {
        check(abort)?;
        let dest = stage_of(&entry.path)?;
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::os::unix::fs::symlink(entry.target.as_ref().expect("a symlink"), &dest)?;
    }
    for (dir, mode) in dirs.iter().rev() {
        use std::os::unix::fs::PermissionsExt;
        // No setuid, setgid or sticky bits from a cache.
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(mode & 0o777))?;
    }
    check(abort)?;
    // Swap: each declared path the archive holds replaces the live one.
    // Staged paths are first brought next to their targets (copied when a
    // symlink under `~/` leads to another file system); then live paths are
    // renamed aside, also next to themselves, and put back if any move fails.
    let mut moves = Moves::default();
    let swapped = swap(
        declared,
        home,
        work_stage.path(),
        home_stage.as_ref().map(|s| s.path()),
        &mut moves,
        abort,
    );
    let Moves {
        placed,
        aside,
        incoming,
    } = moves;
    if let Err(error) = swapped {
        for path in &incoming {
            let _ = remove_path(path);
        }
        for path in placed.iter().rev() {
            let _ = remove_path(path);
        }
        for (moved, original) in aside.iter().rev() {
            let _ = std::fs::rename(moved, original);
        }
        return Err(error);
    }
    for (moved, _) in aside {
        let _ = remove_path(&moved);
    }
    Ok(())
}

const ASIDE_MARK: &str = ".goodflows-aside-";

/// 8 hex digits naming this host (or container) and its boot, for aside
/// names: a pid means something only on the same host since the same boot.
fn aside_tags() -> (String, String) {
    let tag = |text: &[u8]| super::key_hex(Sha256::digest(text))[..8].to_owned();
    let mut host = [0u8; 256];
    // SAFETY: the buffer is valid for its length; gethostname NUL-terminates
    // within it or truncates, and the slice below stops at the first NUL.
    let named = unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) } == 0;
    let host = if named {
        &host[..host.iter().position(|b| *b == 0).unwrap_or(host.len())]
    } else {
        &[][..]
    };
    let boot = std::fs::read("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    (tag(host), tag(boot.trim_ascii()))
}

/// Move `target` to a hidden sibling and return where it went. The name
/// carries this host, boot and process id
/// (`.NAME.goodflows-aside-<host>-<boot>-<pid>-<uuid>`), so a later restore
/// on the same host can remove what a killed one left behind.
fn move_aside(target: &Path) -> Result<Option<PathBuf>> {
    if std::fs::symlink_metadata(target).is_err() {
        return Ok(None);
    }
    let moved = hidden_sibling(target)?;
    std::fs::rename(target, &moved)
        .with_context(|| format!("cannot move {} aside", target.display()))?;
    Ok(Some(moved))
}

/// A fresh hidden name next to `target`, in the aside format, so a later
/// restore prunes it when a killed process left it behind.
fn hidden_sibling(target: &Path) -> Result<PathBuf> {
    let name = target.file_name().context("a declared path has no name")?;
    let mut hidden = OsString::from(".");
    hidden.push(name);
    let (host, boot) = aside_tags();
    hidden.push(format!(
        "{ASIDE_MARK}{host}-{boot}-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4().simple()
    ));
    Ok(target.with_file_name(hidden))
}

/// Whether an aside name's owner is gone. Another host's or container's
/// asides are never touched (its pids mean nothing here); this host's from
/// an earlier boot always are.
fn stale_aside(name: &str) -> bool {
    let Some((_, rest)) = name.split_once(ASIDE_MARK) else {
        return false;
    };
    let mut fields = rest.split('-');
    let (Some(host), Some(boot), Some(pid)) = (fields.next(), fields.next(), fields.next()) else {
        return false;
    };
    let (own_host, own_boot) = aside_tags();
    if host != own_host {
        return false;
    }
    if boot != own_boot {
        return true;
    }
    let Ok(pid) = pid.parse::<i32>() else {
        return false;
    };
    if pid <= 0 {
        return false;
    }
    // SAFETY: signal 0 only checks that the process exists.
    let alive = unsafe { libc::kill(pid, 0) } == 0
        || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH);
    !alive
}

/// Remove asides of the declared paths that a killed restore left behind
/// (between moving a live path aside and finishing the swap). Best effort.
pub fn prune_asides(declared: &Declared) {
    for path in declared.paths {
        let (base, pattern) = match home_parts(path) {
            Some(parts) => match declared.home {
                Some(home) => (home, parts),
                None => continue,
            },
            None => (declared.workdir, glob::segments(path)),
        };
        let Some((last, parents)) = pattern.split_last() else {
            continue;
        };
        let mut aside: Vec<String> = parents.iter().map(|p| p.to_string()).collect();
        aside.push(format!(".{last}{ASIDE_MARK}*"));
        for rel in glob::expand(base, &aside.join("/")) {
            let stale = rel
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(stale_aside);
            if stale {
                let _ = remove_path(&base.join(rel));
            }
        }
    }
}

/// What a swap moved so far, for its rollback.
#[derive(Default)]
struct Moves {
    /// Targets now holding the restored tree.
    placed: Vec<PathBuf>,
    /// Live paths moved aside: (where they went, where they were).
    aside: Vec<(PathBuf, PathBuf)>,
    /// Staged paths brought next to their targets, not yet placed.
    incoming: Vec<PathBuf>,
}

fn swap(
    declared: &Declared,
    home: Option<&Path>,
    work_stage: &Path,
    home_stage: Option<&Path>,
    moves: &mut Moves,
    abort: &AtomicBool,
) -> Result<()> {
    let Moves {
        placed,
        aside,
        incoming,
    } = moves;
    // Every staged path and every live path it replaces, across all the
    // declared paths: declared paths may nest (`deps` and `deps/sub`), so
    // each is handled once, outermost first, and a path inside one already
    // handled is skipped.
    let mut staged: Vec<(PathBuf, PathBuf)> = Vec::new();
    let mut live: Vec<PathBuf> = Vec::new();
    // Each target with the root it must stay under.
    let mut rooted: Vec<(PathBuf, PathBuf)> = Vec::new();
    for path in declared.paths {
        let (stage, live_base, rels): (&Path, &Path, Vec<PathBuf>) =
            if let Some(parts) = home_parts(path) {
                let Some(stage) = home_stage else { continue };
                let rel: PathBuf = parts.iter().collect();
                let rels = if std::fs::symlink_metadata(stage.join(&rel)).is_ok() {
                    vec![rel]
                } else {
                    Vec::new()
                };
                (stage, home.expect("home exists"), rels)
            } else {
                (work_stage, declared.workdir, glob::expand(work_stage, path))
            };
        if rels.is_empty() {
            continue;
        }
        let first = live.len();
        if path.starts_with("~/") {
            live.extend(rels.iter().map(|rel| live_base.join(rel)));
        } else {
            live.extend(
                glob::expand(live_base, path)
                    .iter()
                    .map(|rel| live_base.join(rel)),
            );
        }
        // Links under `~/` are the owner's own (a checkout or an archive
        // cannot make them), so only workdir targets are checked.
        if !path.starts_with("~/") {
            rooted.extend(
                live[first..]
                    .iter()
                    .cloned()
                    .chain(rels.iter().map(|rel| live_base.join(rel)))
                    .map(|target| (live_base.to_path_buf(), target)),
            );
        }
        staged.extend(
            rels.into_iter()
                .map(|rel| (stage.join(&rel), live_base.join(rel))),
        );
    }
    // Before anything moves: a symlinked ancestor in the workdir would
    // carry the swap outside it.
    for (root, target) in &rooted {
        check_ancestors(root, target)?;
    }
    let depth = |path: &Path| path.components().count();
    live.sort_by_key(|target| depth(target));
    staged.sort_by_key(|(_, target)| depth(target));
    // Outermost first; a path inside one already kept comes with it.
    let mut kept: Vec<(PathBuf, PathBuf)> = Vec::new();
    for (from, target) in staged {
        if !kept.iter().any(|(_, k)| target.starts_with(k)) {
            kept.push((from, target));
        }
    }
    // Bring each staged path next to its target first, so the final rename
    // stays on one file system even when a symlinked directory under `~/`
    // leads to another disk. A failure here leaves every live path alone.
    let mut near: Vec<(PathBuf, PathBuf)> = Vec::with_capacity(kept.len());
    for (from, target) in kept {
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let hop = hidden_sibling(&target)?;
        let moved = move_across(&from, &hop, abort);
        if moved.is_err() {
            let _ = remove_path(&hop);
        }
        moved.with_context(|| format!("cannot stage {}", target.display()))?;
        incoming.push(hop.clone());
        near.push((hop, target));
    }
    for target in live {
        if !aside.iter().any(|(_, o)| target.starts_with(o)) {
            if let Some(moved) = move_aside(&target)? {
                aside.push((moved, target));
            }
        }
    }
    for (hop, target) in near {
        std::fs::rename(&hop, &target)
            .with_context(|| format!("cannot move {} into place", target.display()))?;
        incoming.retain(|i| *i != hop);
        placed.push(target);
    }
    Ok(())
}

/// Rename `from` to `to`, or, when they are on different file systems,
/// copy the tree (keeping symlinks, modes and modification times) and
/// remove `from`.
fn move_across(from: &Path, to: &Path, abort: &AtomicBool) -> Result<()> {
    match std::fs::rename(from, to) {
        Err(error) if error.raw_os_error() == Some(libc::EXDEV) => {
            copy_tree(from, to, abort)?;
            let _ = remove_path(from);
            Ok(())
        }
        other => Ok(other?),
    }
}

fn copy_tree(from: &Path, to: &Path, abort: &AtomicBool) -> Result<()> {
    check(abort)?;
    let meta = std::fs::symlink_metadata(from)?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        std::os::unix::fs::symlink(std::fs::read_link(from)?, to)?;
        return Ok(());
    }
    if kind.is_dir() {
        std::fs::create_dir(to)?;
        for entry in std::fs::read_dir(from)? {
            let entry = entry?;
            copy_tree(&entry.path(), &to.join(entry.file_name()), abort)?;
        }
    } else if kind.is_file() {
        std::fs::copy(from, to)?;
    } else {
        bail!("{} is not a file, directory or symlink", from.display());
    }
    // Build tools compare modification times, so keep them; the mode last,
    // so a read-only directory still fills.
    let times = std::fs::FileTimes::new().set_modified(meta.modified()?);
    std::fs::File::open(to)?.set_times(times)?;
    std::fs::set_permissions(to, meta.permissions())?;
    Ok(())
}

/// Refuse `target` when a directory between `root` and it is a symlink:
/// moving it aside or renaming into it would replace a path outside `root`.
/// A missing ancestor is fine, as `create_dir_all` makes real directories.
fn check_ancestors(root: &Path, target: &Path) -> Result<()> {
    let rel = target
        .strip_prefix(root)
        .with_context(|| format!("{} is outside its root", target.display()))?;
    let parts: Vec<_> = rel.components().collect();
    let mut at = root.to_path_buf();
    for part in &parts[..parts.len().saturating_sub(1)] {
        at.push(part);
        match std::fs::symlink_metadata(&at) {
            Ok(meta) if meta.file_type().is_symlink() => bail!(
                "cannot restore {}: {} is a symlink",
                target.display(),
                at.display()
            ),
            Ok(_) => {}
            Err(_) => break,
        }
    }
    Ok(())
}

/// Read only the JSON header of an archive, for tests.
#[cfg(test)]
fn header_of(path: &Path) -> Result<Header> {
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(std::fs::File::open(path)?));
    let mut entry = tar.entries()?.next().context("empty")??;
    let mut bytes = Vec::new();
    entry.read_to_end(&mut bytes)?;
    Ok(serde_json::from_slice(&bytes)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn abort() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(false))
    }

    fn declared<'a>(workdir: &'a Path, home: &'a Path, paths: &'a [String]) -> Declared<'a> {
        Declared {
            workdir,
            home: Some(home),
            paths,
        }
    }

    fn save(workdir: &Path, home: &Path, paths: &[String], out: &Path) -> Result<Written> {
        write(
            out,
            &declared(workdir, home, paths),
            &"k".repeat(64),
            &"p".repeat(64),
            MAX_ENTRY_BYTES,
            None,
            &abort(),
        )
    }

    #[test]
    fn round_trip_keeps_internal_symlinks_and_executable_bits() {
        let (src, home, dest, out) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let w = src.path();
        std::fs::create_dir_all(w.join("deps/bin")).unwrap();
        std::fs::write(w.join("deps/bin/tool"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(
            w.join("deps/bin/tool"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        std::fs::write(w.join("deps/data"), "data").unwrap();
        std::os::unix::fs::symlink("bin/tool", w.join("deps/link")).unwrap();
        std::os::unix::fs::symlink("../mix.lock", w.join("deps/up")).unwrap();
        std::fs::create_dir_all(home.path().join(".cargo/registry/x")).unwrap();
        std::fs::write(home.path().join(".cargo/registry/x/f"), "reg").unwrap();
        let paths = vec!["deps".to_owned(), "~/.cargo/registry".to_owned()];
        let archive = out.path().join("a.tgz");
        let written = save(w, home.path(), &paths, &archive).unwrap();
        assert_eq!(header_of(&archive).unwrap().paths, paths);
        let fresh_home = tempfile::tempdir().unwrap();
        extract(
            &archive,
            written.bytes,
            &written.sha256,
            &declared(dest.path(), fresh_home.path(), &paths),
            &AtomicBool::new(false),
        )
        .unwrap();
        let d = dest.path();
        assert_eq!(
            std::fs::read_to_string(d.join("deps/data")).unwrap(),
            "data"
        );
        let mode = std::fs::metadata(d.join("deps/bin/tool"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o755);
        assert_eq!(
            std::fs::read_link(d.join("deps/link")).unwrap(),
            Path::new("bin/tool")
        );
        assert_eq!(
            std::fs::read_link(d.join("deps/up")).unwrap(),
            Path::new("../mix.lock")
        );
        assert_eq!(
            std::fs::read_to_string(fresh_home.path().join(".cargo/registry/x/f")).unwrap(),
            "reg"
        );
        // Staging directories are gone.
        let leftovers: Vec<_> = std::fs::read_dir(fresh_home.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftovers, [OsString::from(".cargo")]);
    }

    #[test]
    fn writer_refuses_external_symlinks_special_files_and_oversize() {
        let (src, home, out) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let w = src.path();
        std::fs::create_dir(w.join("deps")).unwrap();
        std::os::unix::fs::symlink("../../etc/passwd", w.join("deps/escape")).unwrap();
        let paths = vec!["deps".to_owned()];
        let error = save(w, home.path(), &paths, &out.path().join("1")).unwrap_err();
        assert!(
            format!("{error:#}").contains("outside its root"),
            "{error:#}"
        );
        std::fs::remove_file(w.join("deps/escape")).unwrap();
        std::os::unix::fs::symlink("/etc/passwd", w.join("deps/absolute")).unwrap();
        assert!(save(w, home.path(), &paths, &out.path().join("2")).is_err());
        std::fs::remove_file(w.join("deps/absolute")).unwrap();
        // A home symlink may not leave its declared directory.
        std::fs::create_dir_all(home.path().join(".npm")).unwrap();
        std::os::unix::fs::symlink("../.ssh", home.path().join(".npm/k")).unwrap();
        let home_paths = vec!["~/.npm".to_owned()];
        assert!(save(w, home.path(), &home_paths, &out.path().join("3")).is_err());
        let fifo = w.join("deps/fifo");
        let c = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path.
        assert_eq!(unsafe { libc::mkfifo(c.as_ptr(), 0o600) }, 0);
        let error = save(w, home.path(), &paths, &out.path().join("4")).unwrap_err();
        assert!(format!("{error:#}").contains("special file"), "{error:#}");
        std::fs::remove_file(&fifo).unwrap();
        std::fs::write(w.join("deps/big"), vec![7u8; 100_000]).unwrap();
        let error = write(
            &out.path().join("5"),
            &declared(w, home.path(), &paths),
            "k",
            "p",
            100,
            None,
            &abort(),
        )
        .unwrap_err();
        assert!(error.is::<TooLarge>(), "{error:#}");
    }

    /// A gzip tar built by hand, with the goodflows header first.
    fn malicious(
        entries: &[(&str, tar::EntryType, &str)],
    ) -> (tempfile::TempDir, PathBuf, u64, String) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("evil.tgz");
        let file = std::fs::File::create(&path).unwrap();
        let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
            file,
            flate2::Compression::default(),
        ));
        let header = br#"{"format":1,"key":"k","partial":"p","paths":["deps"],"size":0}"#;
        let mut h = tar::Header::new_gnu();
        h.set_size(header.len() as u64);
        h.set_entry_type(tar::EntryType::Regular);
        h.set_mode(0o644);
        builder
            .append_data(&mut h, HEADER_NAME, &header[..])
            .unwrap();
        for (name, kind, data) in entries {
            let mut h = tar::Header::new_old();
            h.set_entry_type(*kind);
            h.set_mode(0o644);
            if *kind == tar::EntryType::Symlink {
                h.set_size(0);
                h.set_link_name(data).unwrap();
                // Set the raw name so `..` survives the builder's checks.
                h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                h.set_cksum();
                builder.append(&h, std::io::empty()).unwrap();
            } else {
                h.set_size(data.len() as u64);
                h.as_old_mut().name[..name.len()].copy_from_slice(name.as_bytes());
                h.set_cksum();
                builder.append(&h, data.as_bytes()).unwrap();
            }
        }
        builder.into_inner().unwrap().finish().unwrap();
        let (size, sha) = digest(&path).unwrap();
        (dir, path, size, sha)
    }

    #[test]
    fn extractor_refuses_hostile_archives_before_writing() {
        let paths = vec!["deps".to_owned()];
        for (entries, message) in [
            (
                vec![("workdir/../.bashrc", tar::EntryType::Regular, "x")],
                "'..'",
            ),
            (vec![("/etc/x", tar::EntryType::Regular, "x")], "absolute"),
            (
                vec![("workdir/other/x", tar::EntryType::Regular, "x")],
                "outside the declared paths",
            ),
            (
                vec![("home/.ssh/id", tar::EntryType::Regular, "x")],
                "outside the declared paths",
            ),
            (
                vec![("workdir/deps/l", tar::EntryType::Symlink, "../../x")],
                "outside its root",
            ),
            (
                vec![("workdir/deps/l", tar::EntryType::Symlink, "/etc/passwd")],
                "outside its root",
            ),
            (
                vec![("workdir/deps/h", tar::EntryType::Link, "workdir/deps/x")],
                "not a file",
            ),
        ] {
            let (_dir, archive, size, sha) = malicious(&[
                ("workdir/deps/ok", tar::EntryType::Regular, "fine"),
                entries[0],
            ]);
            let (work, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
            let error = extract(
                &archive,
                size,
                &sha,
                &declared(work.path(), home.path(), &paths),
                &AtomicBool::new(false),
            )
            .unwrap_err();
            assert!(
                format!("{error:#}").contains(message),
                "{message}: {error:#}"
            );
            assert!(
                !work.path().join("deps").exists(),
                "{message}: wrote before checking"
            );
            assert!(!work.path().join(".bashrc").exists());
        }
    }

    fn try_extract(entries: &[(&str, tar::EntryType, &str)]) -> (tempfile::TempDir, Result<()>) {
        let (_dir, archive, size, sha) = malicious(entries);
        let root = tempfile::tempdir().unwrap();
        let work = root.path().join("a/b/c/d/work");
        std::fs::create_dir_all(&work).unwrap();
        let paths = vec!["deps".to_owned()];
        let result = extract(
            &archive,
            size,
            &sha,
            &declared(&work, &work, &paths),
            &AtomicBool::new(false),
        );
        (root, result)
    }

    #[test]
    fn nested_declared_paths_restore_the_whole_tree() {
        let (src, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        std::fs::create_dir_all(src.path().join("deps/sub")).unwrap();
        std::fs::write(src.path().join("deps/sub/f"), "f").unwrap();
        std::fs::write(src.path().join("deps/g"), "g").unwrap();
        for paths in [["deps/sub", "deps"], ["deps", "deps/sub"]] {
            let paths: Vec<String> = paths.iter().map(|p| p.to_string()).collect();
            let archive = out.path().join(paths.join(",").replace('/', "_"));
            let written = save(src.path(), src.path(), &paths, &archive).unwrap();
            // A fresh tree, and one with stale content in both paths.
            for stale in [false, true] {
                let dest = tempfile::tempdir().unwrap();
                let d = dest.path();
                if stale {
                    std::fs::create_dir_all(d.join("deps/sub")).unwrap();
                    std::fs::write(d.join("deps/old"), "x").unwrap();
                    std::fs::write(d.join("deps/sub/old"), "x").unwrap();
                }
                extract(
                    &archive,
                    written.bytes,
                    &written.sha256,
                    &declared(d, d, &paths),
                    &AtomicBool::new(false),
                )
                .unwrap();
                assert_eq!(std::fs::read_to_string(d.join("deps/sub/f")).unwrap(), "f");
                assert_eq!(std::fs::read_to_string(d.join("deps/g")).unwrap(), "g");
                assert!(!d.join("deps/old").exists() && !d.join("deps/sub/old").exists());
                let names: Vec<_> = std::fs::read_dir(d)
                    .unwrap()
                    .flatten()
                    .map(|e| e.file_name().into_string().unwrap())
                    .filter(|n| n != ".goodflows")
                    .collect();
                assert_eq!(names, ["deps"], "{paths:?} stale={stale}");
            }
        }
    }

    #[test]
    fn a_link_target_may_not_climb_after_a_name() {
        use tar::EntryType::{Regular, Symlink};
        // On a case-insensitive file system `B` is the link `b`.
        let (root, result) = try_extract(&[
            ("workdir/deps/b", Symlink, ".."),
            ("workdir/deps/c", Symlink, "B/../.."),
        ]);
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("outside its root"), "{error}");
        assert!(!root.path().join("a/b/c/d/work/deps").exists());
        // Leading `..` is still fine.
        let (_root, result) = try_extract(&[
            ("workdir/deps/x/f", Regular, "data"),
            ("workdir/deps/y/l", Symlink, "../x/f"),
        ]);
        result.unwrap();
    }

    #[test]
    fn stale_asides_of_killed_restores_are_pruned() {
        let (work, home) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let mut child = std::process::Command::new("true").spawn().unwrap();
        let dead = child.id();
        child.wait().unwrap();
        let own = std::process::id();
        let (host, boot) = aside_tags();
        let tagged = |base: &Path, name: &str, host: &str, boot: &str, pid: u32| {
            let path = base.join(format!(".{name}{ASIDE_MARK}{host}-{boot}-{pid}-0123abcd"));
            std::fs::create_dir_all(path.join("inner")).unwrap();
            path
        };
        let aside = |base: &Path, name: &str, pid: u32| tagged(base, name, &host, &boot, pid);
        let gone = [
            aside(work.path(), "deps", dead),
            aside(&work.path().join("app"), "node_modules", dead),
            aside(&home.path().join(".cargo"), "registry", dead),
            // This host, an earlier boot: even a live-looking pid is stale.
            tagged(work.path(), "deps", &host, "00000000", own),
        ];
        let kept = [
            aside(work.path(), "deps", own),
            aside(work.path(), "other", dead),
            // Another host or container sharing the directory: its pids
            // mean nothing here, so even a dead-looking one is kept.
            tagged(work.path(), "deps", "00000000", &boot, dead),
        ];
        let paths = vec![
            "deps".to_owned(),
            "*/node_modules".to_owned(),
            "~/.cargo/registry".to_owned(),
        ];
        prune_asides(&declared(work.path(), home.path(), &paths));
        for path in gone {
            assert!(!path.exists(), "{}", path.display());
        }
        for path in kept {
            assert!(path.exists(), "{}", path.display());
        }
    }

    #[test]
    fn extractor_refuses_chained_symlinks_that_leave_the_workdir() {
        use tar::EntryType::{Regular, Symlink};
        // Each link's text stays in the workdir; resolved, `f` is five
        // directories up.
        let chain = [
            ("workdir/deps/b", Symlink, ".."),
            ("workdir/deps/c", Symlink, "b/.."),
            ("workdir/deps/d", Symlink, "c/.."),
            ("workdir/deps/e", Symlink, "d/.."),
            ("workdir/deps/f", Symlink, "e/.."),
        ];
        let mut entries = chain.to_vec();
        entries.push(("workdir/deps/f/PWNED", Regular, "pwned"));
        let (root, result) = try_extract(&entries);
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("symlink"), "{error}");
        let written: Vec<_> = crate::cache::glob::expand(root.path(), "**/PWNED");
        assert!(written.is_empty(), "{written:?}");
        assert!(!root.path().join("a/b/c/d/work/deps").exists());
        // The links alone are refused too: `c` resolves through `b`.
        let (_root, result) = try_extract(&chain);
        let error = format!("{:#}", result.unwrap_err());
        assert!(error.contains("outside its root"), "{error}");
    }

    #[test]
    fn extractor_refuses_entries_below_an_archived_symlink_in_either_order() {
        use tar::EntryType::{Regular, Symlink};
        for entries in [
            [
                ("workdir/deps/l", Symlink, "x"),
                ("workdir/deps/l/file", Regular, "data"),
            ],
            [
                ("workdir/deps/l/file", Regular, "data"),
                ("workdir/deps/l", Symlink, "x"),
            ],
        ] {
            let (root, result) = try_extract(&entries);
            let error = format!("{:#}", result.unwrap_err());
            assert!(error.contains("through a symlink"), "{error}");
            assert!(!root.path().join("a/b/c/d/work/deps").exists());
        }
        // A link to a sibling link, or one that ends at an archived link, is fine.
        let (root, result) = try_extract(&[
            ("workdir/deps/x/file", Regular, "data"),
            ("workdir/deps/l", Symlink, "x"),
            ("workdir/deps/m", Symlink, "l"),
        ]);
        result.unwrap();
        let deps = root.path().join("a/b/c/d/work/deps");
        assert_eq!(
            std::fs::read_to_string(deps.join("m/file")).unwrap(),
            "data"
        );
    }

    #[test]
    fn the_writer_refuses_a_path_below_an_archived_symlink() {
        let (src, out) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
        let w = src.path();
        std::fs::create_dir_all(w.join("deps/real")).unwrap();
        std::fs::write(w.join("deps/real/f"), "x").unwrap();
        std::os::unix::fs::symlink("real", w.join("deps/l")).unwrap();
        // `deps/*/f` names `deps/l/f` (through the link) and `deps/real/f`.
        let paths = vec!["deps/l".to_owned(), "deps/*/f".to_owned()];
        let error = save(w, w, &paths, &out.path().join("a")).unwrap_err();
        assert!(
            format!("{error:#}").contains("below a symlink"),
            "{error:#}"
        );
    }

    #[test]
    fn a_restore_drops_setuid_setgid_and_sticky_bits() {
        let (src, out, dest) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        let w = src.path();
        std::fs::create_dir_all(w.join("deps/dir")).unwrap();
        std::fs::write(w.join("deps/tool"), "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(w.join("deps/tool"), std::fs::Permissions::from_mode(0o4755))
            .unwrap();
        std::fs::set_permissions(w.join("deps/dir"), std::fs::Permissions::from_mode(0o3775))
            .unwrap();
        let paths = vec!["deps".to_owned()];
        let archive = out.path().join("a");
        let written = save(w, w, &paths, &archive).unwrap();
        extract(
            &archive,
            written.bytes,
            &written.sha256,
            &declared(dest.path(), dest.path(), &paths),
            &AtomicBool::new(false),
        )
        .unwrap();
        let mode = |p: &str| {
            std::fs::symlink_metadata(dest.path().join(p))
                .unwrap()
                .permissions()
                .mode()
                & 0o7777
        };
        assert_eq!(mode("deps/tool"), 0o755);
        assert_eq!(mode("deps/dir"), 0o775);
    }

    #[test]
    fn a_restore_never_swaps_through_a_symlinked_destination_ancestor() {
        let (src, out, dest, outside) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir_all(src.path().join("deps/lib")).unwrap();
        std::fs::write(src.path().join("deps/lib/f"), "cached").unwrap();
        std::fs::create_dir_all(src.path().join("other")).unwrap();
        std::fs::write(src.path().join("other/g"), "cached").unwrap();
        let paths = vec!["deps/lib".to_owned(), "other".to_owned()];
        let archive = out.path().join("x");
        let written = save(src.path(), src.path(), &paths, &archive).unwrap();
        // `deps` points outside the workdir; `other` is missing, so a
        // restore runs.
        std::fs::create_dir_all(outside.path().join("lib")).unwrap();
        std::fs::write(outside.path().join("lib/keep"), "external").unwrap();
        std::os::unix::fs::symlink(outside.path(), dest.path().join("deps")).unwrap();
        let error = extract(
            &archive,
            written.bytes,
            &written.sha256,
            &declared(dest.path(), dest.path(), &paths),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("is a symlink"), "{error:#}");
        let names: Vec<_> = std::fs::read_dir(outside.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .collect();
        assert_eq!(names, ["lib"]);
        assert_eq!(
            std::fs::read_to_string(outside.path().join("lib/keep")).unwrap(),
            "external"
        );
        assert!(!outside.path().join("lib/f").exists());
        assert!(!dest.path().join("other").exists());
    }

    /// A directory on another file system than the temporary directories,
    /// when this machine has one.
    fn other_device() -> Option<tempfile::TempDir> {
        use std::os::unix::fs::MetadataExt;
        let here = std::fs::metadata(std::env::temp_dir()).ok()?.dev();
        ["/dev/shm", "/run/shm"].iter().find_map(|dir| {
            let meta = std::fs::metadata(dir).ok()?;
            (meta.dev() != here).then(|| tempfile::tempdir_in(dir).ok())?
        })
    }

    #[test]
    fn a_home_path_restores_through_a_symlink_to_another_disk() {
        use std::os::unix::fs::MetadataExt;
        let other = other_device();
        let same = tempfile::tempdir().unwrap();
        let mut disks = vec![("same device", same.path().to_path_buf())];
        match &other {
            Some(dir) => disks.push(("other device", dir.path().to_path_buf())),
            None => eprintln!("no second file system here; cross-device case not run"),
        }
        for (label, disk) in disks {
            let (src, out, work, home) = (
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
                tempfile::tempdir().unwrap(),
            );
            let registry = src.path().join(".cargo/registry");
            std::fs::create_dir_all(registry.join("bin")).unwrap();
            std::fs::write(registry.join("bin/tool"), "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(
                registry.join("bin/tool"),
                std::fs::Permissions::from_mode(0o755),
            )
            .unwrap();
            std::os::unix::fs::symlink("bin/tool", registry.join("link")).unwrap();
            let mtime = std::fs::metadata(registry.join("bin/tool"))
                .unwrap()
                .mtime();
            let paths = vec!["~/.cargo/registry".to_owned()];
            let archive = out.path().join("x");
            let written = save(work.path(), src.path(), &paths, &archive).unwrap();
            // `~/.cargo` lives on `disk`, with a stale registry in it.
            let cargo = disk.join("cargo");
            std::fs::create_dir_all(cargo.join("registry")).unwrap();
            std::fs::write(cargo.join("registry/stale"), "old").unwrap();
            std::os::unix::fs::symlink(&cargo, home.path().join(".cargo")).unwrap();
            extract(
                &archive,
                written.bytes,
                &written.sha256,
                &declared(work.path(), home.path(), &paths),
                &AtomicBool::new(false),
            )
            .unwrap_or_else(|e| panic!("{label}: {e:#}"));
            let restored = cargo.join("registry");
            assert_eq!(
                std::fs::read_to_string(restored.join("bin/tool")).unwrap(),
                "#!/bin/sh\n",
                "{label}"
            );
            let tool = std::fs::metadata(restored.join("bin/tool")).unwrap();
            assert_eq!(tool.permissions().mode() & 0o777, 0o755, "{label}");
            assert_eq!(tool.mtime(), mtime, "{label}");
            assert_eq!(
                std::fs::read_link(restored.join("link")).unwrap(),
                Path::new("bin/tool"),
                "{label}"
            );
            assert!(!restored.join("stale").exists(), "{label}");
            // Nothing left beside the registry or in the home directory.
            let names = |dir: &Path| -> Vec<String> {
                std::fs::read_dir(dir)
                    .unwrap()
                    .flatten()
                    .map(|e| e.file_name().into_string().unwrap())
                    .collect()
            };
            assert_eq!(names(&cargo), ["registry"], "{label}");
            assert_eq!(names(home.path()), [".cargo"], "{label}");
        }
    }

    #[test]
    fn a_failed_swap_puts_the_live_paths_back() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // root ignores the read-only directory below
        }
        let (src, out, dest) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        for root in [src.path(), dest.path()] {
            std::fs::create_dir_all(root.join("a")).unwrap();
            std::fs::create_dir_all(root.join("sub/b")).unwrap();
        }
        std::fs::write(src.path().join("a/f"), "new").unwrap();
        std::fs::write(src.path().join("sub/b/f"), "new").unwrap();
        std::fs::write(dest.path().join("a/f"), "mine").unwrap();
        std::fs::write(dest.path().join("sub/b/f"), "mine").unwrap();
        let paths = vec!["a".to_owned(), "sub/b".to_owned()];
        let archive = out.path().join("x");
        let written = save(src.path(), src.path(), &paths, &archive).unwrap();
        // `a` is swapped first; `sub/b` cannot be moved.
        let sub = dest.path().join("sub");
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = extract(
            &archive,
            written.bytes,
            &written.sha256,
            &declared(dest.path(), dest.path(), &paths),
            &AtomicBool::new(false),
        );
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(result.is_err());
        for file in ["a/f", "sub/b/f"] {
            assert_eq!(
                std::fs::read_to_string(dest.path().join(file)).unwrap(),
                "mine"
            );
        }
        let names: Vec<_> = std::fs::read_dir(dest.path())
            .unwrap()
            .flatten()
            .map(|e| e.file_name().into_string().unwrap())
            .filter(|n| n != ".goodflows")
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
    }

    #[test]
    fn part_md5s_match_the_slices_on_disk() {
        use base64::Engine;
        let (src, home, out) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        // Incompressible bytes, so the archive spans many write calls.
        let mut state: u64 = 1;
        let noise: Vec<u8> = (0..200_000)
            .map(|_| {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                (state >> 56) as u8
            })
            .collect();
        std::fs::create_dir_all(src.path().join("deps")).unwrap();
        std::fs::write(src.path().join("deps/noise"), &noise).unwrap();
        let paths = vec!["deps".to_owned()];
        let archive = |name: &str, parts| {
            let path = out.path().join(name);
            let written = write(
                &path,
                &declared(src.path(), home.path(), &paths),
                &"k".repeat(64),
                &"p".repeat(64),
                MAX_ENTRY_BYTES,
                parts,
                &abort(),
            )
            .unwrap();
            (written, std::fs::read(&path).unwrap())
        };
        let (plain, _) = archive("none", None);
        assert!(plain.part_md5.is_none());
        let n = plain.bytes;
        assert!(n > 100_000, "{n}");
        // Exact division (1 and n), a short last part (4096, n - 1) and a
        // part larger than the archive (n + 1).
        for size in [1, n, 4096, n - 1, n + 1] {
            let (written, bytes) = archive(&format!("p{size}"), Some(size));
            assert_eq!(written.bytes, n);
            assert_eq!(written.sha256, plain.sha256);
            let (part, md5s) = written.part_md5.unwrap();
            assert_eq!(part, size);
            let expected: Vec<String> = bytes
                .chunks(size as usize)
                .map(|slice| {
                    base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(slice))
                })
                .collect();
            assert_eq!(md5s.len() as u64, n.div_ceil(size), "part size {size}");
            assert_eq!(md5s, expected, "part size {size}");
        }
    }

    #[test]
    fn extractor_checks_the_sha256_first() {
        let (_dir, archive, size, _) =
            malicious(&[("workdir/deps/ok", tar::EntryType::Regular, "x")]);
        let work = tempfile::tempdir().unwrap();
        let paths = vec!["deps".to_owned()];
        let error = extract(
            &archive,
            size,
            &"0".repeat(64),
            &declared(work.path(), work.path(), &paths),
            &AtomicBool::new(false),
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("sha256"), "{error:#}");
        assert!(!work.path().join("deps").exists());
    }

    #[test]
    fn restore_replaces_declared_paths_only() {
        let (src, out, dest) = (
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
            tempfile::tempdir().unwrap(),
        );
        std::fs::create_dir_all(src.path().join("deps")).unwrap();
        std::fs::write(src.path().join("deps/new"), "new").unwrap();
        let paths = vec!["deps".to_owned(), "_build".to_owned()];
        let archive = out.path().join("a");
        let written = save(src.path(), src.path(), &paths, &archive).unwrap();
        std::fs::create_dir_all(dest.path().join("deps")).unwrap();
        std::fs::write(dest.path().join("deps/stale"), "old").unwrap();
        std::fs::write(dest.path().join("keep"), "mine").unwrap();
        extract(
            &archive,
            written.bytes,
            &written.sha256,
            &declared(dest.path(), dest.path(), &paths),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert!(dest.path().join("deps/new").exists());
        assert!(!dest.path().join("deps/stale").exists());
        assert!(dest.path().join("keep").exists());
    }
}
