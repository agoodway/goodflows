//! The remote cache store against a local fake GoodFlows server. No test
//! reaches a network service.

mod common;
use common::mock::{Mock, Reply, Request};
use common::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::{Arc, Mutex};

const PROJECT: &str = "app";
const TOKEN: &str = "ck_test_cache_key";

#[derive(Clone)]
struct Stored {
    key: String,
    partial: String,
    scope: String,
    sha256: String,
    object: String,
    order: usize,
}

#[derive(Default)]
struct State {
    base: String,
    entries: Vec<Stored>,
    /// upload id -> (reservation body, key, expected upload header)
    uploads: HashMap<String, (Value, String, String)>,
    objects: HashMap<String, Vec<u8>>,
    next: usize,
    /// Status overrides.
    lookup: Option<u16>,
    reserve: Option<u16>,
    put: Option<u16>,
    commit: Option<u16>,
    /// 403 for every lookup that sends a scope.
    forbid_scoped: bool,
    /// Serve downloads with one byte changed.
    tamper: bool,
    /// Send half of each download, then stall.
    stall: bool,
    /// Never answer an upload.
    stall_put: bool,
    /// Never answer a lookup.
    stall_lookup: bool,
    /// Answer `Range` requests with the whole object, like a store without
    /// range support.
    ignore_range: bool,
    /// Fail this many ranged downloads after the first one with a `503`.
    flaky_ranges: usize,
    /// Answer an upload two seconds after its body, like a store finishing
    /// a long upload.
    slow_put: bool,
    /// upload id -> (length, base64 MD5) of each part, for a multipart
    /// reservation.
    multipart: HashMap<String, Vec<(u64, String)>>,
    /// (upload id, part number) -> the part's stored bytes.
    part_bodies: HashMap<(String, usize), Vec<u8>>,
    /// Part `PUT`s being answered now, and the most at once.
    parts_in_flight: usize,
    max_parts_in_flight: usize,
    /// Reject `parts` on reserve like a server without multipart support.
    no_multipart: bool,
    /// Ignore `parts` and return a single `upload_url`.
    single_url: bool,
    /// Fail this many part uploads with a `503`.
    flaky_parts: usize,
    /// Flip one received byte of this part number, so its MD5 never
    /// matches.
    corrupt_part: Option<usize>,
    /// Return one part fewer than requested.
    short_parts: bool,
}

struct Fake {
    mock: Mock,
    state: Arc<Mutex<State>>,
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

fn decode(text: &str) -> String {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => {
                out.push(u8::from_str_radix(&text[i + 1..i + 3], 16).unwrap());
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8(out).unwrap()
}

/// `(path, query)` of a request.
fn split(request: &Request) -> (String, HashMap<String, String>) {
    let (path, query) = request
        .path
        .split_once('?')
        .unwrap_or((request.path.as_str(), ""));
    let query = query
        .split('&')
        .filter(|p| !p.is_empty())
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (decode(k), decode(v))
        })
        .collect();
    (path.to_owned(), query)
}

fn status(code: u16, body: Value) -> Reply {
    Reply::Status(code, vec![], body.to_string())
}

fn md5_base64(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(md5::Md5::digest(bytes))
}

/// A part `PUT`, enforced like a part URL presigned with its
/// `content-length` and `content-md5`.
fn put_part(state: &mut State, id: &str, number: usize, request: &Request) -> Reply {
    if state.flaky_parts > 0 {
        state.flaky_parts -= 1;
        return status(503, json!({}));
    }
    let Some((len, md5)) = state
        .multipart
        .get(id)
        .and_then(|parts| parts.get(number.wrapping_sub(1)))
        .cloned()
    else {
        return status(404, json!({}));
    };
    // A missing or different signed header fails the signature.
    if request.headers.get("content-length") != Some(&len.to_string())
        || request.headers.get("content-md5") != Some(&md5)
    {
        return status(403, json!({}));
    }
    let mut body = request.body.clone();
    if state.corrupt_part == Some(number) {
        body[0] ^= 1;
    }
    if body.len() as u64 != len || md5_base64(&body) != md5 {
        return status(400, json!({"Code": "BadDigest"}));
    }
    state.part_bodies.insert((id.to_owned(), number), body);
    status(200, json!({}))
}

/// [`handle`], recording how many part `PUT`s are answered at once; each
/// takes 50 ms so concurrent ones overlap.
fn serve(state: &Mutex<State>, request: &Request) -> Reply {
    let part = request.method == "PUT" && request.path.contains("/parts/");
    if part {
        let mut s = state.lock().unwrap();
        s.parts_in_flight += 1;
        s.max_parts_in_flight = s.max_parts_in_flight.max(s.parts_in_flight);
        drop(s);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let reply = handle(state, request);
    if part {
        state.lock().unwrap().parts_in_flight -= 1;
    }
    reply
}

fn hit(state: &State, entry: &Stored) -> Value {
    let bytes = &state.objects[&entry.object];
    json!({"data": {"key": entry.key, "partial": entry.partial, "name": "deps", "scope": entry.scope,
        "size": bytes.len(), "sha256": entry.sha256, "created_at": "2026-11-18T00:00:00Z",
        "download_url": format!("{}/objects/{}?X-Amz-Signature=sig", state.base, entry.object),
        "download_url_expires_at": "2026-11-18T01:00:00Z"}})
}

fn handle(state: &Mutex<State>, request: &Request) -> Reply {
    let mut state = state.lock().unwrap();
    let (path, query) = split(request);
    let api = format!("/api/v1/projects/{PROJECT}/caches/");
    if let Some(object) = path.strip_prefix("/objects/") {
        if request.method == "PUT" {
            if state.stall_put {
                return Reply::Hang(std::time::Duration::from_secs(30));
            }
            if let Some(code) = state.put {
                return status(code, json!({}));
            }
            if let Some((id, number)) = object.split_once("/parts/") {
                return put_part(&mut state, id, number.parse().unwrap(), request);
            }
            let (body, _, header) = state.uploads[object].clone();
            let sha = hex(Sha256::digest(&request.body));
            let headers_ok = request.headers.get("x-amz-checksum-sha256") == Some(&header)
                && request.headers.get("content-type").map(String::as_str)
                    == Some("application/gzip");
            if sha != body["sha256"] || !headers_ok || request.body.len() as u64 != body["size"] {
                return status(400, json!({}));
            }
            state
                .objects
                .insert(object.to_owned(), request.body.clone());
            if state.slow_put {
                drop(state);
                std::thread::sleep(std::time::Duration::from_secs(2));
            }
            return status(200, json!({}));
        }
        let mut bytes = state.objects[object].clone();
        if state.tamper {
            bytes[0] ^= 1;
        }
        if state.stall {
            return Reply::Stall(bytes, std::time::Duration::from_secs(30));
        }
        let range = request.headers.get("range").cloned();
        if let Some(range) = range.filter(|_| !state.ignore_range) {
            let (start, end) = range
                .strip_prefix("bytes=")
                .and_then(|r| r.split_once('-'))
                .map(|(s, e)| (s.parse::<usize>().unwrap(), e.parse::<usize>().unwrap()))
                .unwrap();
            if start > 0 && state.flaky_ranges > 0 {
                state.flaky_ranges -= 1;
                return status(503, json!({}));
            }
            let end = end.min(bytes.len() - 1);
            let content_range = format!("bytes {start}-{end}/{}", bytes.len());
            return Reply::BytesWith(
                206,
                vec![("Content-Range".into(), content_range)],
                bytes[start..=end].to_vec(),
            );
        }
        return Reply::Bytes(200, bytes);
    }
    let Some(rest) = path.strip_prefix(&api) else {
        return status(404, json!({}));
    };
    if request.headers.get("authorization").map(String::as_str) != Some(&format!("Bearer {TOKEN}"))
    {
        return status(401, json!({}));
    }
    let scope = query.get("scope").cloned();
    match (request.method.as_str(), rest) {
        ("GET", rest) if rest.starts_with("entries") => {
            if state.stall_lookup {
                return Reply::Hang(std::time::Duration::from_secs(30));
            }
            if let Some(code) = state.lookup {
                return status(code, json!({}));
            }
            if state.forbid_scoped && scope.is_some() {
                return status(403, json!({}));
            }
            let order: Vec<String> = match &scope {
                Some(s) => vec![s.clone(), "main".into()],
                None => vec!["main".into()],
            };
            let key = rest.strip_prefix("entries/");
            for scope in order {
                let found = state
                    .entries
                    .iter()
                    .filter(|e| e.scope == scope)
                    .filter(|e| match key {
                        Some(key) => e.key == key,
                        None => Some(&e.partial) == query.get("partial"),
                    })
                    .max_by_key(|e| e.order)
                    .cloned();
                if let Some(entry) = found {
                    return status(200, hit(&state, &entry));
                }
            }
            status(404, json!({}))
        }
        ("POST", rest) if rest.starts_with("entries/") => {
            if let Some(code) = state.reserve {
                return status(code, json!({"errors": {"scope": ["is invalid"]}}));
            }
            let key = rest.strip_prefix("entries/").unwrap().to_owned();
            let body = request.json();
            let parts = body.get("parts").filter(|p| !p.is_null()).cloned();
            if parts.is_some() && state.no_multipart {
                // goodflows_web without multipart support refuses unknown keys.
                return status(422, json!({"errors": {"parts": ["is not allowed"]}}));
            }
            if state
                .entries
                .iter()
                .any(|e| e.key == key && e.scope == body["scope"])
            {
                return status(409, json!({}));
            }
            state.next += 1;
            let id = format!("upload-{}", state.next);
            let header = format!("sum-{}", body["sha256"].as_str().unwrap());
            state
                .uploads
                .insert(id.clone(), (body.clone(), key, header.clone()));
            if let Some(parts) = parts.filter(|_| !state.single_url) {
                let size = body["size"].as_u64().unwrap();
                let part = parts["size"].as_u64().unwrap();
                let md5s: Vec<String> = parts["md5"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|m| m.as_str().unwrap().to_owned())
                    .collect();
                let count = size.div_ceil(part);
                if md5s.len() as u64 != count {
                    return status(
                        422,
                        json!({"errors": {"parts": [format!("md5 must have {count} entries")]}}),
                    );
                }
                let lengths = (0..count).map(|i| part.min(size - i * part));
                let declared: Vec<(u64, String)> = lengths.zip(md5s).collect();
                let mut listed: Vec<Value> = declared
                    .iter()
                    .enumerate()
                    .map(|(i, (len, md5))| {
                        json!({"number": i + 1,
                            "url": format!("{}/objects/{id}/parts/{}?X-Amz-Signature=sig", state.base, i + 1),
                            "headers": {"content-length": len.to_string(), "content-md5": md5}})
                    })
                    .collect();
                if state.short_parts {
                    listed.pop();
                }
                state.multipart.insert(id.clone(), declared);
                return status(
                    201,
                    json!({"data": {"upload_id": id, "upload_parts": listed,
                        "upload_url_expires_at": "x", "expires_at": "x"}}),
                );
            }
            status(
                201,
                json!({"data": {"upload_id": id, "upload_url": format!("{}/objects/{id}?X-Amz-Signature=sig", state.base),
                    "upload_headers": {"content-length": body["size"].to_string(), "content-type": "application/gzip",
                        "x-amz-checksum-sha256": header},
                    "upload_url_expires_at": "x", "expires_at": "x"}}),
            )
        }
        ("POST", rest) if rest.starts_with("uploads/") => {
            if let Some(code) = state.commit {
                return status(code, json!({}));
            }
            let id = rest
                .strip_prefix("uploads/")
                .unwrap()
                .strip_suffix("/commit")
                .unwrap()
                .to_owned();
            let (body, key, _) = state.uploads[&id].clone();
            if let Some(parts) = state.multipart.get(&id).cloned() {
                // Every part, joined, must be the reserved archive.
                let mut joined = Vec::new();
                for number in 1..=parts.len() {
                    match state.part_bodies.get(&(id.clone(), number)) {
                        Some(bytes) => joined.extend_from_slice(bytes),
                        None => return status(422, json!({})),
                    }
                }
                if joined.len() as u64 != body["size"]
                    || hex(Sha256::digest(&joined)) != body["sha256"]
                {
                    return status(422, json!({}));
                }
                state.objects.insert(id.clone(), joined);
            }
            if !state.objects.contains_key(&id) {
                return status(422, json!({}));
            }
            state.next += 1;
            let entry = Stored {
                key,
                partial: body["partial"].as_str().unwrap().into(),
                scope: body["scope"].as_str().unwrap().into(),
                sha256: body["sha256"].as_str().unwrap().into(),
                object: id,
                order: state.next,
            };
            state.entries.push(entry.clone());
            status(200, hit(&state, &entry))
        }
        _ => status(404, json!({})),
    }
}

impl Fake {
    fn start() -> Self {
        let state = Arc::new(Mutex::new(State::default()));
        let shared = state.clone();
        let mock = Mock::start(move |request, _| serve(&shared, request));
        state.lock().unwrap().base = mock.url.clone();
        Self { mock, state }
    }

    fn set(&self, change: impl FnOnce(&mut State)) {
        change(&mut self.state.lock().unwrap());
    }

    /// Cache API and object requests, in order.
    fn requests(&self) -> Vec<Request> {
        self.mock
            .requests()
            .into_iter()
            .filter(|r| r.path.contains("/caches/") || r.path.starts_with("/objects/"))
            .collect()
    }

    /// Part `PUT`s, in order, as (upload id, part number, request).
    fn part_puts(&self) -> Vec<(String, usize, Request)> {
        self.requests()
            .into_iter()
            .filter(|r| r.method == "PUT")
            .filter_map(|r| {
                let (path, _) = split(&r);
                let (id, number) = path.strip_prefix("/objects/")?.split_once("/parts/")?;
                Some((id.to_owned(), number.parse().unwrap(), r))
            })
            .collect()
    }

    fn lookups(&self) -> Vec<(String, HashMap<String, String>)> {
        self.requests()
            .iter()
            .filter(|r| r.method == "GET" && r.path.contains("/caches/entries"))
            .map(split)
            .collect()
    }

    fn scopes(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .entries
            .iter()
            .map(|e| e.scope.clone())
            .collect()
    }
}

struct Machine {
    _dir: tempfile::TempDir,
    cache_home: PathBuf,
    home: PathBuf,
}

impl Machine {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        Self {
            cache_home: root.join("xdg"),
            home: root.join("home"),
            _dir: dir,
        }
    }

    fn run(&self, p: &Project, url: &str, args: &[&str], vars: &[(&str, &str)]) -> Output {
        self.command(p, url, args, vars).output().unwrap()
    }

    fn command(
        &self,
        p: &Project,
        url: &str,
        args: &[&str],
        vars: &[(&str, &str)],
    ) -> std::process::Command {
        let mut command = p.command();
        command
            .env("XDG_CACHE_HOME", &self.cache_home)
            .env("HOME", &self.home)
            .env("GOODFLOWS_URL", url)
            .env("GOODFLOWS_CACHE_KEY", TOKEN)
            .env_remove("GITHUB_HEAD_REF")
            .env_remove("GITHUB_REF_NAME")
            .args(args);
        for (key, value) in vars {
            command.env(key, value);
        }
        command
    }

    fn entries(&self) -> Vec<PathBuf> {
        files_under(&self.cache_home)
            .into_iter()
            .filter(|f| f.extension().is_some_and(|e| e == "tgz"))
            .collect()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
}

fn config(extra: &str, caches: &str) -> String {
    format!(
        "[reporting]\nproject = '{PROJECT}'\n[cache]\nremote = 'goodflows'\n{extra}\n\
         [environments.ro.cache]\nwrite = false\n\
         [[jobs]]\nname = 'deps'\ncommand = 'test -d deps || {{ mkdir -p deps && cat mix.lock > deps/built; }}'\n{caches}"
    )
}

const ONE: &str = "[[jobs.caches]]\nname = 'deps'\npaths = ['deps']\nkey.files = ['mix.lock']\n";

/// A checkout on `branch` whose lockfile holds `lock`.
fn checkout(config: &str, branch: &str, lock: &str) -> Project {
    let p = Project::new(config);
    git(&p.dir, &["init", "-q", "-b", branch]);
    git(
        &p.dir,
        &["remote", "add", "origin", "https://example.com/org/app"],
    );
    std::fs::write(p.dir.join("mix.lock"), lock).unwrap();
    p
}

fn of_type(output: &Output, kind: &str) -> Vec<Value> {
    events(output)
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}

fn restored(output: &Output) -> (String, Value) {
    let event = of_type(output, "cache_restored")
        .pop()
        .expect("cache_restored");
    (
        event["result"].as_str().unwrap().to_owned(),
        event["tier"].clone(),
    )
}

fn reasons(output: &Output) -> Vec<String> {
    of_type(output, "cache_save_skipped")
        .iter()
        .map(|e| e["reason"].as_str().unwrap().to_owned())
        .collect()
}

/// Save one entry on `branch` from a machine of its own.
fn seed(fake: &Fake, branch: &str) {
    let p = checkout(&config("", ONE), branch, "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
}

#[test]
fn a_saved_entry_restores_on_another_machine_without_authorization_on_presigned_urls() {
    let fake = Fake::start();
    seed(&fake, "main");
    assert_eq!(fake.scopes(), ["main"]);
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "feat-x", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("exact".into(), json!("remote")));
    assert_eq!(p.read("deps/built"), "v1");
    // The remote hit is kept in the local store.
    assert_eq!(machine.entries().len(), 1);
    // Lookups send the branch scope; the server fell back to main.
    // The seed run made three lookups: exact, fallback and the save's check.
    let lookups = fake.lookups();
    assert_eq!(lookups.len(), 4);
    assert_eq!(
        lookups[3].1.get("scope").map(String::as_str),
        Some("feat-x")
    );
    for request in fake.requests() {
        let presigned = request.path.starts_with("/objects/");
        assert_eq!(
            request.headers.contains_key("authorization"),
            !presigned,
            "{} {}",
            request.method,
            request.path
        );
        if presigned {
            assert!(!format!("{:?}", request.headers).contains(TOKEN));
        }
    }
    assert!(fake.requests().iter().any(|r| r.method == "PUT"));
}

#[test]
fn a_tampered_download_is_discarded() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.tamper = true);
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json", "--env", "ro"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    let warning = &of_type(&output, "cache_warning")[0]["message"];
    assert!(warning.as_str().unwrap().contains("sha256"), "{warning}");
    assert!(machine.entries().is_empty());
    assert!(!p.dir.join("deps/x").exists());
}

/// Parts of 16 bytes, so a test archive downloads in ranges.
const SMALL_PARTS: (&str, &str) = ("GOODFLOWS_TEST_CACHE_PART_BYTES", "16");

/// The size of the one stored archive.
fn object_size(fake: &Fake) -> usize {
    let state = fake.state.lock().unwrap();
    assert_eq!(state.objects.len(), 1);
    state.objects.values().next().unwrap().len()
}

/// The `Range` header of each object download after the first `skip`
/// requests.
fn object_gets(fake: &Fake, skip: usize) -> Vec<Option<String>> {
    fake.requests()[skip..]
        .iter()
        .filter(|r| r.method == "GET" && r.path.starts_with("/objects/"))
        .inspect(|r| assert!(!r.headers.contains_key("authorization")))
        .map(|r| r.headers.get("range").cloned())
        .collect()
}

#[test]
fn a_large_archive_downloads_in_concurrent_ranges_and_retries_a_failed_part() {
    let fake = Fake::start();
    seed(&fake, "main");
    let size = object_size(&fake);
    assert!(
        size >= 4 * 16,
        "the archive must span the ranged minimum: {size}"
    );
    fake.set(|s| s.flaky_ranges = 2);
    let skip = fake.requests().len();
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "feat-x", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS]);
    success(&output);
    assert_eq!(restored(&output), ("exact".into(), json!("remote")));
    assert_eq!(p.read("deps/built"), "v1");
    let gets = object_gets(&fake, skip);
    let parts = size.div_ceil(16);
    // Every part once, plus the two that failed and were fetched again.
    assert_eq!(gets.len(), parts + 2, "{gets:?}");
    let mut ranges: Vec<(usize, usize)> = gets
        .iter()
        .map(|r| {
            let (s, e) = r.as_deref().unwrap()["bytes=".len()..]
                .split_once('-')
                .unwrap();
            (s.parse().unwrap(), e.parse().unwrap())
        })
        .collect();
    ranges.sort();
    ranges.dedup();
    let mut next = 0;
    for (start, end) in ranges {
        assert_eq!(start, next);
        assert_eq!(end, (start + 15).min(size - 1));
        next = end + 1;
    }
    assert_eq!(next, size);
}

#[test]
fn a_store_without_range_support_serves_the_whole_archive_in_one_request() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.ignore_range = true);
    let skip = fake.requests().len();
    let p = checkout(&config("", ONE), "feat-x", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS]);
    success(&output);
    assert_eq!(restored(&output), ("exact".into(), json!("remote")));
    assert_eq!(p.read("deps/built"), "v1");
    assert_eq!(object_gets(&fake, skip).len(), 1);
}

#[test]
fn a_tampered_ranged_download_is_discarded() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.tamper = true);
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    let output = machine.run(
        &p,
        &fake.mock.url,
        &["--json", "--env", "ro"],
        &[SMALL_PARTS],
    );
    success(&output);
    assert!(object_gets(&fake, 0).iter().any(Option::is_some));
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    let warning = &of_type(&output, "cache_warning")[0]["message"];
    assert!(warning.as_str().unwrap().contains("sha256"), "{warning}");
    assert!(machine.entries().is_empty());
}

#[test]
fn an_upload_slower_than_the_download_read_timeout_is_saved() {
    let fake = Fake::start();
    fake.set(|s| s.slow_put = true);
    let p = checkout(&config("", ONE), "main", "v1");
    let output = Machine::new().run(
        &p,
        &fake.mock.url,
        &["--json"],
        &[("GOODFLOWS_TEST_CACHE_READ_TIMEOUT_SECS", "1")],
    );
    success(&output);
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    assert_eq!(fake.scopes(), ["main"]);
}

#[test]
fn reserve_conflict_counts_as_saved() {
    let fake = Fake::start();
    fake.set(|s| s.reserve = Some(409));
    let p = checkout(&config("", ONE), "main", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local", "remote"])
    );
    assert!(!fake.requests().iter().any(|r| r.method == "PUT"));
}

#[test]
fn upload_refusals_map_to_skip_reasons_and_keep_the_local_entry() {
    for (code, reason) in [(413, "too_large"), (507, "quota")] {
        let fake = Fake::start();
        fake.set(|s| s.reserve = Some(code));
        let machine = Machine::new();
        let p = checkout(&config("", ONE), "main", "v1");
        let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
        success(&output);
        assert_eq!(reasons(&output), [reason]);
        assert_eq!(
            of_type(&output, "cache_saved")[0]["tiers"],
            json!(["local"])
        );
        assert_eq!(machine.entries().len(), 1);
    }
}

#[test]
fn a_read_only_key_warns_once_and_attempts_no_further_saves() {
    let fake = Fake::start();
    fake.set(|s| s.reserve = Some(403));
    let caches = "[[jobs.caches]]\nname = 'a'\npaths = ['deps']\n[[jobs.caches]]\nname = 'b'\npaths = ['mix.lock']\n";
    let machine = Machine::new();
    let p = checkout(&config("", caches), "main", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(
        of_type(&output, "cache_warning").len(),
        1,
        "{}",
        stdout(&output)
    );
    assert_eq!(reasons(&output), ["read_only", "read_only"]);
    let reserves = fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(reserves, 1);
    assert_eq!(machine.entries().len(), 2);
}

#[test]
fn a_rejected_commit_warns_and_a_failed_upload_keeps_the_local_entry() {
    for set in [
        (|s: &mut State| s.commit = Some(422)) as fn(&mut State),
        |s: &mut State| s.put = Some(500),
    ] {
        let fake = Fake::start();
        fake.set(set);
        let machine = Machine::new();
        let p = checkout(&config("", ONE), "main", "v1");
        let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
        success(&output);
        assert_eq!(
            of_type(&output, "cache_warning").len(),
            1,
            "{}",
            stdout(&output)
        );
        assert_eq!(
            of_type(&output, "cache_saved")[0]["tiers"],
            json!(["local"])
        );
        assert_eq!(machine.entries().len(), 1);
        assert!(fake.scopes().is_empty());
    }
}

#[test]
fn an_unavailable_server_turns_the_remote_store_off_once() {
    let fake = Fake::start();
    fake.set(|s| s.lookup = Some(503));
    let caches = "[[jobs.caches]]\nname = 'a'\npaths = ['deps']\n[[jobs.caches]]\nname = 'b'\npaths = ['mix.lock']\n";
    let p = checkout(&config("", caches), "main", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    let unavailable = of_type(&output, "cache_remote_unavailable");
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0]["reason"], "server_unavailable");
    assert_eq!(fake.requests().len(), 1);
    // Saves still go to the local store.
    assert_eq!(of_type(&output, "cache_saved").len(), 2);

    // A connection failure behaves the same.
    let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let p = checkout(&config("", ONE), "main", "v1");
    let output = Machine::new().run(&p, &url, &["--json"], &[]);
    success(&output);
    assert_eq!(of_type(&output, "cache_remote_unavailable").len(), 1);
}

#[test]
fn missing_credentials_use_the_local_store() {
    let p = checkout(&config("", ONE), "main", "v1");
    let output = p
        .command()
        .env("XDG_CACHE_HOME", p.dir.join(".xdg"))
        .env_remove("GOODFLOWS_CACHE_KEY")
        .args(["--json"])
        .output()
        .unwrap();
    success(&output);
    let unavailable = of_type(&output, "cache_remote_unavailable");
    assert_eq!(unavailable.len(), 1);
    assert_eq!(unavailable[0]["reason"], "credentials_unavailable");
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local"])
    );
}

#[test]
fn a_detached_checkout_without_a_scope_saves_locally_only() {
    let fake = Fake::start();
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    git(&p.dir, &["add", "goodflows.toml", "mix.lock"]);
    git(&p.dir, &["commit", "-qm", "init"]);
    git(&p.dir, &["checkout", "-q", "--detach"]);
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert!(fake.lookups().iter().all(|(_, q)| !q.contains_key("scope")));
    assert_eq!(reasons(&output), ["no_scope"]);
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local"])
    );
    assert!(fake.scopes().is_empty());
}

const PR_SCOPE: &str = "scope = 'env.GITHUB_HEAD_REF ?? env.GITHUB_REF_NAME'";

#[test]
fn the_scope_comes_from_github_head_ref() {
    let fake = Fake::start();
    let p = checkout(&config(PR_SCOPE, ONE), "main", "v1");
    let output = Machine::new().run(
        &p,
        &fake.mock.url,
        &["--json"],
        &[
            ("GITHUB_HEAD_REF", "feat/x y"),
            ("GITHUB_REF_NAME", "42/merge"),
        ],
    );
    success(&output);
    assert!(fake
        .lookups()
        .iter()
        .all(|(_, q)| q.get("scope").map(String::as_str) == Some("feat/x y")));
    assert_eq!(fake.scopes(), ["feat/x y"]);
}

#[test]
fn an_empty_github_head_ref_falls_back_to_the_branch() {
    let fake = Fake::start();
    let p = checkout(&config(PR_SCOPE, ONE), "feat-b", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[("GITHUB_HEAD_REF", "")]);
    success(&output);
    assert!(of_type(&output, "cache_warning").is_empty());
    assert_eq!(fake.scopes(), ["feat-b"]);
}

#[test]
fn a_scope_over_256_bytes_falls_back_to_the_branch_with_a_warning() {
    let fake = Fake::start();
    let p = checkout(&config("scope = 'env.LONG'", ONE), "feat-c", "v1");
    let long = "x".repeat(257);
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[("LONG", &long)]);
    success(&output);
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("256 bytes"));
    assert_eq!(fake.scopes(), ["feat-c"]);
}

#[test]
fn a_forbidden_scoped_lookup_reads_only_the_default_branch_for_the_rest_of_the_run() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.forbid_scoped = true);
    let caches = "[[jobs.caches]]\nname = 'deps'\npaths = ['deps']\nkey.files = ['mix.lock']\n[[jobs.caches]]\nname = 'other'\npaths = ['other']\n";
    let p = checkout(&config("", caches), "feature/login", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json", "--env", "ro"], &[]);
    success(&output);
    let events = of_type(&output, "cache_restored");
    assert_eq!(events[0]["result"], "exact");
    assert_eq!(events[0]["tier"], "remote");
    assert_eq!(
        of_type(&output, "cache_warning").len(),
        1,
        "{}",
        stdout(&output)
    );
    let lookups = fake.lookups();
    // The seed run's lookups came first; this run sent one scoped lookup.
    let run: Vec<_> = lookups
        .iter()
        .skip_while(|(_, q)| q.get("scope").map(String::as_str) == Some("main"))
        .collect();
    assert_eq!(
        run[0].1.get("scope").map(String::as_str),
        Some("feature/login")
    );
    assert!(
        run[1..].iter().all(|(_, q)| !q.contains_key("scope")),
        "{run:?}"
    );
    assert!(run.len() > 2);
}

/// No staging file left under `dir`.
fn temporaries(dir: &Path) -> Vec<PathBuf> {
    files_under(dir)
        .into_iter()
        .filter(|f| {
            f.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with(".tmp"))
        })
        .collect()
}

#[test]
fn a_stalled_download_is_abandoned_at_the_caches_timeout_and_the_job_runs_as_a_miss() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.stall = true);
    let machine = Machine::new();
    // The job passes only when the restore left no declared path behind.
    let config = format!(
        "[reporting]\nproject = '{PROJECT}'\n[cache]\nremote = 'goodflows'\n\
         [[jobs]]\nname = 'deps'\ncommand = 'test ! -e deps && touch ran'\n{ONE}timeout_secs = 1\n"
    );
    let p = checkout(&config, "main", "v1");
    let started = std::time::Instant::now();
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    // Abandoned at the bound, not at the 30 s stall or the read timeout.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(fake
        .requests()
        .iter()
        .any(|r| r.method == "GET" && r.path.starts_with("/objects/")));
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(
        warnings[0]["message"]
            .as_str()
            .unwrap()
            .contains("timeout_secs"),
        "{}",
        warnings[0]
    );
    assert!(p.dir.join("ran").exists());
    assert!(!p.dir.join("deps").exists());
    assert!(temporaries(&machine.cache_home).is_empty());
    assert!(temporaries(&p.dir.join(".goodflows")).is_empty());
    let summary = of_type(&output, "summary").pop().unwrap();
    assert_eq!(summary["outcome"], "passed", "{summary}");
}

#[test]
fn a_stalled_lookup_is_abandoned_at_the_caches_timeout_and_keeps_the_remote_store() {
    let fake = Fake::start();
    seed(&fake, "main");
    fake.set(|s| s.stall_lookup = true);
    let machine = Machine::new();
    let config = format!(
        "[reporting]\nproject = '{PROJECT}'\n[cache]\nremote = 'goodflows'\n\
         [[jobs]]\nname = 'deps'\ncommand = 'touch ran'\n{ONE}timeout_secs = 1\n"
    );
    let p = checkout(&config, "main", "v1");
    let started = std::time::Instant::now();
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    // The restore and the save each stop at their 1 s bound, not at the
    // API client's 30 s request timeout.
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    let warnings = of_type(&output, "cache_warning");
    assert!(!warnings.is_empty(), "{}", stdout(&output));
    for warning in &warnings {
        assert!(
            warning["message"]
                .as_str()
                .unwrap()
                .contains("timeout_secs"),
            "{warning}"
        );
    }
    // A request the bound stopped is not an outage.
    assert!(
        of_type(&output, "cache_remote_unavailable").is_empty(),
        "{}",
        stdout(&output)
    );
    assert!(p.dir.join("ran").exists());
    let summary = of_type(&output, "summary").pop().unwrap();
    assert_eq!(summary["outcome"], "passed", "{summary}");
}

#[test]
fn jobs_never_see_the_cache_key() {
    for token_env in ["GOODFLOWS_CACHE_KEY", "MY_CACHE_KEY"] {
        let fake = Fake::start();
        let machine = Machine::new();
        // The job's command and its condition both look for the key.
        let config = format!(
            "[reporting]\nproject = '{PROJECT}'\n[cache]\nremote = 'goodflows'\ntoken_env = '{token_env}'\n\
             [[jobs]]\nname = 'deps'\ncondition = {{ expression = \"(env.{token_env} ?? 'gone') == 'gone'\" }}\ncommand = 'env > seen'\n{ONE}"
        );
        let p = checkout(&config, "main", "v1");
        // The harness always sets GOODFLOWS_CACHE_KEY; with a custom
        // token_env it is not a credential, so clear it.
        let vars = [("GOODFLOWS_CACHE_KEY", ""), (token_env, TOKEN)];
        let output = machine.run(&p, &fake.mock.url, &["--json"], &vars);
        success(&output);
        // The key still reached the cache server.
        assert_eq!(fake.scopes(), ["main"], "{}", stdout(&output));
        let seen = std::fs::read_to_string(p.dir.join("seen")).unwrap();
        assert!(!seen.contains(TOKEN), "{token_env}: {seen}");
    }
}

#[test]
fn with_the_local_store_off_caches_use_the_remote_store_only() {
    let fake = Fake::start();
    let local_off = "local = false";
    let machine = Machine::new();
    let p = checkout(&config(local_off, ONE), "main", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["remote"]),
        "{}",
        stdout(&output)
    );
    assert_eq!(fake.scopes(), ["main"]);
    assert!(files_under(&machine.cache_home).is_empty());
    assert!(temporaries(&p.dir.join(".goodflows")).is_empty());

    // A fresh checkout restores the exact entry from the remote store.
    let machine = Machine::new();
    let p = checkout(&config(local_off, ONE), "main", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("exact".into(), json!("remote")));
    assert_eq!(p.read("deps/built"), "v1");
    assert_eq!(reasons(&output), ["hit"]);
    assert!(of_type(&output, "cache_warning").is_empty());
    assert!(files_under(&machine.cache_home).is_empty());
    assert!(!machine.cache_home.exists());
    assert!(temporaries(&p.dir.join(".goodflows")).is_empty());
}

#[test]
fn with_the_local_store_off_a_save_the_remote_store_would_refuse_builds_no_archive() {
    // No scope: a detached checkout.
    let fake = Fake::start();
    let machine = Machine::new();
    let p = checkout(&config("local = false", ONE), "main", "v1");
    git(&p.dir, &["add", "goodflows.toml", "mix.lock"]);
    git(&p.dir, &["commit", "-qm", "init"]);
    git(&p.dir, &["checkout", "-q", "--detach"]);
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(reasons(&output), ["no_scope"]);
    assert!(of_type(&output, "cache_saved").is_empty());
    assert!(of_type(&output, "cache_warning").is_empty());
    assert!(!p.dir.join(".goodflows/cache-save").exists());
    assert!(!fake.requests().iter().any(|r| r.method == "POST"));

    // A read-only key: the first save learns it, the second builds nothing.
    let fake = Fake::start();
    fake.set(|s| s.reserve = Some(403));
    let caches = "[[jobs.caches]]\nname = 'a'\npaths = ['deps']\n[[jobs.caches]]\nname = 'b'\npaths = ['mix.lock']\n";
    let p = checkout(&config("local = false", caches), "main", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(reasons(&output), ["read_only", "read_only"]);
    assert_eq!(
        of_type(&output, "cache_warning").len(),
        1,
        "{}",
        stdout(&output)
    );
    let reserves = fake
        .requests()
        .iter()
        .filter(|r| r.method == "POST")
        .count();
    assert_eq!(reserves, 1);
    assert!(temporaries(&p.dir.join(".goodflows")).is_empty());
}

#[test]
fn a_signal_during_a_stalled_restore_ends_the_run_promptly_as_cancelled() {
    for (signal, code) in [(libc::SIGINT, 130), (libc::SIGTERM, 143)] {
        let fake = Fake::start();
        seed(&fake, "main");
        fake.set(|s| s.stall = true);
        let machine = Machine::new();
        // The default 300 s bound: only the signal can end the restore.
        let p = checkout(&config("", ONE), "main", "v1");
        let downloads = || {
            fake.requests()
                .iter()
                .filter(|r| r.method == "GET" && r.path.starts_with("/objects/"))
                .count()
        };
        let before = downloads();
        let child = machine
            .command(&p, &fake.mock.url, &["--json"], &[])
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        let waited = std::time::Instant::now();
        while downloads() == before {
            assert!(waited.elapsed() < std::time::Duration::from_secs(20));
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
        let signalled = std::time::Instant::now();
        // SAFETY: signalling our own child process.
        unsafe {
            libc::kill(child.id() as i32, signal);
        }
        let output = child.wait_with_output().unwrap();
        assert!(
            signalled.elapsed() < std::time::Duration::from_secs(10),
            "{:?}",
            signalled.elapsed()
        );
        assert_eq!(output.status.code(), Some(code), "{}", stdout(&output));
        assert_eq!(restored(&output), ("miss".into(), Value::Null));
        let warning = &of_type(&output, "cache_warning")[0]["message"];
        assert!(warning.as_str().unwrap().contains("cancelled"), "{warning}");
        let summary = of_type(&output, "summary").pop().unwrap();
        assert_eq!(summary["outcome"], "cancelled", "{summary}");
        let job = of_type(&output, "job_finished").pop().unwrap();
        assert_eq!(job["result"]["status"], "cancelled", "{job}");
        assert!(reasons(&output).is_empty() || reasons(&output) == ["not_passed"]);
        assert!(!p.dir.join("deps").exists());
        assert!(temporaries(&machine.cache_home).is_empty());
        assert!(temporaries(&p.dir.join(".goodflows")).is_empty());
    }
}

#[test]
fn an_upload_abandoned_at_the_bound_keeps_and_reports_the_local_entry() {
    let fake = Fake::start();
    fake.set(|s| s.stall_put = true);
    let machine = Machine::new();
    let p = checkout(
        &config("", &format!("{ONE}timeout_secs = 1\n")),
        "main",
        "v1",
    );
    let started = std::time::Instant::now();
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(fake.requests().iter().any(|r| r.method == "PUT"));
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local"]),
        "{}",
        stdout(&output)
    );
    let skipped = of_type(&output, "cache_save_skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        (&skipped[0]["reason"], &skipped[0]["tier"]),
        (&json!("error"), &json!("remote"))
    );
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("timeout_secs"));
    assert_eq!(machine.entries().len(), 1);
    assert!(fake.scopes().is_empty());
}

#[test]
fn a_signal_during_an_upload_keeps_the_local_entry_and_names_the_cancellation() {
    let fake = Fake::start();
    fake.set(|s| s.stall_put = true);
    let machine = Machine::new();
    // The default 300 s bound: only the signal can end the upload.
    let p = checkout(&config("", ONE), "main", "v1");
    let child = machine
        .command(&p, &fake.mock.url, &["--json"], &[])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    while !fake.requests().iter().any(|r| r.method == "PUT") {
        assert!(waited.elapsed() < std::time::Duration::from_secs(20));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let signalled = std::time::Instant::now();
    // SAFETY: signalling our own child process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert!(signalled.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(130), "{}", stdout(&output));
    assert_eq!(
        of_type(&output, "cache_saved")[0]["tiers"],
        json!(["local"]),
        "{}",
        stdout(&output)
    );
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(message.contains("the run was cancelled"), "{message}");
    assert!(!message.contains("timeout_secs"), "{message}");
    assert_eq!(machine.entries().len(), 1);
}

/// The bodies of the reserve requests, in order.
fn reserves(fake: &Fake) -> Vec<Value> {
    fake.requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path.contains("/caches/entries/"))
        .map(Request::json)
        .collect()
}

fn commits(fake: &Fake) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == "POST" && r.path.contains("/caches/uploads/"))
        .count()
}

/// Single `PUT`s, not part uploads.
fn single_puts(fake: &Fake) -> usize {
    fake.requests()
        .iter()
        .filter(|r| r.method == "PUT" && !r.path.contains("/parts/"))
        .count()
}

/// Save one entry on `main` in 16-byte parts from a machine of its own.
fn save_in_parts(fake: &Fake, vars: &[(&str, &str)]) -> (Machine, Output) {
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    let mut all = vec![SMALL_PARTS];
    all.extend_from_slice(vars);
    let output = machine.run(&p, &fake.mock.url, &["--json"], &all);
    success(&output);
    (machine, output)
}

fn saved_tiers(output: &Output) -> Value {
    of_type(output, "cache_saved")[0]["tiers"].clone()
}

#[test]
fn a_small_archive_is_reserved_without_parts() {
    let fake = Fake::start();
    seed(&fake, "main");
    let reserves = reserves(&fake);
    assert_eq!(reserves.len(), 1);
    assert!(reserves[0].get("parts").is_none(), "{}", reserves[0]);
    assert_eq!(single_puts(&fake), 1);
    assert!(fake.part_puts().is_empty());
}

#[test]
fn a_large_archive_uploads_in_parallel_parts_that_restore_on_another_machine() {
    let fake = Fake::start();
    let (_, output) = save_in_parts(&fake, &[]);
    assert_eq!(
        saved_tiers(&output),
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    let size = object_size(&fake);
    let count = size.div_ceil(16);
    assert!(count >= 4, "{size}");
    let reserve = &reserves(&fake)[0];
    assert_eq!(reserve["parts"]["size"], 16);
    assert_eq!(reserve["parts"]["md5"].as_array().unwrap().len(), count);
    assert_eq!(single_puts(&fake), 0);
    let puts = fake.part_puts();
    assert_eq!(puts.len(), count);
    let mut numbers: Vec<usize> = puts.iter().map(|(_, n, _)| *n).collect();
    numbers.sort();
    assert_eq!(numbers, (1..=count).collect::<Vec<_>>());
    // The part bodies, in order, are the archive byte for byte.
    let mut sorted: Vec<_> = puts.iter().collect();
    sorted.sort_by_key(|(_, n, _)| *n);
    let joined: Vec<u8> = sorted.iter().flat_map(|(_, _, r)| r.body.clone()).collect();
    let stored = fake
        .state
        .lock()
        .unwrap()
        .objects
        .values()
        .next()
        .unwrap()
        .clone();
    assert_eq!(joined, stored);
    for (_, _, request) in &puts {
        assert!(!request.headers.contains_key("authorization"));
        assert!(!request.headers.contains_key("transfer-encoding"));
        assert!(!format!("{:?}", request.headers).contains(TOKEN));
    }
    let most = fake.state.lock().unwrap().max_parts_in_flight;
    assert!((2..=8).contains(&most), "{most}");
    assert_eq!(commits(&fake), 1);

    let machine = Machine::new();
    let p = checkout(&config("", ONE), "feat-x", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("exact".into(), json!("remote")));
    assert_eq!(p.read("deps/built"), "v1");
}

#[test]
fn a_reservation_with_missing_parts_uploads_nothing_and_warns() {
    let fake = Fake::start();
    fake.set(|s| s.short_parts = true);
    let (machine, output) = save_in_parts(&fake, &[]);
    assert!(fake.requests().iter().all(|r| r.method != "PUT"));
    assert_eq!(commits(&fake), 0);
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("invalid reservation"));
    assert_eq!(saved_tiers(&output), json!(["local"]));
    assert_eq!(machine.entries().len(), 1);
}

#[test]
fn a_server_without_multipart_storage_gets_one_put() {
    let fake = Fake::start();
    fake.set(|s| s.single_url = true);
    let (_, output) = save_in_parts(&fake, &[]);
    assert_eq!(
        saved_tiers(&output),
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    let reserves = reserves(&fake);
    assert_eq!(reserves.len(), 1);
    assert!(reserves[0]["parts"].is_object());
    assert_eq!(single_puts(&fake), 1);
    assert!(fake.part_puts().is_empty());
    assert_eq!(fake.scopes(), ["main"]);
}

#[test]
fn an_archive_over_10000_parts_is_reserved_without_parts() {
    let fake = Fake::start();
    let config = format!(
        "[reporting]\nproject = '{PROJECT}'\n[cache]\nremote = 'goodflows'\n\
         [[jobs]]\nname = 'deps'\ncommand = 'mkdir -p deps && head -c 200000 /dev/urandom > deps/big'\n{ONE}"
    );
    let p = checkout(&config, "main", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS]);
    success(&output);
    assert_eq!(
        saved_tiers(&output),
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    assert!(object_size(&fake).div_ceil(16) > 10_000);
    let reserves = reserves(&fake);
    assert_eq!(reserves.len(), 1);
    assert!(reserves[0].get("parts").is_none());
    assert_eq!(single_puts(&fake), 1);
}

#[test]
fn a_part_that_fails_once_is_sent_again() {
    let fake = Fake::start();
    fake.set(|s| s.flaky_parts = 1);
    let (_, output) = save_in_parts(&fake, &[]);
    assert_eq!(
        saved_tiers(&output),
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    assert!(of_type(&output, "cache_warning").is_empty());
    let count = object_size(&fake).div_ceil(16);
    assert_eq!(fake.part_puts().len(), count + 1);
    assert_eq!(fake.scopes(), ["main"]);
}

#[test]
fn a_part_rejected_by_its_md5_fails_the_save_without_a_commit() {
    let fake = Fake::start();
    fake.set(|s| s.corrupt_part = Some(2));
    let (machine, output) = save_in_parts(&fake, &[]);
    let tries = fake.part_puts().iter().filter(|(_, n, _)| *n == 2).count();
    assert_eq!(tries, 3);
    assert_eq!(commits(&fake), 0);
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("HTTP 400"));
    assert_eq!(saved_tiers(&output), json!(["local"]));
    assert_eq!(machine.entries().len(), 1);
    assert!(fake.scopes().is_empty());
}

const REFUSED_MULTIPART: &str =
    "the cache server refused a multipart upload; uploading archives in one request";

#[test]
fn an_older_server_that_refuses_parts_gets_a_second_reserve_and_one_put() {
    let fake = Fake::start();
    fake.set(|s| s.no_multipart = true);
    let (_, output) = save_in_parts(&fake, &[]);
    assert_eq!(
        saved_tiers(&output),
        json!(["local", "remote"]),
        "{}",
        stdout(&output)
    );
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert_eq!(warnings[0]["message"], REFUSED_MULTIPART);
    let sent = reserves(&fake);
    assert_eq!(sent.len(), 2);
    assert!(sent[0]["parts"].is_object());
    assert!(sent[1].get("parts").is_none(), "{}", sent[1]);
    assert_eq!(single_puts(&fake), 1);
    assert_eq!(commits(&fake), 1);

    // Two saves in one run fall back twice but warn once.
    let fake = Fake::start();
    fake.set(|s| s.no_multipart = true);
    let caches = "[[jobs.caches]]\nname = 'a'\npaths = ['deps']\n[[jobs.caches]]\nname = 'b'\npaths = ['mix.lock']\n";
    let p = checkout(&config("", caches), "main", "v1");
    let output = Machine::new().run(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS]);
    success(&output);
    let saved = of_type(&output, "cache_saved");
    assert_eq!(saved.len(), 2, "{}", stdout(&output));
    assert!(saved
        .iter()
        .all(|e| e["tiers"] == json!(["local", "remote"])));
    let all = reserves(&fake);
    assert_eq!(all.len(), 4);
    assert_eq!(all.iter().filter(|r| r["parts"].is_object()).count(), 2);
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert_eq!(warnings[0]["message"], REFUSED_MULTIPART);
    assert_eq!(fake.scopes(), ["main", "main"]);
}

#[test]
fn another_422_on_a_multipart_reserve_still_fails_the_save() {
    let fake = Fake::start();
    fake.set(|s| s.reserve = Some(422));
    let (_, output) = save_in_parts(&fake, &[]);
    assert_eq!(reserves(&fake).len(), 1);
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert_eq!(
        warnings[0]["message"],
        "the cache reservation failed (HTTP 422)"
    );
    assert_eq!(saved_tiers(&output), json!(["local"]));
}

#[test]
fn a_stalled_multipart_upload_is_abandoned_at_the_bound_and_keeps_the_local_entry() {
    let fake = Fake::start();
    fake.set(|s| s.stall_put = true);
    let machine = Machine::new();
    let p = checkout(
        &config("", &format!("{ONE}timeout_secs = 1\n")),
        "main",
        "v1",
    );
    let started = std::time::Instant::now();
    let output = machine.run(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS]);
    success(&output);
    assert!(started.elapsed() < std::time::Duration::from_secs(20));
    assert!(!fake.part_puts().is_empty());
    assert_eq!(single_puts(&fake), 0);
    assert_eq!(commits(&fake), 0);
    assert_eq!(
        saved_tiers(&output),
        json!(["local"]),
        "{}",
        stdout(&output)
    );
    let skipped = of_type(&output, "cache_save_skipped");
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        (&skipped[0]["reason"], &skipped[0]["tier"]),
        (&json!("error"), &json!("remote"))
    );
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("timeout_secs"));
    assert_eq!(machine.entries().len(), 1);
}

#[test]
fn a_signal_during_a_multipart_upload_names_the_cancellation() {
    let fake = Fake::start();
    fake.set(|s| s.stall_put = true);
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    let child = machine
        .command(&p, &fake.mock.url, &["--json"], &[SMALL_PARTS])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let waited = std::time::Instant::now();
    while fake.part_puts().is_empty() {
        assert!(waited.elapsed() < std::time::Duration::from_secs(20));
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    std::thread::sleep(std::time::Duration::from_millis(200));
    let signalled = std::time::Instant::now();
    // SAFETY: signalling our own child process.
    unsafe {
        libc::kill(child.id() as i32, libc::SIGINT);
    }
    let output = child.wait_with_output().unwrap();
    assert!(signalled.elapsed() < std::time::Duration::from_secs(10));
    assert_eq!(output.status.code(), Some(130), "{}", stdout(&output));
    assert_eq!(
        saved_tiers(&output),
        json!(["local"]),
        "{}",
        stdout(&output)
    );
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    let message = warnings[0]["message"].as_str().unwrap();
    assert!(message.contains("the run was cancelled"), "{message}");
    assert_eq!(commits(&fake), 0);
    assert_eq!(machine.entries().len(), 1);
}

#[test]
fn a_multipart_entry_changed_after_its_commit_is_discarded_on_restore() {
    let fake = Fake::start();
    save_in_parts(&fake, &[]);
    assert!(!fake.part_puts().is_empty());
    fake.set(|s| {
        for bytes in s.objects.values_mut() {
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
        }
    });
    let machine = Machine::new();
    let p = checkout(&config("", ONE), "main", "v1");
    let output = machine.run(&p, &fake.mock.url, &["--json", "--env", "ro"], &[]);
    success(&output);
    assert_eq!(restored(&output), ("miss".into(), Value::Null));
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"].as_str().unwrap().contains("sha256"));
    assert!(machine.entries().is_empty());
    // The job ran as a miss and built the path itself.
    assert_eq!(p.read("deps/built"), "v1");
}

#[test]
fn parts_start_at_four_whole_parts() {
    // A first save measures the archive; the same checkout archives the
    // same bytes again, so later saves can size the parts around it.
    let p = checkout(&config("", ONE), "main", "v1");
    let fake = Fake::start();
    success(&Machine::new().run(&p, &fake.mock.url, &["--json"], &[]));
    let size = object_size(&fake) as u64;
    assert!(size >= 8, "{size}");
    for (part, expected) in [(size / 4, true), (size / 4 + 1, false)] {
        // Without the stamp the path is not current, so the job saves again.
        std::fs::remove_dir_all(p.dir.join(".goodflows/cache-stamps")).unwrap();
        let fake = Fake::start();
        let part_env = part.to_string();
        let output = Machine::new().run(
            &p,
            &fake.mock.url,
            &["--json"],
            &[("GOODFLOWS_TEST_CACHE_PART_BYTES", &part_env)],
        );
        success(&output);
        assert_eq!(object_size(&fake) as u64, size, "the archive changed");
        assert_eq!(saved_tiers(&output), json!(["local", "remote"]));
        let reserve = &reserves(&fake)[0];
        assert_eq!(
            reserve["parts"].is_object(),
            expected,
            "part size {part} of {size}: {reserve}"
        );
        if expected {
            assert_eq!(reserve["parts"]["size"], part);
            let md5s = reserve["parts"]["md5"].as_array().unwrap().len() as u64;
            assert_eq!(md5s, size.div_ceil(part));
            assert_eq!(fake.part_puts().len() as u64, md5s);
        } else {
            assert_eq!(single_puts(&fake), 1);
        }
    }
}
