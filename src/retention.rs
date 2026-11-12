//! Run identity and retention for log run directories and run history files.
//! A leaf module: it knows names, locks and caps, never jobs or results.

use crate::fsutil::{self, Dir, Kind};
use anyhow::{bail, Context, Result};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const STEM_TIME_LEN: usize = "YYYYMMDDTHHMMSSZ".len();

/// A history directory temporary file older than this is a crash leftover.
const STALE_TEMP: Duration = Duration::from_secs(3600);

/// One invocation's identity: a lowercase v4 UUID and its UTC start second.
#[derive(Debug, Clone)]
pub struct RunIdentity {
    pub id: String,
    /// `YYYYMMDDTHHMMSSZ-<id>`: names the log run directory and the history file.
    pub stem: String,
    /// The stem's instant in RFC 3339 (`YYYY-MM-DDTHH:MM:SSZ`).
    pub started_at: String,
}

impl RunIdentity {
    pub fn new() -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        Self::at(secs, uuid::Uuid::new_v4().hyphenated().to_string())
    }

    /// A run identity with a preallocated id (a Sprite run's canonical UUID).
    pub fn with_id(id: String) -> Self {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        Self::at(secs, id)
    }

    fn at(secs: u64, id: String) -> Self {
        let (year, month, day) = civil_from_days((secs / 86_400) as i64);
        let rem = secs % 86_400;
        let (hour, minute, second) = (rem / 3600, rem % 3600 / 60, rem % 60);
        let stem = format!("{year:04}{month:02}{day:02}T{hour:02}{minute:02}{second:02}Z-{id}");
        let started_at =
            format!("{year:04}-{month:02}-{day:02}T{hour:02}:{minute:02}:{second:02}Z");
        Self {
            id,
            stem,
            started_at,
        }
    }
}

/// Days since 1970-01-01 to a proleptic Gregorian (year, month, day), after
/// Howard Hinnant's `civil_from_days`.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// Whether a file or directory name is a run stem. Prune touches only these.
pub fn is_stem(name: &str) -> bool {
    let bytes = name.as_bytes();
    bytes.len() == STEM_TIME_LEN + 1 + 36
        && bytes[..8].iter().all(u8::is_ascii_digit)
        && bytes[8] == b'T'
        && bytes[9..15].iter().all(u8::is_ascii_digit)
        && bytes[15] == b'Z'
        && bytes[16] == b'-'
        && fsutil::canonical_uuid(&name[STEM_TIME_LEN + 1..])
}

/// Whether an error means the entry is already gone, for example because a
/// concurrent run pruned it first.
fn gone(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::NotFound
}

/// Lock file inside each log run directory, held exclusively for the whole run.
pub const LOG_LOCK: &str = ".goodflows.lock";

/// This invocation's log run directory. Dropping it releases the lock.
pub struct LogRun {
    pub dir: PathBuf,
    _lock: std::fs::File,
}

impl LogRun {
    /// Create `<base>/<stem>` (mode 0700, as is any missing parent) and lock
    /// it. A failure here fails the run. The prune of older run directories
    /// that follows never does: its error is returned alongside the new run.
    ///
    /// The directory is built as `.<stem>.tmp`, which prune never matches, and
    /// renamed to `<stem>` only once its lock is held (an `flock` follows the
    /// open file across the rename), so a concurrent prune can never see the
    /// new directory unlocked. A crash between create and rename can leave a
    /// `.<stem>.tmp` directory behind; like any unrelated name, prune keeps it.
    pub fn create(base: &Path, stem: &str, keep: u64) -> Result<(Self, Option<anyhow::Error>)> {
        let dir = base.join(stem);
        fsutil::dir_builder(true)
            .create(base)
            .with_context(|| format!("failed to create log directory {}", base.display()))?;
        let staging = base.join(format!(".{stem}.tmp"));
        // Non-recursive: a run never shares a directory with another invocation.
        fsutil::dir_builder(false)
            .create(&staging)
            .with_context(|| format!("failed to create log directory {}", dir.display()))?;
        let lock = (|| {
            let lock = open_lock(&staging.join(LOG_LOCK), true)
                .and_then(|file| flock(&file, false).map(|_| file))
                .with_context(|| format!("failed to lock log directory {}", dir.display()))?;
            // The stem carries a fresh UUID, so an existing target is a bug or
            // tampering; refuse it rather than rename over an empty directory.
            if std::fs::symlink_metadata(&dir).is_ok() {
                bail!("log directory {} already exists", dir.display());
            }
            std::fs::rename(&staging, &dir)
                .with_context(|| format!("failed to create log directory {}", dir.display()))?;
            Ok(lock)
        })();
        let lock = match lock {
            Ok(lock) => lock,
            Err(error) => {
                let _ = std::fs::remove_dir_all(&staging);
                return Err(error);
            }
        };
        let pruned = prune_log_runs(base, stem, keep)
            .with_context(|| format!("failed to prune log runs in {}", base.display()))
            .err();
        Ok((Self { dir, _lock: lock }, pruned))
    }
}

fn open_lock(path: &Path, create: bool) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(true)
        .write(create)
        .create(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

/// Take an exclusive `flock`. With `nonblocking`, `Ok(false)` means another
/// process holds it.
fn flock(file: &std::fs::File, nonblocking: bool) -> std::io::Result<bool> {
    use std::os::fd::AsRawFd;
    let operation = libc::LOCK_EX | if nonblocking { libc::LOCK_NB } else { 0 };
    loop {
        // SAFETY: flock takes a descriptor this file owns and an integer operation.
        if unsafe { libc::flock(file.as_raw_fd(), operation) } == 0 {
            return Ok(true);
        }
        let error = std::io::Error::last_os_error();
        match error.raw_os_error() {
            Some(libc::EINTR) => continue,
            Some(libc::EWOULDBLOCK) if nonblocking => return Ok(false),
            _ => return Err(error),
        }
    }
}

/// Keep the newest `keep` run directories by name, counting the current one.
/// Never deletes the current directory, a locked (live) one, an unrelated
/// name or a symlink. An unfinished run that is no longer live counts. A run
/// still being created is named `.<stem>.tmp`, an unrelated name, until it
/// holds its lock. A directory whose lock file cannot be opened or locked for
/// any reason other than being absent is kept and reported. An entry that
/// disappears meanwhile (a concurrent run pruned it) counts as pruned.
fn prune_log_runs(base: &Path, current: &str, keep: u64) -> Result<()> {
    let mut older = Vec::new();
    for entry in std::fs::read_dir(base)? {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) if gone(&error) => continue,
            Err(error) => return Err(error.into()),
        };
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if name == current || !is_stem(&name) {
            continue;
        }
        // `file_type` does not follow symlinks.
        match entry.file_type() {
            Ok(kind) if kind.is_dir() => older.push(name),
            Ok(_) => {}
            Err(error) if gone(&error) => {}
            Err(error) => return Err(error.into()),
        }
    }
    older.sort();
    older.reverse();
    prune_runs(base, older, keep, LOG_LOCK)
}

/// Keep the newest `keep` run directories, counting the current one, which
/// `newest_first` leaves out: it names the other run directories under
/// `base`, newest first. Each run holds an exclusive `flock` on its
/// `lock_file` for its whole life. A locked (live) run is never removed; an
/// unlocked one, or one without that file, is. A directory whose lock file
/// cannot be opened or locked for any other reason is kept and reported.
/// An entry that disappears meanwhile (a concurrent run pruned it) counts as
/// pruned.
pub fn prune_runs(
    base: &Path,
    newest_first: Vec<String>,
    keep: u64,
    lock_file: &str,
) -> Result<()> {
    let mut kept = 1;
    let mut first_error: Option<anyhow::Error> = None;
    for name in newest_first {
        if kept < keep {
            kept += 1;
            continue;
        }
        let dir = base.join(&name);
        let lock = dir.join(lock_file);
        // Hold the lock while deleting so a live run is never removed. Only a
        // missing lock file means an old or crashed run (or one already gone).
        let guard = match open_lock(&lock, false) {
            Ok(file) => match flock(&file, true) {
                Ok(true) => Some(file),
                Ok(false) => continue,
                Err(error) => {
                    first_error.get_or_insert(
                        anyhow::Error::new(error)
                            .context(format!("skipped {}: failed to lock", dir.display())),
                    );
                    continue;
                }
            },
            Err(error) if gone(&error) => None,
            Err(error) => {
                first_error.get_or_insert(anyhow::Error::new(error).context(format!(
                    "skipped {}: failed to open {lock_file}",
                    dir.display()
                )));
                continue;
            }
        };
        match std::fs::remove_dir_all(&dir) {
            Err(error) if !gone(&error) => {
                first_error.get_or_insert(
                    anyhow::Error::new(error)
                        .context(format!("failed to remove {}", dir.display())),
                );
            }
            _ => {}
        }
        drop(guard);
    }
    match first_error {
        Some(error) => Err(error),
        None => Ok(()),
    }
}

/// Whether a name is a `Dir::write_new` temporary file.
fn is_temp(name: &str) -> bool {
    name.strip_prefix(fsutil::TEMP_PREFIX)
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Keep the newest `<stem>.json` files by name until both caps hold. The
/// current file always stays, even when it alone exceeds `max_bytes`.
/// Unrelated names and symlinks are never touched, except that a regular
/// `.tmp<random>` file older than an hour (left by a crash mid-write) is
/// removed. A file that disappears meanwhile (a concurrent run pruned it)
/// counts as pruned.
pub fn prune_history(dir: &Dir, current: &str, max_runs: u64, max_bytes: u64) -> Result<()> {
    let current_name = format!("{current}.json");
    let mut older = Vec::new();
    let mut stale = Vec::new();
    let mut kept_bytes = 0;
    let now = SystemTime::now();
    for entry in dir.entries()? {
        let Some(name) = entry.name.to_str().map(str::to_owned) else {
            continue;
        };
        if entry.kind != Kind::File {
            continue;
        }
        if is_temp(&name) {
            if now
                .duration_since(entry.modified)
                .is_ok_and(|age| age > STALE_TEMP)
            {
                stale.push(name);
            }
            continue;
        }
        if !name.strip_suffix(".json").is_some_and(is_stem) {
            continue;
        }
        if name == current_name {
            kept_bytes = entry.len;
        } else {
            older.push((name, entry.len));
        }
    }
    older.sort();
    let mut kept_runs = 1;
    let mut full = false;
    let mut victims = stale;
    for (name, size) in older.into_iter().rev() {
        full |= kept_runs + 1 > max_runs || kept_bytes + size > max_bytes;
        if full {
            victims.push(name);
        } else {
            kept_runs += 1;
            kept_bytes += size;
        }
    }
    let mut first_error = None;
    for name in victims {
        match dir.remove_file(OsStr::new(&name)) {
            Err(error) if !gone(&error) => {
                first_error.get_or_insert(error);
            }
            _ => {}
        }
    }
    match first_error {
        Some(error) => Err(error.into()),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "67f7946d-136b-4608-ab01-e1f251a09754";

    fn stem(second: u32) -> String {
        format!("20260101T0000{second:02}Z-{ID}")
    }

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn identity_formats_utc_seconds() {
        let run = RunIdentity::at(1_793_750_400, ID.into());
        assert_eq!(run.stem, format!("20261104T000000Z-{ID}"));
        assert_eq!(run.started_at, "2026-11-04T00:00:00Z");
        let leap = RunIdentity::at(951_868_799, ID.into());
        assert_eq!(leap.started_at, "2000-02-29T23:59:59Z");
        assert_eq!(
            RunIdentity::at(0, ID.into()).started_at,
            "1970-01-01T00:00:00Z"
        );
        assert!(is_stem(&run.stem));
        let fresh = RunIdentity::new();
        assert!(is_stem(&fresh.stem), "{}", fresh.stem);
        assert!(fresh.stem.ends_with(&fresh.id));
    }

    #[test]
    fn stems_are_recognized_strictly() {
        assert!(!is_stem("notes.txt"));
        assert!(!is_stem(&format!("20261104T000000Z-{}", ID.to_uppercase())));
        assert!(!is_stem(&format!("20261104X000000Z-{ID}")));
        assert!(!is_stem(&format!("20261104T000000Z_{ID}")));
        assert!(!is_stem(&format!("20261104T000000Z-{ID}x")));
    }

    #[test]
    fn log_run_is_locked_before_it_gets_its_stem_name() {
        let base = tempfile::tempdir().unwrap();
        let (run, pruned) = LogRun::create(base.path(), &stem(1), 10).unwrap();
        assert!(pruned.is_none());
        assert_eq!(run.dir, base.path().join(stem(1)));
        // The staging name is gone and the renamed directory's lock is held.
        assert_eq!(names(base.path()), [stem(1)]);
        use std::os::unix::fs::PermissionsExt;
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&run.dir), 0o700);
        assert_eq!(mode(&run.dir.join(LOG_LOCK)), 0o600);
        let other = open_lock(&run.dir.join(LOG_LOCK), false).unwrap();
        assert!(!flock(&other, true).unwrap());
        drop(run);
        // A process another test thread is spawning can briefly hold a copy
        // of the descriptor between fork and exec, so allow a short retry.
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        while !flock(&other, true).unwrap() {
            assert!(std::time::Instant::now() < deadline, "lock never released");
            std::thread::sleep(Duration::from_millis(10));
        }
        // An existing target is refused and the staging directory removed.
        assert!(LogRun::create(base.path(), &stem(1), 10).is_err());
        assert_eq!(names(base.path()), [stem(1)]);
    }

    #[test]
    fn prune_keeps_staging_dirs_and_unopenable_locks() {
        // SAFETY: geteuid has no preconditions.
        if unsafe { libc::geteuid() } == 0 {
            return; // Permissions do not stop root.
        }
        use std::os::unix::fs::PermissionsExt;
        let base = tempfile::tempdir().unwrap();
        // A crashed create leaves a staging directory, an unrelated name.
        let staging = base.path().join(format!(".{}.tmp", stem(0)));
        std::fs::create_dir(&staging).unwrap();
        // A lock that exists but cannot be opened is not treated as unlocked.
        let unreadable = base.path().join(stem(1));
        std::fs::create_dir(&unreadable).unwrap();
        let lock = unreadable.join(LOG_LOCK);
        std::fs::write(&lock, "").unwrap();
        std::fs::set_permissions(&lock, std::fs::Permissions::from_mode(0o000)).unwrap();
        // A run without a lock file is an old or crashed run.
        std::fs::create_dir(base.path().join(stem(2))).unwrap();
        let (_run, pruned) = LogRun::create(base.path(), &stem(3), 1).unwrap();
        let error = format!("{:#}", pruned.expect("unopenable lock is reported"));
        assert!(
            error.contains(&stem(1)) && error.contains(LOG_LOCK),
            "{error}"
        );
        assert_eq!(
            names(base.path()),
            [format!(".{}.tmp", stem(0)), stem(1), stem(3)]
        );
    }

    fn many_stems() -> Vec<String> {
        (0..60)
            .map(|n| format!("2026010{}T0000{:02}Z-{ID}", 1 + n / 60, n % 60))
            .collect()
    }

    #[test]
    fn concurrent_log_prunes_both_succeed() {
        let base = tempfile::tempdir().unwrap();
        for name in many_stems() {
            let dir = base.path().join(name);
            std::fs::create_dir_all(dir.join("sub")).unwrap();
            std::fs::write(dir.join("sub/001-a.stdout.log"), "x").unwrap();
        }
        // Two runs prune the same directories at once; whichever loses a
        // race finds entries already gone, which is not an error.
        let current = format!("20270101T000000Z-{ID}");
        std::thread::scope(|scope| {
            let prunes: Vec<_> = (0..2)
                .map(|_| scope.spawn(|| prune_log_runs(base.path(), &current, 1)))
                .collect();
            for prune in prunes {
                prune.join().unwrap().unwrap();
            }
        });
        assert!(names(base.path()).is_empty());
    }

    #[test]
    fn concurrent_history_prunes_both_succeed() {
        let root = tempfile::tempdir().unwrap();
        for name in many_stems() {
            std::fs::write(root.path().join(format!("{name}.json")), "1234").unwrap();
        }
        let current = format!("20270101T000000Z-{ID}");
        std::fs::write(root.path().join(format!("{current}.json")), "1234").unwrap();
        std::thread::scope(|scope| {
            let prunes: Vec<_> = (0..2)
                .map(|_| {
                    scope.spawn(|| {
                        let dir = Dir::open(root.path()).unwrap();
                        prune_history(&dir, &current, 1, 1000)
                    })
                })
                .collect();
            for prune in prunes {
                prune.join().unwrap().unwrap();
            }
        });
        assert_eq!(names(root.path()), [format!("{current}.json")]);
    }

    #[test]
    fn prune_history_keeps_newest_files_within_both_caps() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        let dir = Dir::open(path).unwrap();
        for second in 0..5 {
            std::fs::write(path.join(format!("{}.json", stem(second))), "1234").unwrap();
        }
        std::fs::write(path.join("notes.txt"), "keep").unwrap();
        std::os::unix::fs::symlink(
            "/nonexistent",
            path.join(format!("{}.json", stem(0).replace("00Z", "99Z"))),
        )
        .unwrap();
        prune_history(&dir, &stem(4), 3, 1000).unwrap();
        assert!(names(path).contains(&format!("{}.json", stem(2))));
        assert!(!names(path).contains(&format!("{}.json", stem(1))));
        prune_history(&dir, &stem(4), 1000, 5).unwrap();
        let left = names(path);
        assert!(left.contains(&format!("{}.json", stem(4))));
        assert!(!left.contains(&format!("{}.json", stem(3))));
        assert!(left.contains(&"notes.txt".to_owned()));
        assert!(
            left.iter().any(|name| name.contains("0099Z")),
            "symlink kept: {left:?}"
        );
        // The current file stays even when it alone exceeds the byte cap,
        // and even when it does not sort newest.
        std::fs::write(path.join(format!("{}.json", stem(9))), "1234").unwrap();
        prune_history(&dir, &stem(4), 1000, 1).unwrap();
        let left = names(path);
        assert!(left.contains(&format!("{}.json", stem(4))));
        assert!(!left.contains(&format!("{}.json", stem(9))));
    }

    #[test]
    fn prune_history_removes_only_stale_temporary_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path();
        let dir = Dir::open(path).unwrap();
        let old = SystemTime::now() - Duration::from_secs(7200);
        for name in [".tmpAbC123", ".tmp", ".tmp-x", ".tmpfresh1"] {
            std::fs::write(path.join(name), "partial").unwrap();
        }
        for name in [".tmpAbC123", ".tmp", ".tmp-x"] {
            std::fs::File::options()
                .write(true)
                .open(path.join(name))
                .unwrap()
                .set_modified(old)
                .unwrap();
        }
        prune_history(&dir, &stem(1), 10, 1000).unwrap();
        // A stale temp file goes; a fresh one (a write in progress) and
        // names outside the pattern stay.
        assert_eq!(names(path), [".tmp", ".tmp-x", ".tmpfresh1"]);
    }
}
