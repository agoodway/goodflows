//! Filesystem helpers shared by reporting, log retention and run history.

use anyhow::{Context, Result};
use std::ffi::{CString, OsStr, OsString};
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::{Duration, SystemTime};

/// A lowercase, hyphenated UUID: the server's only accepted `workflow_run_id`
/// spelling, and the run id inside every run stem.
pub fn canonical_uuid(id: &str) -> bool {
    uuid::Uuid::parse_str(id).is_ok_and(|uuid| uuid.hyphenated().to_string() == id)
}

/// Write a file by atomic replacement so readers never observe partial content.
pub fn write_atomic(path: &Path, bytes: &[u8], replace: bool) -> Result<()> {
    let parent = path.parent().context("path has no parent")?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    if replace {
        file.persist(path)?;
    } else {
        file.persist_noclobber(path)?;
    }
    Ok(())
}

/// A builder for directories private to the invoking user (mode 0700).
pub fn dir_builder(recursive: bool) -> std::fs::DirBuilder {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(recursive);
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder
}

/// Confirm a path is a real directory, never a symlink to one.
pub fn real_dir(path: &Path) -> std::io::Result<()> {
    if std::fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(std::io::Error::other("not a real directory"))
    }
}

/// Temporary files that `Dir::write_new` creates start with this prefix.
pub const TEMP_PREFIX: &str = ".tmp";

fn c_name(name: &OsStr) -> std::io::Result<CString> {
    CString::new(name.as_bytes()).map_err(|_| std::io::Error::other("name contains a NUL byte"))
}

/// Retry a libc call returning -1 on error while it fails with `EINTR`.
fn retry(mut call: impl FnMut() -> libc::c_int) -> std::io::Result<libc::c_int> {
    loop {
        let result = call();
        if result != -1 {
            return Ok(result);
        }
        let error = std::io::Error::last_os_error();
        if error.kind() != std::io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// `O_DIRECTORY | O_NOFOLLOW` fails on a symlink with `ELOOP` or, on some
/// systems, `ENOTDIR`; report every symlink as `ELOOP`.
fn symlink_error(error: std::io::Error, is_symlink: impl Fn() -> bool) -> std::io::Error {
    match error.raw_os_error() {
        Some(libc::ELOOP | libc::ENOTDIR) if is_symlink() => {
            std::io::Error::from_raw_os_error(libc::ELOOP)
        }
        _ => error,
    }
}

/// What a directory entry is, without following a symlink.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Dir,
    Symlink,
    Other,
}

/// One entry of an open directory, from `fstatat(AT_SYMLINK_NOFOLLOW)`.
#[derive(Debug)]
pub struct Entry {
    pub name: OsString,
    pub kind: Kind,
    pub len: u64,
    pub modified: SystemTime,
}

/// An open directory. Entries are created, listed and removed relative to its
/// descriptor, so they stay inside this directory even if its path is later
/// swapped for a symlink.
pub struct Dir {
    fd: OwnedFd,
}

impl Dir {
    /// Open a real directory. A symlink at `path` itself is refused with
    /// `ELOOP`.
    pub fn open(path: &Path) -> std::io::Result<Self> {
        let c = c_name(path.as_os_str())?;
        // SAFETY: a NUL-terminated path and integer flags; the result is checked.
        let fd = retry(|| unsafe {
            libc::open(
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })
        .map_err(|error| {
            symlink_error(error, || {
                std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
            })
        })?;
        // SAFETY: `fd` was just opened and is owned by nothing else.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Create the private directory `name` inside this one, or open it if it
    /// already exists. A symlink there is refused with `ELOOP`.
    pub fn create_dir(&self, name: &OsStr) -> std::io::Result<Self> {
        let c = c_name(name)?;
        // SAFETY: a valid directory descriptor, a NUL-terminated name and a mode.
        match retry(|| unsafe { libc::mkdirat(self.fd.as_raw_fd(), c.as_ptr(), 0o700) }) {
            Err(error) if error.kind() != std::io::ErrorKind::AlreadyExists => return Err(error),
            _ => {}
        }
        // SAFETY: as above, with integer flags; the result is checked.
        let fd = retry(|| unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        })
        .map_err(|error| {
            symlink_error(error, || {
                self.stat(name)
                    .is_ok_and(|entry| entry.kind == Kind::Symlink)
            })
        })?;
        // SAFETY: `fd` was just opened and is owned by nothing else.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Write `bytes` to the new file `name` (mode 0600) so readers never see
    /// partial content, failing rather than replacing an existing entry. The
    /// bytes go to a `.tmp<random>` file first, which is then hard-linked to
    /// `name` and unlinked.
    pub fn write_new(&self, name: &OsStr, bytes: &[u8]) -> std::io::Result<()> {
        let temp = format!("{TEMP_PREFIX}{}", uuid::Uuid::new_v4().simple());
        let temp_c = c_name(OsStr::new(&temp))?;
        // SAFETY: a valid directory descriptor, a NUL-terminated name, integer
        // flags and a mode; the result is checked.
        let fd = retry(|| unsafe {
            libc::openat(
                self.fd.as_raw_fd(),
                temp_c.as_ptr(),
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL | libc::O_NOFOLLOW | libc::O_CLOEXEC,
                0o600 as libc::c_uint,
            )
        })?;
        // SAFETY: `fd` was just opened and is owned by nothing else.
        let mut file = std::fs::File::from(unsafe { OwnedFd::from_raw_fd(fd) });
        let written = (|| {
            file.write_all(bytes)?;
            file.sync_all()?;
            let target = c_name(name)?;
            // SAFETY: valid descriptors and NUL-terminated names. `linkat`
            // fails with EEXIST rather than replacing `name`.
            retry(|| unsafe {
                libc::linkat(
                    self.fd.as_raw_fd(),
                    temp_c.as_ptr(),
                    self.fd.as_raw_fd(),
                    target.as_ptr(),
                    0,
                )
            })
            .map(drop)
        })();
        let _ = self.remove_file(OsStr::new(&temp));
        written
    }

    /// Unlink the file `name`; never follows a symlink.
    pub fn remove_file(&self, name: &OsStr) -> std::io::Result<()> {
        let c = c_name(name)?;
        // SAFETY: a valid directory descriptor and a NUL-terminated name.
        retry(|| unsafe { libc::unlinkat(self.fd.as_raw_fd(), c.as_ptr(), 0) }).map(drop)
    }

    /// Every entry except `.` and `..`. An entry removed while listing is
    /// left out.
    pub fn entries(&self) -> std::io::Result<Vec<Entry>> {
        // SAFETY: duplicates a valid descriptor; the result is checked.
        let fd = retry(|| unsafe { libc::fcntl(self.fd.as_raw_fd(), libc::F_DUPFD_CLOEXEC, 0) })?;
        // SAFETY: `fd` is a fresh duplicate; on success the stream owns it.
        let stream = unsafe { libc::fdopendir(fd) };
        if stream.is_null() {
            let error = std::io::Error::last_os_error();
            // SAFETY: `fd` is still ours when fdopendir fails.
            unsafe { libc::close(fd) };
            return Err(error);
        }
        let stream = DirStream(stream);
        // The duplicate shares the file offset; start from the beginning.
        // SAFETY: a valid directory stream.
        unsafe { libc::rewinddir(stream.0) };
        let mut names = Vec::new();
        loop {
            // SAFETY: a valid directory stream; the entry stays valid until
            // the next readdir on this stream, and its name is copied now.
            let entry = unsafe { libc::readdir(stream.0) };
            if entry.is_null() {
                break;
            }
            // SAFETY: readdir returned a valid entry with a NUL-terminated name.
            let name = unsafe { std::ffi::CStr::from_ptr((*entry).d_name.as_ptr()) };
            let name = name.to_bytes();
            if name != b"." && name != b".." {
                names.push(OsString::from_vec(name.to_vec()));
            }
        }
        drop(stream);
        let mut entries = Vec::with_capacity(names.len());
        for name in names {
            match self.stat(&name) {
                Ok(entry) => entries.push(entry),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(entries)
    }

    fn stat(&self, name: &OsStr) -> std::io::Result<Entry> {
        let c = c_name(name)?;
        // SAFETY: stat is plain old data; fstatat fills it on success.
        let mut stat: libc::stat = unsafe { std::mem::zeroed() };
        // SAFETY: a valid directory descriptor, a NUL-terminated name and a
        // valid out-pointer; never follows a symlink.
        retry(|| unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c.as_ptr(),
                &mut stat,
                libc::AT_SYMLINK_NOFOLLOW,
            )
        })?;
        let kind = match stat.st_mode & libc::S_IFMT {
            libc::S_IFREG => Kind::File,
            libc::S_IFDIR => Kind::Dir,
            libc::S_IFLNK => Kind::Symlink,
            _ => Kind::Other,
        };
        let modified = u64::try_from(stat.st_mtime)
            .map(|secs| SystemTime::UNIX_EPOCH + Duration::from_secs(secs))
            .unwrap_or(SystemTime::UNIX_EPOCH);
        Ok(Entry {
            name: name.to_owned(),
            kind,
            len: stat.st_size as u64,
            modified,
        })
    }
}

/// Closes the directory stream (and its descriptor) on drop.
struct DirStream(*mut libc::DIR);

impl Drop for DirStream {
    fn drop(&mut self) {
        // SAFETY: the stream came from a successful fdopendir and is closed once.
        unsafe { libc::closedir(self.0) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_uuids_are_lowercase_hyphenated() {
        assert!(canonical_uuid("67f7946d-136b-4608-ab01-e1f251a09754"));
        for bad in [
            "67F7946D-136B-4608-AB01-E1F251A09754",
            "67f7946d136b4608ab01e1f251a09754",
            "{67f7946d-136b-4608-ab01-e1f251a09754}",
            "urn:uuid:67f7946d-136b-4608-ab01-e1f251a09754",
            "../67f7946d-136b-4608-ab01-e1f251a09754",
            "",
        ] {
            assert!(!canonical_uuid(bad), "{bad}");
        }
    }

    #[test]
    fn dir_writes_new_files_and_refuses_symlinks() {
        let root = tempfile::tempdir().unwrap();
        let dir = Dir::open(root.path()).unwrap();
        let sub = dir.create_dir(OsStr::new("sub")).unwrap();
        // Opening an existing directory again works.
        dir.create_dir(OsStr::new("sub")).unwrap();
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(root.path().join("sub"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o700);
        sub.write_new(OsStr::new("a.json"), b"one").unwrap();
        let path = root.path().join("sub/a.json");
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        // An existing name is never replaced, and no temporary file is left.
        assert!(sub.write_new(OsStr::new("a.json"), b"two").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"one");
        let entries = sub.entries().unwrap();
        assert_eq!(entries.len(), 1, "{entries:?}");
        assert_eq!(entries[0].name, "a.json");
        assert_eq!((entries[0].kind, entries[0].len), (Kind::File, 3));
        // Symlinks are listed as such and refused as directories.
        std::os::unix::fs::symlink(root.path().join("sub"), root.path().join("link")).unwrap();
        let error = dir.create_dir(OsStr::new("link")).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        let error = Dir::open(&root.path().join("link")).err().unwrap();
        assert_eq!(error.raw_os_error(), Some(libc::ELOOP));
        let link = dir
            .entries()
            .unwrap()
            .into_iter()
            .find(|e| e.name == "link")
            .unwrap();
        assert_eq!(link.kind, Kind::Symlink);
        sub.remove_file(OsStr::new("a.json")).unwrap();
        assert!(sub.entries().unwrap().is_empty());
        // Listing twice starts from the beginning each time.
        assert_eq!(dir.entries().unwrap().len(), 2);
        assert_eq!(dir.entries().unwrap().len(), 2);
    }
}
