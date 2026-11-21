//! Run links: the agent threads and tickets behind the change a run tests.
//!
//! The value rules here are the ones upload validation
//! (`reporting::run_contract`) and goodflows_web share. Collection merges,
//! in order, the `--ticket` and `--thread-url` flags, `GOODFLOWS_TICKETS` and
//! `GOODFLOWS_THREAD_URLS` from the caller's environment, the agent session,
//! then git. It never fails a run and never touches `reporting::source`.

use crate::reporting::run_contract::MAX_URL_BYTES;
use reqwest::Url;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

/// Each list holds at most this many entries.
pub const MAX_ENTRIES: usize = 32;
const MAX_TICKET_BYTES: usize = 64;
const MAX_PREFIX_BYTES: usize = 16;
/// The git scan's own budget, separate from the `source` capture's.
const SCAN_TIMEOUT: Duration = Duration::from_secs(2);
/// The most branch commits a scan reads.
const MAX_COMMITS: usize = 200;
const DEFAULT_ORIGIN: &str = "https://ampcode.com";
const THREAD_LINE: &str = "Amp-Thread-ID:";
const TICKETS_ENV: &str = "GOODFLOWS_TICKETS";
const THREAD_URLS_ENV: &str = "GOODFLOWS_THREAD_URLS";

/// The `links` of a history file and an uploaded run record. Each list is
/// left out when empty, and the record leaves out `links` when both are.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Links {
    /// Thread URLs in normalized form, unique.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub threads: Vec<String>,
    /// Ticket IDs as first spelled, unique ignoring ASCII case.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tickets: Vec<String>,
}

impl Links {
    pub fn is_empty(&self) -> bool {
        self.threads.is_empty() && self.tickets.is_empty()
    }

    /// Add a normalized thread URL unless it is listed or the list is full.
    fn add_thread(&mut self, url: String) {
        if self.threads.len() < MAX_ENTRIES && !self.threads.contains(&url) {
            self.threads.push(url);
        }
    }

    /// Add a valid ticket unless a spelling of it is listed or the list is full.
    fn add_ticket(&mut self, ticket: &str) {
        if self.tickets.len() < MAX_ENTRIES
            && !self.tickets.iter().any(|t| t.eq_ignore_ascii_case(ticket))
        {
            self.tickets.push(ticket.to_owned());
        }
    }
}

/// 1 to 64 bytes of printable ASCII other than `,`: no whitespace, control
/// characters or commas.
pub fn valid_ticket(ticket: &str) -> bool {
    (1..=MAX_TICKET_BYTES).contains(&ticket.len())
        && ticket
            .bytes()
            .all(|b| (b'!'..=b'~').contains(&b) && b != b',')
}

/// `text` as a thread URL in normalized form, the form `reqwest::Url`
/// serializes, or `None` when it breaks a rule: `https`, a host, no
/// userinfo, query or fragment (an empty one counts), RFC 3986 host and path
/// characters, and at most `MAX_URL_BYTES`.
pub fn thread_url(text: &str) -> Option<String> {
    if text.len() > MAX_URL_BYTES {
        return None;
    }
    let url = Url::parse(text).ok()?;
    let host = url.host_str()?;
    let valid = url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.query().is_none()
        && url.fragment().is_none()
        // Only an IPv6 address is bracketed.
        && (host.starts_with('[') || host.bytes().all(|b| unreserved(b) || sub_delim(b)))
        && rfc3986_path(url.path())
        && url.as_str().len() <= MAX_URL_BYTES;
    valid.then(|| url.as_str().to_owned())
}

/// Whether `text` is a thread URL already in normalized form.
pub fn normalized_thread_url(text: &str) -> bool {
    thread_url(text).is_some_and(|url| url == text)
}

fn unreserved(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~')
}

fn sub_delim(b: u8) -> bool {
    matches!(
        b,
        b'!' | b'$' | b'&' | b'\'' | b'(' | b')' | b'*' | b'+' | b',' | b';' | b'='
    )
}

/// Every byte an RFC 3986 `pchar` or `/`, and every `%` the start of a `%`
/// plus two hex digits.
fn rfc3986_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' => {
                let triplet = bytes
                    .get(i + 1..i + 3)
                    .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));
                if !triplet {
                    return false;
                }
                i += 3;
            }
            b if unreserved(b) || sub_delim(b) || matches!(b, b':' | b'@' | b'/') => i += 1,
            _ => return false,
        }
    }
    true
}

/// A `ticket_prefixes` entry: 1 to 16 ASCII alphanumerics starting with a letter.
pub fn valid_prefix(prefix: &str) -> bool {
    prefix.len() <= MAX_PREFIX_BYTES
        && prefix
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic())
        && prefix.bytes().all(|b| b.is_ascii_alphanumeric())
}

/// The value parser for `--ticket`.
pub fn ticket_arg(value: &str) -> Result<String, String> {
    if valid_ticket(value) {
        Ok(value.to_owned())
    } else {
        Err("must be 1-64 bytes of printable ASCII without whitespace or commas".into())
    }
}

/// The value parser for `--thread-url`, which gives the normalized form.
pub fn thread_url_arg(value: &str) -> Result<String, String> {
    thread_url(value).ok_or_else(|| {
        "must be an https URL of at most 2048 bytes with a host, no userinfo, query or \
         fragment, and only RFC 3986 host and path characters"
            .into()
    })
}

/// Tickets in `text` for the configured prefixes, in order: a prefix in any
/// ASCII case, `-`, then one or more digits, with no ASCII alphanumeric
/// before or after. Each is spelled with the configured prefix and the
/// digits as written.
pub fn tickets_in(text: &str, prefixes: &[String]) -> Vec<String> {
    let bytes = text.as_bytes();
    let mut found = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        if i > 0 && bytes[i - 1].is_ascii_alphanumeric() {
            i += 1;
            continue;
        }
        let hit = prefixes.iter().find_map(|prefix| {
            let start = i + prefix.len() + 1;
            let head = bytes.get(i..start - 1)?;
            if !head.eq_ignore_ascii_case(prefix.as_bytes()) || bytes.get(start - 1) != Some(&b'-')
            {
                return None;
            }
            let end = start
                + bytes[start..]
                    .iter()
                    .take_while(|b| b.is_ascii_digit())
                    .count();
            let bounded = end > start && !bytes.get(end).is_some_and(u8::is_ascii_alphanumeric);
            bounded.then(|| (format!("{prefix}-{}", &text[start..end]), end))
        });
        match hit {
            Some((ticket, end)) => {
                found.push(ticket);
                i = end;
            }
            None => i += 1,
        }
    }
    found
}

/// `T-` followed by a lowercase canonical UUID.
fn thread_id(id: &str) -> bool {
    id.strip_prefix("T-")
        .is_some_and(crate::fsutil::canonical_uuid)
}

/// The thread origin: `AMP_URL`, or `https://ampcode.com` when it is unset.
/// `None` unless it is `https` with a host, no userinfo, query or fragment,
/// and a path of `/` or nothing.
fn origin(amp_url: Option<&str>) -> Option<Url> {
    let url = Url::parse(amp_url.unwrap_or(DEFAULT_ORIGIN)).ok()?;
    let valid = url.scheme() == "https"
        && url.host_str().is_some()
        && url.username().is_empty()
        && url.password().is_none()
        && url.path() == "/"
        && url.query().is_none()
        && url.fragment().is_none();
    valid.then_some(url)
}

/// The URL of thread `id` under `origin`, built as `ci::run_url` builds its URL.
fn expand(origin: &Url, id: &str) -> Option<String> {
    thread_url(&format!(
        "{}/threads/{id}",
        origin.origin().ascii_serialization()
    ))
}

/// Thread URLs from every line of `message` that starts, after optional
/// whitespace, with `Amp-Thread-ID:`. A value is a thread URL, or a bare
/// thread ID expanded with `origin`; anything else is dropped.
pub fn thread_lines<'a>(
    message: &'a str,
    origin: Option<&'a Url>,
) -> impl Iterator<Item = String> + 'a {
    message.lines().filter_map(move |line| {
        let value = line.trim_start().strip_prefix(THREAD_LINE)?.trim();
        if thread_id(value) {
            expand(origin?, value)
        } else {
            thread_url(value)
        }
    })
}

/// Environment entries: split on commas and whitespace, empty pieces left out.
fn entries(value: &str) -> impl Iterator<Item = &str> {
    value
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|piece| !piece.is_empty())
}

/// The run's links, merged in order from `flags`, the link variables, the
/// agent session and git, each list capped at `MAX_ENTRIES`. `lookup` reads
/// the caller's process environment, never settings or job `env`. `warn`
/// receives one message per invalid environment entry, naming its variable
/// and position but never its value. Git supplies tickets only for
/// `prefixes`, from commit subjects, `branch` and `GITHUB_HEAD_REF`.
pub async fn collect(
    flags: &Links,
    prefixes: &[String],
    branch: Option<&str>,
    workdir: &Path,
    token_env: Option<&str>,
    lookup: &(dyn Fn(&str) -> Option<String> + Sync),
    warn: &mut (dyn FnMut(String) + Send),
) -> Links {
    let var = |name: &str| lookup(name).filter(|value| !value.is_empty());
    let mut links = Links::default();
    for ticket in &flags.tickets {
        links.add_ticket(ticket);
    }
    for url in &flags.threads {
        links.add_thread(url.clone());
    }
    if let Some(value) = var(TICKETS_ENV) {
        for (n, entry) in entries(&value).enumerate() {
            if valid_ticket(entry) {
                links.add_ticket(entry);
            } else {
                warn(format!(
                    "{TICKETS_ENV} entry {} is not a valid ticket; ignored",
                    n + 1
                ));
            }
        }
    }
    if let Some(value) = var(THREAD_URLS_ENV) {
        for (n, entry) in entries(&value).enumerate() {
            match thread_url(entry) {
                Some(url) => links.add_thread(url),
                None => warn(format!(
                    "{THREAD_URLS_ENV} entry {} is not a valid thread URL; ignored",
                    n + 1
                )),
            }
        }
    }
    let origin = origin(var("AMP_URL").as_deref());
    let session = var("AMP_CURRENT_THREAD_ID").or_else(|| var("AMP_THREAD_ID"));
    if let (Some(id), Some(origin)) = (session, &origin) {
        if let Some(url) = thread_id(&id).then(|| expand(origin, &id)).flatten() {
            links.add_thread(url);
        }
    }
    let messages = scan(workdir, token_env, lookup).await;
    for message in &messages {
        for url in thread_lines(message, origin.as_ref()) {
            links.add_thread(url);
        }
    }
    if !prefixes.is_empty() {
        let head_ref = var("GITHUB_HEAD_REF");
        let subjects = messages.iter().map(|m| m.lines().next().unwrap_or(""));
        for text in subjects.chain(branch).chain(head_ref.as_deref()) {
            for ticket in tickets_in(text, prefixes) {
                if valid_ticket(&ticket) {
                    links.add_ticket(&ticket);
                }
            }
        }
    }
    links
}

/// The messages of the branch's own commits, newest first: at most
/// `MAX_COMMITS` reachable from `HEAD` but not from the base, or `HEAD`'s
/// alone when the checkout is shallow, no base resolves or the range is
/// empty. The base is `origin/$GITHUB_BASE_REF` when that is set, then
/// `origin/HEAD`. A timeout or any git failure gives no messages.
pub async fn scan(
    workdir: &Path,
    token_env: Option<&str>,
    lookup: &(dyn Fn(&str) -> Option<String> + Sync),
) -> Vec<String> {
    let base_ref = lookup("GITHUB_BASE_REF").filter(|name| !name.is_empty());
    let scan = messages(workdir, token_env, base_ref.as_deref());
    tokio::time::timeout(SCAN_TIMEOUT, scan)
        .await
        .unwrap_or_default()
}

async fn messages(workdir: &Path, token_env: Option<&str>, base_ref: Option<&str>) -> Vec<String> {
    let git = |args: Vec<String>| git(workdir, token_env, args);
    let args = |args: &[&str]| args.iter().map(|a| a.to_string()).collect::<Vec<_>>();
    let Some(shallow) = git(args(&["rev-parse", "--is-shallow-repository"])).await else {
        return Vec::new();
    };
    // Below a partial depth's boundary, old base commits look like the
    // branch's own.
    if shallow.trim() == "false" {
        let candidates = base_ref
            .map(|name| format!("refs/remotes/origin/{name}"))
            .into_iter()
            .chain(["refs/remotes/origin/HEAD".to_owned()]);
        for candidate in candidates {
            let verify = vec![
                "rev-parse".into(),
                "--verify".into(),
                "--quiet".into(),
                format!("{candidate}^{{commit}}"),
            ];
            let Some(base) = git(verify).await else {
                continue;
            };
            let log = vec![
                "log".into(),
                "-z".into(),
                format!("--max-count={MAX_COMMITS}"),
                "--format=%B".into(),
                "HEAD".into(),
                "--not".into(),
                base.trim().to_owned(),
            ];
            if let Some(listed) = git(log).await.map(|out| split(&out)) {
                if !listed.is_empty() {
                    return listed;
                }
            }
            break;
        }
    }
    git(args(&["log", "-z", "-1", "--format=%B", "HEAD"]))
        .await
        .map(|out| split(&out))
        .unwrap_or_default()
}

/// `git log -z` output as one message per commit.
fn split(output: &str) -> Vec<String> {
    output
        .split('\0')
        .filter(|message| !message.trim().is_empty())
        .map(str::to_owned)
        .collect()
}

/// A git command's stdout, `None` when it could not run or failed.
async fn git(workdir: &Path, token_env: Option<&str>, args: Vec<String>) -> Option<String> {
    let mut command = Command::new("git");
    command.args(&args).current_dir(workdir);
    if let Some(token_env) = token_env {
        command.env_remove(token_env);
    }
    let output = command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::path::PathBuf;

    const UUID: &str = "3bb6cad7-8c04-413a-b23c-5fca67cd4f7b";
    const OTHER: &str = "0e54fb55-a115-4ac7-80fe-8fde26e76afb";

    fn thread(id: &str) -> String {
        format!("https://ampcode.com/threads/T-{id}")
    }

    #[test]
    fn tickets_are_printable_ascii_without_commas() {
        assert!(valid_ticket("GOO-291"));
        assert!(valid_ticket(&"a".repeat(64)));
        assert!(!valid_ticket(&"a".repeat(65)));
        assert!(!valid_ticket(""));
        assert!(valid_ticket("!~"));
        for bad in ["GOO\t1", "GOO 1", "GOO-1,GOO-2", "GOO-é", "GOO-\u{7f}"] {
            assert!(!valid_ticket(bad), "{bad:?}");
        }
        assert!(ticket_arg("GOO-1,GOO-2").is_err());
        assert_eq!(ticket_arg("goo-1"), Ok("goo-1".into()));
    }

    #[test]
    fn thread_urls_need_https_a_host_and_nothing_secret() {
        for bad in [
            "http://ampcode.com/threads/T-x",
            "https://u@h/",
            "https://u:p@h/",
            "https://h/x?a=1",
            "https://h/x?",
            "https://h/x#m",
            "https://h/x#",
            "T-x",
            "",
        ] {
            assert_eq!(thread_url(bad), None, "{bad}");
        }
        assert_eq!(thread_url(&thread(UUID)), Some(thread(UUID)));
    }

    #[test]
    fn thread_url_paths_and_hosts_are_rfc_3986() {
        for bad in [
            "https://h/a|b",
            "https://h/a^b",
            "https://h/a[b",
            "https://h/a]b",
            "https://h/a%zzb",
            "https://h/a%",
            "https://h/a%4",
            "https://a\"b.com/x",
            "https://a`b.com/x",
            "https://a{b.com/x",
            "https://a}b.com/x",
        ] {
            assert_eq!(thread_url(bad), None, "{bad}");
        }
        assert_eq!(
            thread_url("https://[::1]/x"),
            Some("https://[::1]/x".into())
        );
        assert_eq!(
            thread_url("https://a!$&'()*+,;=b.com/x"),
            Some("https://a!$&'()*+,;=b.com/x".into())
        );
        assert_eq!(
            thread_url("https://h/a-._~!$&'()*+,;=:@/%41"),
            Some("https://h/a-._~!$&'()*+,;=:@/%41".into())
        );
    }

    #[test]
    fn thread_urls_are_at_most_2048_bytes() {
        let prefix = "https://h/";
        let fits = format!("{prefix}{}", "a".repeat(MAX_URL_BYTES - prefix.len()));
        assert_eq!(thread_url(&fits), Some(fits.clone()));
        assert_eq!(thread_url(&format!("{fits}a")), None);
    }

    #[test]
    fn thread_urls_are_normalized_with_triplets_kept() {
        for (given, normalized) in [
            ("https://h:443/a/../b", "https://h/b"),
            ("https://h", "https://h/"),
            ("https://h/a/./b", "https://h/a/b"),
            (
                "HTTPS://AmpCode.com/threads/T-x",
                "https://ampcode.com/threads/T-x",
            ),
            ("https://h/a%7Eb", "https://h/a%7Eb"),
            ("https://h/a%2fb", "https://h/a%2fb"),
            ("https://h:8443/x", "https://h:8443/x"),
        ] {
            assert_eq!(thread_url(given).as_deref(), Some(normalized), "{given}");
            assert_eq!(normalized_thread_url(given), given == normalized, "{given}");
        }
        assert!(normalized_thread_url("https://h/a%7Eb"));
        assert!(!normalized_thread_url("https://h/a/%2E%2e/b"));
        assert!(!normalized_thread_url("https://ex%61mple.com/x"));
        assert!(!normalized_thread_url("https://127.1/x"));
        assert!(!normalized_thread_url("https://h:0443/x"));
        assert!(!normalized_thread_url("https://[0:0:0:0:0:0:0:1]/x"));
    }

    #[test]
    fn merging_deduplicates_and_stops_at_32() {
        let mut links = Links::default();
        for url in [
            "HTTPS://AmpCode.com/threads/T-x",
            "https://ampcode.com/threads/T-x",
        ] {
            links.add_thread(thread_url(url).unwrap());
        }
        assert_eq!(links.threads, ["https://ampcode.com/threads/T-x"]);
        for ticket in ["goo-1", "GOO-1", "GOO-01"] {
            links.add_ticket(ticket);
        }
        // The first spelling wins; the digits are compared as written.
        assert_eq!(links.tickets, ["goo-1", "GOO-01"]);
        let mut links = Links::default();
        for n in 0..33 {
            links.add_ticket(&format!("T-{n}"));
            links.add_thread(format!("https://h/{n}"));
        }
        assert_eq!(links.tickets.len(), 32);
        assert_eq!(links.threads.len(), 32);
        assert_eq!(links.tickets.last().unwrap(), "T-31");
        assert_eq!(links.threads.last().unwrap(), "https://h/31");
    }

    #[test]
    fn empty_lists_are_left_out() {
        let links = Links {
            threads: Vec::new(),
            tickets: vec!["GOO-1".into()],
        };
        assert_eq!(
            serde_json::to_string(&links).unwrap(),
            r#"{"tickets":["GOO-1"]}"#
        );
        assert_eq!(serde_json::to_string(&Links::default()).unwrap(), "{}");
    }

    #[test]
    fn prefixes_are_short_alphanumerics_starting_with_a_letter() {
        assert!(valid_prefix("GOO"));
        assert!(valid_prefix("a1"));
        assert!(valid_prefix(&"A".repeat(16)));
        for bad in ["", "1GO", "GO-O", "GO_O", "GÖ", &"A".repeat(17)] {
            assert!(!valid_prefix(bad), "{bad}");
        }
    }

    #[test]
    fn tickets_match_whole_prefixed_numbers() {
        let goo = ["GOO".to_owned()];
        assert_eq!(
            tickets_in("MEGOO-12 GOO-7, goo-0042b and goo-15", &goo),
            ["GOO-7", "GOO-15"]
        );
        assert_eq!(tickets_in("tbrewer/goo-291-fix", &goo), ["GOO-291"]);
        assert_eq!(tickets_in("fix GOO-", &goo), Vec::<String>::new());
        assert_eq!(tickets_in("GOO-", &goo), Vec::<String>::new());
        assert_eq!(tickets_in("GOO-0042", &goo), ["GOO-0042"]);
        assert_eq!(tickets_in("é GOO-1é", &goo), ["GOO-1"]);
        let two = ["GOO".to_owned(), "eng".to_owned()];
        assert_eq!(
            tickets_in("ENG-4: follow GOO-3 and eng-5", &two),
            ["eng-4", "GOO-3", "eng-5"]
        );
        assert!(tickets_in("GOO-291: Fix UTF-8 handling", &[]).is_empty());
    }

    #[test]
    fn amp_thread_id_lines_supply_threads() {
        let origin = Url::parse("https://ampcode.com").unwrap();
        let lines = |message: &str| thread_lines(message, Some(&origin)).collect::<Vec<_>>();
        assert_eq!(
            lines(&format!("Subject\n\nAmp-Thread-ID: {}\n", thread(UUID))),
            [thread(UUID)]
        );
        assert_eq!(
            lines(&format!("Subject\n\nAmp-Thread-ID: T-{UUID}")),
            [thread(UUID)]
        );
        let custom = Url::parse("https://amp.example:8443/").unwrap();
        assert_eq!(
            thread_lines(&format!("Amp-Thread-ID: T-{UUID}"), Some(&custom)).collect::<Vec<_>>(),
            [format!("https://amp.example:8443/threads/T-{UUID}")]
        );
        // A bare ID needs an origin.
        assert_eq!(
            thread_lines(&format!("Amp-Thread-ID: T-{UUID}"), None).count(),
            0
        );
        assert_eq!(
            lines(&format!("Squash (#45)\n\n* first\n\n    Amp-Thread-ID: {}\n* second\n  Amp-Thread-ID: T-{OTHER}\nCo-authored-by: x\n", thread(UUID))),
            [thread(UUID), thread(OTHER)]
        );
        assert!(lines(&format!("Amp-Thread-ID: T-{}", UUID.to_uppercase())).is_empty());
        assert!(lines(&format!("Amp-Thread-ID: {}?token=x", thread(UUID))).is_empty());
        assert!(lines(&format!("Not-Amp-Thread-ID: {}", thread(UUID))).is_empty());
    }

    fn git(dir: &Path, args: &[&str]) -> String {
        let output = std::process::Command::new("git")
            .args(["-c", "user.email=t@example.com", "-c", "user.name=t"])
            .args([
                "-c",
                "commit.gpgsign=false",
                "-c",
                "init.defaultBranch=main",
            ])
            // No hook of the machine's adds trailers to the messages.
            .args(["-c", "core.hooksPath=/nonexistent"])
            .args(args)
            .current_dir(dir)
            .stderr(Stdio::null())
            .output()
            .unwrap();
        assert!(output.status.success(), "git {args:?}");
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    fn commit(dir: &Path, message: &str) -> String {
        git(dir, &["commit", "-q", "--allow-empty", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    /// A checkout with base commits `base 1` and `base 2`.
    fn repo() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        git(&root, &["init", "-q"]);
        commit(&root, "base 1");
        commit(&root, "base 2");
        (dir, root)
    }

    fn env(vars: &[(&str, &str)]) -> impl Fn(&str) -> Option<String> + Sync {
        let vars: HashMap<String, String> = vars
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        move |name| vars.get(name).cloned()
    }

    async fn scanned(dir: &Path, vars: &[(&str, &str)]) -> Vec<String> {
        scan(dir, Some("GOODFLOWS_API_KEY"), &env(vars))
            .await
            .iter()
            .map(|m| m.trim_end().to_owned())
            .collect()
    }

    #[tokio::test]
    async fn a_scan_reads_the_branch_commits_newest_first() {
        let (_dir, root) = repo();
        let base = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["update-ref", "refs/remotes/origin/HEAD", &base]);
        git(&root, &["checkout", "-q", "-b", "feature"]);
        commit(&root, "one\n\nbody");
        commit(&root, "two");
        assert_eq!(scanned(&root, &[]).await, ["two", "one\n\nbody"]);
    }

    #[tokio::test]
    async fn an_empty_range_or_no_base_reads_head_alone() {
        let (_dir, root) = repo();
        // No origin/HEAD.
        assert_eq!(scanned(&root, &[]).await, ["base 2"]);
        // HEAD is the base, as on main.
        let head = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["update-ref", "refs/remotes/origin/HEAD", &head]);
        assert_eq!(scanned(&root, &[]).await, ["base 2"]);
    }

    #[tokio::test]
    async fn github_base_ref_chooses_the_base() {
        let (_dir, root) = repo();
        let first = git(&root, &["rev-parse", "HEAD~1"]);
        git(&root, &["update-ref", "refs/remotes/origin/HEAD", &first]);
        commit(&root, "release");
        let release = git(&root, &["rev-parse", "HEAD"]);
        git(
            &root,
            &["update-ref", "refs/remotes/origin/release", &release],
        );
        commit(&root, "feature");
        assert_eq!(scanned(&root, &[]).await, ["feature", "release", "base 2"]);
        assert_eq!(
            scanned(&root, &[("GITHUB_BASE_REF", "release")]).await,
            ["feature"]
        );
        // A base that does not resolve falls back to origin/HEAD.
        assert_eq!(
            scanned(&root, &[("GITHUB_BASE_REF", "gone")]).await,
            ["feature", "release", "base 2"]
        );
    }

    #[tokio::test]
    async fn a_shallow_checkout_reads_head_alone() {
        let (_dir, source) = repo();
        git(&source, &["checkout", "-q", "-b", "feature"]);
        commit(&source, "feature");
        git(&source, &["checkout", "-q", "main"]);
        commit(&source, "base 3");
        commit(&source, "base 4");
        let clone = tempfile::tempdir().unwrap();
        let url = format!("file://{}", source.display());
        git(
            clone.path(),
            &[
                "clone",
                "-q",
                "--depth",
                "2",
                "--no-single-branch",
                &url,
                "c",
            ],
        );
        let checkout = clone.path().join("c");
        git(&checkout, &["checkout", "-q", "feature"]);
        // `HEAD --not origin/HEAD` would also list `base 2`, cut off from
        // main by the shallow boundary.
        let range = git(
            &checkout,
            &["log", "--format=%s", "HEAD", "--not", "origin/HEAD"],
        );
        assert_eq!(range, "feature\nbase 2");
        assert_eq!(scanned(&checkout, &[]).await, ["feature"]);
    }

    #[tokio::test]
    async fn outside_a_checkout_a_scan_reads_nothing() {
        let dir = tempfile::tempdir().unwrap();
        assert!(scanned(dir.path(), &[]).await.is_empty());
    }

    /// Collect outside any checkout, so git adds nothing.
    async fn collected(flags: &Links, vars: &[(&str, &str)]) -> (Links, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let mut warnings = Vec::new();
        let links = collect(
            flags,
            &["GOO".to_owned()],
            None,
            dir.path(),
            None,
            &env(vars),
            &mut |warning| warnings.push(warning),
        )
        .await;
        (links, warnings)
    }

    #[tokio::test]
    async fn invalid_environment_entries_are_warned_by_position() {
        let long = "x".repeat(65);
        let (links, warnings) = collected(
            &Links::default(),
            &[
                (TICKETS_ENV, &format!("GOO-1,, \n{long} GOO-2,")),
                (THREAD_URLS_ENV, "https://h/a?token=secret , https://h/b"),
            ],
        )
        .await;
        assert_eq!(links.tickets, ["GOO-1", "GOO-2"]);
        assert_eq!(links.threads, ["https://h/b"]);
        assert_eq!(
            warnings,
            [
                "GOODFLOWS_TICKETS entry 2 is not a valid ticket; ignored",
                "GOODFLOWS_THREAD_URLS entry 1 is not a valid thread URL; ignored",
            ]
        );
    }

    #[tokio::test]
    async fn the_session_adds_its_thread_under_a_checked_origin() {
        let id = format!("T-{UUID}");
        let session = |amp_url: &'static str| {
            let id = id.clone();
            async move {
                let mut vars = vec![("AMP_CURRENT_THREAD_ID", id.as_str())];
                if !amp_url.is_empty() {
                    vars.push(("AMP_URL", amp_url));
                }
                collected(&Links::default(), &vars).await.0.threads
            }
        };
        assert_eq!(session("").await, [thread(UUID)]);
        assert_eq!(session("https://ampcode.com/").await, [thread(UUID)]);
        assert_eq!(
            session("https://Amp.Example").await,
            [format!("https://amp.example/threads/T-{UUID}")]
        );
        for bad in [
            "https://ampcode.com/app",
            "http://ampcode.com",
            "https://ampcode.com/?a=1",
            "https://ampcode.com/#x",
            "https://u@ampcode.com",
        ] {
            assert!(session(bad).await.is_empty(), "{bad}");
        }
        // The current thread wins over AMP_THREAD_ID, which is the fallback.
        let other = format!("T-{OTHER}");
        let (links, _) = collected(
            &Links::default(),
            &[("AMP_CURRENT_THREAD_ID", &id), ("AMP_THREAD_ID", &other)],
        )
        .await;
        assert_eq!(links.threads, [thread(UUID)]);
        let (links, _) = collected(&Links::default(), &[("AMP_THREAD_ID", &other)]).await;
        assert_eq!(links.threads, [thread(OTHER)]);
        // An ID that is not `T-` and a lowercase UUID adds nothing.
        let upper = format!("T-{}", UUID.to_uppercase());
        let (links, _) = collected(&Links::default(), &[("AMP_CURRENT_THREAD_ID", &upper)]).await;
        assert!(links.is_empty());
    }

    #[tokio::test]
    async fn flags_come_first_and_the_cap_spans_sources() {
        let flags = Links {
            threads: Vec::new(),
            tickets: (1..=30).map(|n| format!("F-{n}")).collect(),
        };
        let (links, _) = collected(&flags, &[(TICKETS_ENV, "E-1,E-2,E-3,E-4,E-5")]).await;
        let mut expected = flags.tickets.clone();
        expected.extend(["E-1".into(), "E-2".into()]);
        assert_eq!(links.tickets, expected);
        let flags = Links {
            threads: Vec::new(),
            tickets: vec!["goo-291".into()],
        };
        let (links, _) = collected(&flags, &[(TICKETS_ENV, "GOO-291,GOO-300")]).await;
        assert_eq!(links.tickets, ["goo-291", "GOO-300"]);
    }

    #[tokio::test]
    async fn git_supplies_threads_from_messages_and_tickets_from_subjects() {
        let (_dir, root) = repo();
        let base = git(&root, &["rev-parse", "HEAD"]);
        git(&root, &["update-ref", "refs/remotes/origin/HEAD", &base]);
        git(&root, &["checkout", "-q", "-b", "tbrewer/goo-291-fix"]);
        commit(
            &root,
            &format!("Start ENG-1\n\nfollow-up to GOO-200\n\nAmp-Thread-ID: T-{OTHER}"),
        );
        commit(
            &root,
            &format!("GOO-7 more\n\nAmp-Thread-ID: {}", thread(UUID)),
        );
        let mut warnings = Vec::new();
        let links = collect(
            &Links::default(),
            &["GOO".to_owned()],
            Some("tbrewer/goo-291-fix"),
            &root,
            None,
            &env(&[("GITHUB_HEAD_REF", "goo-300-pr")]),
            &mut |warning| warnings.push(warning),
        )
        .await;
        assert_eq!(links.threads, [thread(UUID), thread(OTHER)]);
        assert_eq!(links.tickets, ["GOO-7", "GOO-291", "GOO-300"]);
        assert!(warnings.is_empty());
        // Without prefixes git supplies no tickets.
        let links = collect(
            &Links::default(),
            &[],
            Some("tbrewer/goo-291-fix"),
            &root,
            None,
            &env(&[]),
            &mut |_| {},
        )
        .await;
        assert!(links.tickets.is_empty());
        assert_eq!(links.threads.len(), 2);
    }
}
