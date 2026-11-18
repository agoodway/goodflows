//! The local cache store: a private directory of `<key>.tgz` archives, each
//! with a `<key>.meta` sidecar. Saves are atomic and the first writer of a
//! key wins. Pruning removes the least recently used entries, in the manner
//! of `src/retention.rs`: it never removes an entry a reader holds locked.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Temporary files and archives without a sidecar older than this are left
/// by a crash and removed when pruning.
const STALE: Duration = Duration::from_secs(3600);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Meta {
    pub name: String,
    pub partial: String,
    pub sha256: String,
    pub size: u64,
    pub created_at_ms: u64,
    pub accessed_at_ms: u64,
}

pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or_default()
}

pub struct Store {
    dir: PathBuf,
}

/// An entry held open for reading: pruning skips it until this drops.
pub struct Reading {
    _file: std::fs::File,
    pub path: PathBuf,
    pub meta: Meta,
}

fn flock(file: &std::fs::File, operation: libc::c_int) -> std::io::Result<bool> {
    loop {
        // SAFETY: flock takes a descriptor this file owns and an integer operation.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) if operation & libc::LOCK_NB != 0 => return Ok(false),
            _ => return Err(error),
        }
    }
}

impl Store {
    /// Open the store at `dir`, creating it private (0700).
    pub fn open(dir: &Path) -> Result<Self> {
        crate::fsutil::dir_builder(true)
            .create(dir)
            .with_context(|| format!("cannot create the cache store {}", dir.display()))?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
        crate::fsutil::real_dir(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn archive(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.tgz"))
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.meta"))
    }

    /// The entry's metadata, when both files exist and the sidecar parses.
    pub fn get(&self, key: &str) -> Option<Meta> {
        if !super::key::valid_digest(key) || !self.archive(key).is_file() {
            return None;
        }
        serde_json::from_slice(&std::fs::read(self.meta_path(key)).ok()?).ok()
    }

    /// Every complete entry as `(key, meta)`.
    fn entries(&self) -> Vec<(String, Meta)> {
        let Ok(dir) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        dir.flatten()
            .filter_map(|entry| {
                let name = entry.file_name().into_string().ok()?;
                let key = name.strip_suffix(".meta")?.to_owned();
                let meta = self.get(&key)?;
                Some((key, meta))
            })
            .collect()
    }

    /// The newest entry with this partial digest.
    pub fn newest(&self, partial: &str) -> Option<(String, Meta)> {
        self.entries()
            .into_iter()
            .filter(|(_, meta)| meta.partial == partial)
            .max_by(|(ka, a), (kb, b)| (a.created_at_ms, ka).cmp(&(b.created_at_ms, kb)))
    }

    /// Open an entry for reading and record the access. `None` when it is
    /// missing.
    pub fn read(&self, key: &str) -> Option<Reading> {
        let mut meta = self.get(key)?;
        let path = self.archive(key);
        let file = std::fs::File::open(&path).ok()?;
        flock(&file, libc::LOCK_SH).ok()?;
        meta.accessed_at_ms = now_ms();
        if let Ok(bytes) = serde_json::to_vec(&meta) {
            let _ = crate::fsutil::write_atomic(&self.meta_path(key), &bytes, true);
        }
        Some(Reading {
            _file: file,
            path,
            meta,
        })
    }

    /// A new temporary path in the store, for an archive to [`put`].
    pub fn temp_path(&self) -> PathBuf {
        self.dir.join(format!(
            "{}{}",
            crate::fsutil::TEMP_PREFIX,
            uuid::Uuid::new_v4().simple()
        ))
    }

    /// Publish the archive at `temp` under `key`. The first writer wins:
    /// `Ok(false)` means another writer already stored the key, which
    /// counts as saved. The caller removes `temp`.
    pub fn put(&self, key: &str, temp: &Path, meta: &Meta) -> Result<bool> {
        match std::fs::hard_link(temp, self.archive(key)) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return Ok(false),
            Err(error) => return Err(error).context("cannot store the cache entry"),
        }
        crate::fsutil::write_atomic(&self.meta_path(key), &serde_json::to_vec(meta)?, true)?;
        Ok(true)
    }

    /// Remove an entry, for example one that failed verification.
    pub fn remove(&self, key: &str) {
        let _ = std::fs::remove_file(self.meta_path(key));
        let _ = std::fs::remove_file(self.archive(key));
    }

    /// Delete the least recently used entries until the store holds at most
    /// `max_bytes`, keeping `keep` and every entry a reader holds. Also
    /// removes stale temporary files and archives without a sidecar.
    pub fn prune(&self, max_bytes: u64, keep: &str) -> Result<()> {
        let now = SystemTime::now();
        let stale = |path: &Path| {
            std::fs::symlink_metadata(path)
                .and_then(|m| m.modified())
                .is_ok_and(|t| now.duration_since(t).is_ok_and(|age| age > STALE))
        };
        for entry in std::fs::read_dir(&self.dir)?.flatten() {
            let Ok(name) = entry.file_name().into_string() else {
                continue;
            };
            let path = entry.path();
            let orphan = name
                .strip_suffix(".tgz")
                .is_some_and(|key| !self.meta_path(key).exists());
            if (name.starts_with(crate::fsutil::TEMP_PREFIX) || orphan) && stale(&path) {
                let _ = std::fs::remove_file(path);
            }
        }
        let mut entries = self.entries();
        let mut total: u64 = entries.iter().map(|(_, m)| m.size).sum();
        entries.sort_by(|(ka, a), (kb, b)| (a.accessed_at_ms, ka).cmp(&(b.accessed_at_ms, kb)));
        for (key, meta) in entries {
            if total <= max_bytes {
                break;
            }
            if key == keep {
                continue;
            }
            let Ok(file) = std::fs::File::open(self.archive(&key)) else {
                continue;
            };
            // A reader holds a shared lock: leave the entry for a later prune.
            if !flock(&file, libc::LOCK_EX | libc::LOCK_NB)? {
                continue;
            }
            self.remove(&key);
            drop(file);
            total = total.saturating_sub(meta.size);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn meta(partial: &str, size: u64, created: u64) -> Meta {
        Meta {
            name: "deps".into(),
            partial: partial.into(),
            sha256: "0".repeat(64),
            size,
            created_at_ms: created,
            accessed_at_ms: created,
        }
    }

    fn put(store: &Store, key: &str, meta: &Meta) -> bool {
        let temp = store.temp_path();
        std::fs::write(&temp, vec![0u8; meta.size as usize]).unwrap();
        let won = store.put(key, &temp, meta).unwrap();
        std::fs::remove_file(temp).unwrap();
        won
    }

    fn key(n: u8) -> String {
        format!("{n:x}").repeat(64)[..64].to_owned()
    }

    #[test]
    fn the_store_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(&dir.path().join("a/b")).unwrap();
        let mode = std::fs::metadata(store.dir()).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700);
    }

    #[test]
    fn concurrent_saves_of_one_key_leave_one_entry() {
        let dir = tempfile::tempdir().unwrap();
        let store = std::sync::Arc::new(Store::open(dir.path()).unwrap());
        let winners: Vec<bool> = (0..8)
            .map(|i| {
                let store = store.clone();
                std::thread::spawn(move || put(&store, &key(1), &meta("p", 10 + i, i)))
            })
            .collect::<Vec<_>>()
            .into_iter()
            .map(|h| h.join().unwrap())
            .collect();
        assert_eq!(winners.iter().filter(|w| **w).count(), 1);
        let files: Vec<_> = std::fs::read_dir(dir.path()).unwrap().flatten().collect();
        assert_eq!(files.len(), 2, "one archive and one sidecar");
    }

    #[test]
    fn newest_entry_wins_for_a_partial() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        put(&store, &key(1), &meta("p", 1, 10));
        put(&store, &key(2), &meta("p", 1, 30));
        put(&store, &key(3), &meta("q", 1, 50));
        assert_eq!(store.newest("p").unwrap().0, key(2));
        assert!(store.newest("r").is_none());
    }

    #[test]
    fn pruning_removes_least_recently_used_first_and_skips_readers() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        put(&store, &key(1), &meta("p", 100, 1));
        put(&store, &key(2), &meta("p", 100, 2));
        put(&store, &key(3), &meta("p", 100, 3));
        // Reading entry 1 makes it the most recently used.
        drop(store.read(&key(1)).unwrap());
        store.prune(250, &key(3)).unwrap();
        assert!(
            store.get(&key(2)).is_none(),
            "least recently used goes first"
        );
        assert!(store.get(&key(1)).is_some() && store.get(&key(3)).is_some());
        // An entry being read is never deleted.
        let reading = store.read(&key(1)).unwrap();
        store.prune(0, &key(3)).unwrap();
        assert!(store.get(&key(1)).is_some());
        drop(reading);
        store.prune(0, &key(3)).unwrap();
        assert!(store.get(&key(1)).is_none());
        assert!(store.get(&key(3)).is_some(), "the entry just saved stays");
    }

    #[test]
    fn a_corrupt_sidecar_reads_as_a_miss() {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path()).unwrap();
        put(&store, &key(1), &meta("p", 1, 1));
        std::fs::write(dir.path().join(format!("{}.meta", key(1))), "{not json").unwrap();
        assert!(store.get(&key(1)).is_none());
        assert!(store.read(&key(1)).is_none());
        assert!(store.newest("p").is_none());
    }
}
