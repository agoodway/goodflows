//! Secret references in settings, their resolution through the 1Password CLI,
//! and the masker that replaces resolved values in job output.

use crate::config::Config;
use anyhow::{bail, Result};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// The longest accepted reference, in bytes.
const MAX_REFERENCE_BYTES: usize = 1024;
/// Values shorter than this are delivered but never masked: replacing them
/// would corrupt unrelated output.
pub const MIN_MASKED_BYTES: usize = 4;
pub const REDACTED: &str = "[redacted]";
/// The name under which a Sprite controller forwards resolved values (JSON
/// mapping references to values) in the private forwarded-value frame. The
/// worker hands them to the runner on stdin; hooks never see them.
pub const SPRITE_SECRETS_ENV: &str = "GOODFLOWS_SPRITE_SECRETS";
/// The first read waits for a possible desktop approval prompt.
const FIRST_READ_TIMEOUT: Duration = Duration::from_secs(120);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const PARALLEL_READS: usize = 8;
const MAX_STDERR_BYTES: usize = 512;

/// A parsed secret reference. The URI scheme selects the provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    /// The whole `op://` reference, passed to `op read` as written.
    OnePassword(String),
}

impl Reference {
    pub fn parse(text: &str) -> Result<Self> {
        let Some(path) = text.strip_prefix("op://") else {
            bail!("unsupported secret reference; only op:// references are supported");
        };
        if text.len() > MAX_REFERENCE_BYTES {
            bail!("secret reference must be at most {MAX_REFERENCE_BYTES} bytes");
        }
        if text.chars().any(|c| c.is_whitespace() || c.is_control()) {
            bail!("secret reference must not contain whitespace or control characters");
        }
        let path = path.split_once('?').map_or(path, |(path, _)| path);
        let segments: Vec<&str> = path.split('/').collect();
        if !(3..=4).contains(&segments.len()) || segments.iter().any(|s| s.is_empty()) {
            bail!("op:// reference must be op://vault/item/field or op://vault/item/section/field");
        }
        Ok(Self::OnePassword(text.to_owned()))
    }

    fn as_str(&self) -> &str {
        match self {
            Self::OnePassword(text) => text,
        }
    }
}

/// Replaces resolved values with [`REDACTED`]. Cheap to clone.
#[derive(Debug, Clone, Default)]
pub struct Masker {
    /// Values of at least [`MIN_MASKED_BYTES`], longest first, without duplicates.
    values: Arc<[Vec<u8>]>,
}

impl Masker {
    pub fn new<'a>(values: impl IntoIterator<Item = &'a str>) -> Self {
        let mut values: Vec<Vec<u8>> = values
            .into_iter()
            .filter(|v| v.len() >= MIN_MASKED_BYTES)
            .map(|v| v.as_bytes().to_vec())
            .collect();
        values.sort_by(|a, b| b.len().cmp(&a.len()).then_with(|| a.cmp(b)));
        values.dedup();
        Self {
            values: values.into(),
        }
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    /// State for one output stream.
    pub fn stream(&self) -> MaskStream {
        MaskStream {
            masker: self.clone(),
            pending: Vec::new(),
        }
    }

    pub fn mask_str(&self, text: &str) -> String {
        if self.is_empty() {
            return text.to_owned();
        }
        String::from_utf8_lossy(&self.scan(text.as_bytes(), true).0).into_owned()
    }

    /// Replace every complete value in `bytes`. Unless `last`, stop at the
    /// first position whose remainder is a proper prefix of a value and
    /// return that position, so the caller keeps those bytes for later.
    fn scan(&self, bytes: &[u8], last: bool) -> (Vec<u8>, usize) {
        let mut out = Vec::with_capacity(bytes.len());
        let mut i = 0;
        while i < bytes.len() {
            let rest = &bytes[i..];
            // Checked first: a longer value may still complete here, and it
            // must win over a shorter one that already matches.
            if !last
                && self
                    .values
                    .iter()
                    .any(|v| v.len() > rest.len() && v.starts_with(rest))
            {
                return (out, i);
            }
            if let Some(value) = self.values.iter().find(|v| rest.starts_with(v)) {
                out.extend_from_slice(REDACTED.as_bytes());
                i += value.len();
                continue;
            }
            out.push(bytes[i]);
            i += 1;
        }
        (out, i)
    }
}

/// Per-stream masking state: holds back only a suffix that could start a value.
pub struct MaskStream {
    masker: Masker,
    pending: Vec<u8>,
}

impl MaskStream {
    pub fn feed(&mut self, chunk: &[u8]) -> Vec<u8> {
        if self.masker.is_empty() {
            return chunk.to_vec();
        }
        self.pending.extend_from_slice(chunk);
        let (out, end) = self.masker.scan(&self.pending, false);
        self.pending.drain(..end);
        out
    }

    /// What is left at end of stream, masked.
    pub fn finish(self) -> Vec<u8> {
        if self.masker.is_empty() {
            return self.pending;
        }
        self.masker.scan(&self.pending, true).0
    }
}

/// A run's resolved secrets.
#[derive(Debug, Default)]
pub struct Secrets {
    /// Values keyed by reference.
    pub values: BTreeMap<String, String>,
    pub masker: Masker,
}

impl Secrets {
    pub fn new(values: BTreeMap<String, String>) -> Self {
        let masker = Masker::new(values.values().map(String::as_str));
        Self { values, masker }
    }

    /// Values a Sprite controller forwarded, checked against what the run needs.
    pub fn forwarded(
        pairs: &[(String, String)],
        mut values: BTreeMap<String, String>,
    ) -> Result<Self> {
        for (name, reference) in pairs {
            if !values.contains_key(reference) {
                bail!("secret {name} ({reference}) was not forwarded by the controller; the controller and runner versions differ");
            }
        }
        values.retain(|reference, _| pairs.iter().any(|(_, r)| r == reference));
        Ok(Self::new(values))
    }
}

/// A secret could not be resolved. `main` reports it as `secret_unavailable`.
#[derive(Debug)]
pub struct Unavailable {
    pub secret: String,
    message: String,
}

impl std::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for Unavailable {}

/// Every `(name, reference)` a selected root job or any job of a called
/// workflow lists, in config order, under each workflow's merged secrets.
pub fn needed(root: &Config, targets: &BTreeMap<String, Config>) -> Vec<(String, String)> {
    let mut pairs = Vec::new();
    for config in std::iter::once(root).chain(targets.values()) {
        for job in &config.jobs {
            for name in &job.secrets {
                if let Some(reference) = config.settings.secrets.get(name) {
                    let pair = (name.clone(), reference.clone());
                    if !pairs.contains(&pair) {
                        pairs.push(pair);
                    }
                }
            }
        }
    }
    pairs
}

/// Read each distinct reference once with `op read`: the first alone, the
/// rest at most [`PARALLEL_READS`] at a time. Never looks for `op` when
/// `pairs` is empty.
pub async fn resolve(
    pairs: &[(String, String)],
    workdir: &Path,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<Secrets> {
    // Distinct references in config order, each with the first name that lists it.
    let mut reads: Vec<(&str, Reference)> = Vec::new();
    for (name, reference) in pairs {
        if reads.iter().any(|(_, r)| r.as_str() == reference) {
            continue;
        }
        let parsed = Reference::parse(reference).map_err(|error| Unavailable {
            secret: name.clone(),
            message: format!("secret {name} ({reference}) could not be read: {error}"),
        })?;
        reads.push((name, parsed));
    }
    let Some(((first_name, first), rest)) = reads.split_first() else {
        return Ok(Secrets::default());
    };
    let Some(op) = crate::sprite::transport::find_on_path("op") else {
        return Err(Unavailable {
            secret: (*first_name).to_owned(),
            message: format!("secret {first_name} ({}) could not be read: the 1Password CLI (`op`) is not installed or not on PATH; see https://developer.1password.com/docs/cli/get-started/", first.as_str()),
        }
        .into());
    };
    let op = Arc::new(op);
    let mut values = BTreeMap::new();
    let first_timeout = timeout(
        FIRST_READ_TIMEOUT,
        "GOODFLOWS_TEST_SECRET_FIRST_READ_TIMEOUT_SECS",
    );
    let read_timeout = timeout(READ_TIMEOUT, "GOODFLOWS_TEST_SECRET_READ_TIMEOUT_SECS");
    let first_value = read(&op, first, workdir, first_timeout, cancel.clone()).await;
    let mut outcomes = vec![(*first_name, first, first_value)];
    if outcomes[0].2.is_ok() {
        let limit = Arc::new(tokio::sync::Semaphore::new(PARALLEL_READS));
        let mut set = tokio::task::JoinSet::new();
        for (index, (_, reference)) in rest.iter().enumerate() {
            let (op, limit, reference) = (op.clone(), limit.clone(), reference.clone());
            let (workdir, cancel) = (workdir.to_owned(), cancel.clone());
            set.spawn(async move {
                let _permit = limit.acquire_owned().await;
                (
                    index,
                    read(&op, &reference, &workdir, read_timeout, cancel).await,
                )
            });
        }
        let mut results: Vec<Option<Result<String, String>>> = vec![None; rest.len()];
        while let Some(joined) = set.join_next().await {
            let (index, result) = joined?;
            results[index] = Some(result);
        }
        outcomes.extend(rest.iter().zip(results).map(|((name, reference), result)| {
            (*name, reference, result.expect("every read finished"))
        }));
    }
    if let Some(signal) = *cancel.borrow() {
        bail!("interrupted by signal {signal} while reading secrets");
    }
    for (_, reference, outcome) in &outcomes {
        if let Ok(value) = outcome {
            values.insert(reference.as_str().to_owned(), value.clone());
        }
    }
    let masker = Masker::new(values.values().map(String::as_str));
    if let Some((name, reference, Err(reason))) = outcomes.iter().find(|o| o.2.is_err()) {
        let mut reason = masker.mask_str(reason);
        if reason.len() > MAX_STDERR_BYTES {
            let mut end = MAX_STDERR_BYTES;
            while !reason.is_char_boundary(end) {
                end -= 1;
            }
            reason.truncate(end);
        }
        return Err(Unavailable {
            secret: (*name).to_owned(),
            message: format!(
                "secret {name} ({}) could not be read: {reason}",
                reference.as_str()
            ),
        }
        .into());
    }
    // Every name, not only the first that lists a shared reference.
    for (name, reference) in pairs {
        if values[reference].len() < MIN_MASKED_BYTES {
            eprintln!("goodflows: warning: secret {name} is shorter than {MIN_MASKED_BYTES} bytes, so its value is not masked in output");
        }
    }
    Ok(Secrets { values, masker })
}

/// A read timeout (debug builds: overridden in seconds by `variable`).
fn timeout(default: Duration, variable: &str) -> Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var(variable).ok().and_then(|v| v.parse().ok()) {
        return Duration::from_secs(secs);
    }
    #[cfg(not(debug_assertions))]
    let _ = variable;
    default
}

/// One `op read`; the error is the reason, never containing the value.
async fn read(
    op: &Path,
    reference: &Reference,
    workdir: &Path,
    timeout: Duration,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<String, String> {
    let Reference::OnePassword(reference) = reference;
    let child = tokio::process::Command::new(op)
        .args(["read", "--no-newline", reference])
        .current_dir(workdir)
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| format!("failed to run op: {error}"))?;
    let output = tokio::select! {
        output = tokio::time::timeout(timeout, child.wait_with_output()) => match output {
            Ok(Ok(output)) => output,
            Ok(Err(error)) => return Err(format!("failed to run op: {error}")),
            Err(_) => return Err(format!("op timed out after {}s", timeout.as_secs())),
        },
        _ = async {
            // A dropped sender can never cancel: keep waiting for the read.
            let cancelled = cancel.borrow().is_some();
            if !cancelled && cancel.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        } => return Err("interrupted".into()),
    };
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let line = stderr.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
        return Err(match output.status.code() {
            Some(code) if line.is_empty() => format!("op exited with {code}"),
            None if line.is_empty() => "op was terminated by a signal".into(),
            _ => line.trim().to_owned(),
        });
    }
    let value = String::from_utf8(output.stdout)
        .map_err(|_| "op returned a value that is not valid UTF-8".to_owned())?;
    if value.contains('\0') {
        return Err("op returned a value that contains NUL".into());
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_op_references() {
        for ok in [
            "op://dev/postgres/url",
            "op://dev/github/2fa/otp",
            "op://dev/github/2fa/otp?attribute=otp",
            "op://dev/postgres/url?attribute=password",
        ] {
            assert_eq!(
                Reference::parse(ok).unwrap(),
                Reference::OnePassword(ok.into())
            );
        }
        let long_ok = format!("op://v/i/{}", "a".repeat(1024 - "op://v/i/".len()));
        assert_eq!(long_ok.len(), 1024);
        Reference::parse(&long_ok).unwrap();
        let too_long = format!("{long_ok}a");
        assert!(Reference::parse(&too_long)
            .unwrap_err()
            .to_string()
            .contains("at most 1024 bytes"));
        for bad in [
            "op://dev//url",
            "op://dev/postgres",
            "op://a/b/c/d/e",
            "op:///a/b/c",
            "op://dev/postgres/url/",
        ] {
            assert!(
                Reference::parse(bad)
                    .unwrap_err()
                    .to_string()
                    .contains("op://vault/item/field"),
                "{bad}"
            );
        }
        for bad in [
            "op://dev/github/2fa/one-time password?attribute=otp",
            "op://dev/postgres/u\trl",
        ] {
            assert!(
                Reference::parse(bad)
                    .unwrap_err()
                    .to_string()
                    .contains("whitespace"),
                "{bad}"
            );
        }
        for bad in [
            "0p://dev/postgres/url",
            "OP://dev/postgres/url",
            "hunter2",
            "",
        ] {
            assert!(
                Reference::parse(bad)
                    .unwrap_err()
                    .to_string()
                    .contains("only op:// references are supported"),
                "{bad}"
            );
        }
    }

    fn masked(masker: &Masker, chunks: &[&[u8]]) -> String {
        let mut stream = masker.stream();
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend(stream.feed(chunk));
        }
        out.extend(stream.finish());
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn masks_values_split_at_every_byte() {
        let masker = Masker::new(["s3cr3t-value"]);
        let text = b"before s3cr3t-value after";
        for split in 0..=text.len() {
            let (a, b) = text.split_at(split);
            assert_eq!(
                masked(&masker, &[a, b]),
                "before [redacted] after",
                "split {split}"
            );
        }
    }

    #[test]
    fn holds_back_only_a_possible_prefix() {
        let masker = Masker::new(["s3cr3t-value"]);
        let mut stream = masker.stream();
        assert_eq!(stream.feed(b"ab s3cr"), b"ab ");
        // The held-back suffix did not start a value and is released.
        assert_eq!(stream.feed(b"atch\n"), b"s3cratch\n");
        assert_eq!(stream.feed(b"plain"), b"plain");
        assert_eq!(stream.feed(b" s"), b" ");
        assert_eq!(stream.finish(), b"s");
    }

    #[test]
    fn longest_value_wins_and_overlaps_are_left_to_right() {
        let masker = Masker::new(["pass", "password1"]);
        assert_eq!(
            masked(&masker, &[b"x password1 pass"]),
            "x [redacted] [redacted]"
        );
        assert_eq!(masked(&masker, &[b"passw", b"ord1"]), "[redacted]");
        let masker = Masker::new(["abab"]);
        assert_eq!(masked(&masker, &[b"ababab"]), "[redacted]ab");
        assert_eq!(masked(&masker, &[b"aba", b"bab"]), "[redacted]ab");
    }

    #[test]
    fn masks_multi_byte_values() {
        let masker = Masker::new(["pässwörd"]);
        let text = "x pässwörd y".as_bytes();
        for split in 0..=text.len() {
            let (a, b) = text.split_at(split);
            assert_eq!(masked(&masker, &[a, b]), "x [redacted] y");
        }
    }

    #[test]
    fn no_values_and_short_values_change_nothing() {
        let masker = Masker::default();
        let mut stream = masker.stream();
        assert_eq!(stream.feed(b"anything \xff"), b"anything \xff");
        assert!(stream.finish().is_empty());
        let masker = Masker::new(["abc"]);
        assert!(masker.is_empty());
        assert_eq!(masked(&masker, &[b"abc abc"]), "abc abc");
        assert_eq!(masker.mask_str("abc"), "abc");
        let masker = Masker::new(["abcd", "xy"]);
        assert_eq!(masker.mask_str("abcd xy"), "[redacted] xy");
    }
}
