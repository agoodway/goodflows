//! Cache keys. The partial digest names a cache's lineage (format, name,
//! platform and `key.values`); the key adds the hashed `key.files` and
//! `key.unique`, so a changed lockfile or unique value changes only the key.

use super::glob;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Bumped when keys or archives change incompatibly.
pub const FORMAT_VERSION: u32 = 1;

pub fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// sha256 of the canonical JSON array `[version, name, os, arch, values]`.
/// Values are scalars, whose JSON encoding is canonical.
pub fn partial(name: &str, os: &str, arch: &str, values: &[Value]) -> String {
    let document = json!([FORMAT_VERSION, name, os, arch, values]);
    hex(Sha256::digest(document.to_string()))
}

/// The digest of the files under `workdir` that match `patterns`: the
/// sorted relative paths and each file's content digest. No matching file
/// gives the digest of nothing, which is not an error.
pub fn files_digest(
    workdir: &Path,
    patterns: &[String],
    abort: &std::sync::atomic::AtomicBool,
) -> std::io::Result<String> {
    let mut files: Vec<_> = patterns
        .iter()
        .flat_map(|pattern| glob::expand(workdir, pattern))
        .filter(|rel| std::fs::metadata(workdir.join(rel)).is_ok_and(|m| m.is_file()))
        .collect();
    files.sort();
    files.dedup();
    let mut hasher = Sha256::new();
    for rel in files {
        if abort.load(std::sync::atomic::Ordering::Relaxed) {
            return Err(std::io::Error::other("abandoned"));
        }
        let bytes = rel.as_os_str().as_encoded_bytes();
        hasher.update((bytes.len() as u64).to_be_bytes());
        hasher.update(bytes);
        let mut file = std::fs::File::open(workdir.join(&rel))?;
        let mut content = Sha256::new();
        let mut buffer = vec![0; 1 << 16];
        loop {
            if abort.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(std::io::Error::other("abandoned"));
            }
            let read = std::io::Read::read(&mut file, &mut buffer)?;
            if read == 0 {
                break;
            }
            content.update(&buffer[..read]);
        }
        hasher.update(content.finalize());
    }
    Ok(hex(hasher.finalize()))
}

/// sha256 of the canonical JSON array `[partial, files digest, unique]`.
pub fn key(partial: &str, files_digest: &str, unique: &[Value]) -> String {
    let document = json!([partial, files_digest, unique]);
    hex(Sha256::digest(document.to_string()))
}

/// 64 lowercase hex digits: the shape of every key, partial and sha256.
pub fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(dir: &Path, values: &[Value], unique: &[Value]) -> (String, String) {
        let partial = partial("deps", "linux", "x86_64", values);
        let files = files_digest(dir, &["**/mix.lock".into()], &Default::default()).unwrap();
        let key = key(&partial, &files, unique);
        (partial, key)
    }

    #[test]
    fn a_changed_lockfile_or_unique_value_changes_only_the_key() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("mix.lock"), "a").unwrap();
        let (p1, k1) = keys(dir.path(), &[json!("26")], &[]);
        std::fs::write(dir.path().join("mix.lock"), "b").unwrap();
        let (p2, k2) = keys(dir.path(), &[json!("26")], &[]);
        assert_eq!(p1, p2);
        assert_ne!(k1, k2);
        let (p3, k3) = keys(dir.path(), &[json!("26")], &[json!("sha1")]);
        assert_eq!(p1, p3);
        assert_ne!(k2, k3);
        assert!(valid_digest(&p1) && valid_digest(&k1));
    }

    #[test]
    fn value_order_matters_and_values_change_the_partial() {
        let a = partial("deps", "linux", "x86_64", &[json!("a"), json!("b")]);
        let b = partial("deps", "linux", "x86_64", &[json!("b"), json!("a")]);
        assert_ne!(a, b);
        // A string and a number with the same text are different values.
        assert_ne!(
            partial("deps", "linux", "x86_64", &[json!("1")]),
            partial("deps", "linux", "x86_64", &[json!(1)])
        );
    }

    #[test]
    fn no_matching_files_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let empty = files_digest(dir.path(), &["**/mix.lock".into()], &Default::default()).unwrap();
        assert_eq!(empty, hex(Sha256::digest([])));
        assert_eq!(
            files_digest(dir.path(), &[], &Default::default()).unwrap(),
            empty
        );
    }

    #[test]
    fn a_different_os_or_arch_changes_the_partial() {
        let base = partial("deps", "linux", "x86_64", &[]);
        assert_ne!(base, partial("deps", "macos", "x86_64", &[]));
        assert_ne!(base, partial("deps", "linux", "aarch64", &[]));
        assert_ne!(base, partial("other", "linux", "x86_64", &[]));
    }

    #[test]
    fn file_paths_are_part_of_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        std::fs::write(dir.path().join("a/mix.lock"), "x").unwrap();
        let one = files_digest(dir.path(), &["**/mix.lock".into()], &Default::default()).unwrap();
        std::fs::rename(dir.path().join("a"), dir.path().join("b")).unwrap();
        let two = files_digest(dir.path(), &["**/mix.lock".into()], &Default::default()).unwrap();
        assert_ne!(one, two);
    }
}
