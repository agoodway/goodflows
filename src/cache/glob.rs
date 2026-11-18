//! The small glob dialect of cache `paths` and `key.files`: `/`-separated
//! segments where `*` and `?` match within one segment, `[...]` matches one
//! character from a class (`[!...]` negates), and a `**` segment matches
//! zero or more directories. `**` never descends into `.git`, `.goodflows`
//! or symlinked directories.

use std::path::{Path, PathBuf};

/// Directories `**` never descends into.
const SKIPPED: [&str; 2] = [".git", ".goodflows"];

pub fn has_glob(text: &str) -> bool {
    text.contains(['*', '?', '['])
}

/// The nonempty, non-`.` segments of a validated relative pattern.
pub fn segments(pattern: &str) -> Vec<&str> {
    pattern
        .split('/')
        .filter(|s| !s.is_empty() && *s != ".")
        .collect()
}

/// Whether one name matches one glob segment.
pub fn segment_match(pattern: &str, name: &str) -> bool {
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    // Iterative matching with one backtrack point for the last `*`.
    let (mut pi, mut ni) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ni < n.len() {
        if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ni));
                    pi += 1;
                    continue;
                }
                '?' => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                '[' => {
                    if let Some((matched, next)) = class(&p, pi, n[ni]) {
                        if matched {
                            pi = next;
                            ni += 1;
                            continue;
                        }
                    } else if n[ni] == '[' {
                        pi += 1;
                        ni += 1;
                        continue;
                    }
                }
                c if c == n[ni] => {
                    pi += 1;
                    ni += 1;
                    continue;
                }
                _ => {}
            }
        }
        match star {
            Some((sp, sn)) => {
                pi = sp + 1;
                ni = sn + 1;
                star = Some((sp, sn + 1));
            }
            None => return false,
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}

/// Match `c` against the class starting at `p[start] == '['`: whether it
/// matched and the index after the class, or `None` for an unclosed class.
fn class(p: &[char], start: usize, c: char) -> Option<(bool, usize)> {
    let mut i = start + 1;
    let negate = matches!(p.get(i), Some('!' | '^'));
    if negate {
        i += 1;
    }
    let mut matched = false;
    let mut first = true;
    while i < p.len() {
        if p[i] == ']' && !first {
            return Some((matched != negate, i + 1));
        }
        first = false;
        if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' {
            matched |= p[i] <= c && c <= p[i + 2];
            i += 3;
        } else {
            matched |= p[i] == c;
            i += 1;
        }
    }
    None
}

/// Whether the whole of `path` matches the whole of `pattern`.
pub fn full_match(pattern: &[&str], path: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|skip| full_match(rest, &path[skip..])),
        Some((first, rest)) => path
            .split_first()
            .is_some_and(|(name, tail)| segment_match(first, name) && full_match(rest, tail)),
    }
}

/// Whether `path` is a match of `pattern` or lies below one.
pub fn within(pattern: &[&str], path: &[&str]) -> bool {
    (1..=path.len()).any(|len| full_match(pattern, &path[..len]))
}

/// Every existing path below `base` that matches `pattern`, relative to
/// `base`, sorted and without duplicates. Unreadable directories are skipped.
pub fn expand(base: &Path, pattern: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    walk(base, PathBuf::new(), &segments(pattern), &mut out);
    out.sort();
    out.dedup();
    out
}

fn children(dir: &Path) -> Vec<(String, bool)> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<(String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let real_dir = entry.file_type().is_ok_and(|t| t.is_dir());
            Some((name, real_dir))
        })
        .collect();
    names.sort();
    names
}

fn walk(base: &Path, rel: PathBuf, segs: &[&str], out: &mut Vec<PathBuf>) {
    let Some((&seg, rest)) = segs.split_first() else {
        if !rel.as_os_str().is_empty() {
            out.push(rel);
        }
        return;
    };
    if seg == "**" {
        walk(base, rel.clone(), rest, out);
        for (name, real_dir) in children(&base.join(&rel)) {
            if real_dir && !SKIPPED.contains(&name.as_str()) {
                walk(base, rel.join(name), segs, out);
            }
        }
    } else if !has_glob(seg) {
        let next = rel.join(seg);
        if std::fs::symlink_metadata(base.join(&next)).is_ok() {
            walk(base, next, rest, out);
        }
    } else {
        for (name, _) in children(&base.join(&rel)) {
            if segment_match(seg, &name) {
                walk(base, rel.join(name), rest, out);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn segments_match_like_shell_globs() {
        for (pattern, name, expected) in [
            ("*.lock", "mix.lock", true),
            ("*.lock", "mix.lockx", false),
            ("m?x.lock", "mix.lock", true),
            ("[mn]ix", "nix", true),
            ("[!m]ix", "mix", false),
            ("[a-c]x", "bx", true),
            ("*", ".hidden", true),
            ("a*b*c", "aXbYc", true),
            ("a*b*c", "aXbY", false),
            ("plain", "plain", true),
        ] {
            assert_eq!(segment_match(pattern, name), expected, "{pattern} {name}");
        }
    }

    #[test]
    fn double_star_matches_any_depth() {
        let p = segments("**/mix.lock");
        assert!(full_match(&p, &["mix.lock"]));
        assert!(full_match(&p, &["apps", "a", "mix.lock"]));
        assert!(!full_match(&p, &["apps", "mix.lockx"]));
        assert!(within(
            &segments("_build/*/lib"),
            &["_build", "dev", "lib", "x.beam"]
        ));
        assert!(!within(&segments("_build/*/lib"), &["_build", "dev"]));
    }

    #[test]
    fn expand_finds_matches_and_skips_git() {
        let dir = tempfile::tempdir().unwrap();
        for path in ["a/mix.lock", "b/c/mix.lock", ".git/mix.lock", "mix.lock"] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, "x").unwrap();
        }
        let found = expand(dir.path(), "**/mix.lock");
        let found: Vec<_> = found.iter().map(|p| p.to_str().unwrap()).collect();
        assert_eq!(found, ["a/mix.lock", "b/c/mix.lock", "mix.lock"]);
        assert_eq!(expand(dir.path(), "missing"), Vec::<PathBuf>::new());
        assert_eq!(expand(dir.path(), "a"), [PathBuf::from("a")]);
    }
}
