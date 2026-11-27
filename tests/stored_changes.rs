mod common;
use common::mock::{Mock, Reply, Request};
use common::*;
use serde_json::{json, Map, Value};
use std::collections::BTreeMap;
use std::path::Path;
use std::process::Output;
use std::sync::{Arc, Mutex};

const CONFIG: &str =
    "[reporting]\nproject = 'goodcases'\n\n[[jobs]]\nname = 'test'\ncommand = 'true'\n";
const PREFIX: &str = "/api/v1/projects/goodcases";

fn git(p: &Project, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=T", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(&p.dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "git {args:?}: {}", stderr(&output));
    String::from_utf8(output.stdout).unwrap().trim().to_owned()
}

/// A checkout on branch `plan` with one commit.
fn checkout() -> Project {
    let p = Project::new(CONFIG);
    git(&p, &["init", "-q", "-b", "plan"]);
    std::fs::write(p.dir.join(".gitignore"), ".goodflows/\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-q", "-m", "base"]);
    p
}

fn change_md(id: &str, status: &str) -> String {
    format!(
        "---\nformat: goodflows-change/1\nid: {id}\nstatus: {status}\nsize: small\ndoor: two_way\ncapabilities: [auth]\ndepends_on: []\ngoals: [{{id: G1, text: Sign in}}]\nnon_goals: []\nanti_goals: []\ndecisions: []\nleft_to_builder: [The form]\n---\n\n## Why\n\nPeople sign in.\n"
    )
}

const DELTA: &str =
    "## ADDED Requirements\n\n### Requirement: Sign in\nRefs: G1\nPeople SHALL sign in.\n";

fn qa(id: &str) -> String {
    format!(
        "---\nformat: qa-scenarios/2\nsource: intent\nchange: {id}\nspecs: uncommitted\nscenarios:\n  - id: 1\n    name: Sign in\n    priority: must\n    covers:\n      - {{capability: auth, requirement: Sign in, implied: true}}\n---\n\n# QA\n\n## Scenarios\n\n### 1. Sign in (Must)\n\nSign in.\n\n- Expect: signed in.\n"
    )
}

/// The files of a valid change, by path in its directory.
fn change_files(id: &str, status: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("change.md".to_owned(), change_md(id, status)),
        ("specs/auth/spec.md".to_owned(), DELTA.to_owned()),
        ("qa-scenarios.md".to_owned(), qa(id)),
    ])
}

fn write_files(dir: &Path, files: &BTreeMap<String, String>) {
    for (path, text) in files {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }
}

fn write_change(p: &Project, id: &str) -> std::path::PathBuf {
    let dir = p.dir.join("goodflows/changes").join(id);
    write_files(&dir, &change_files(id, "proposed"));
    dir
}

/// Every file under `dir`, by relative path.
fn read_dir(dir: &Path) -> BTreeMap<String, String> {
    files_under(dir)
        .into_iter()
        .map(|path| {
            let relative = path
                .strip_prefix(dir)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            (relative, std::fs::read_to_string(&path).unwrap())
        })
        .collect()
}

fn run(p: &Project, mock: &Mock, args: &[&str]) -> Output {
    p.command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", "sk_test")
        .args(args)
        .output()
        .unwrap()
}

fn event(output: &Output) -> Value {
    let text = stdout(output);
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines.len(),
        1,
        "one JSON line: {text}\nstderr: {}",
        stderr(output)
    );
    serde_json::from_str(lines[0]).unwrap()
}

fn code(output: &Output) -> i32 {
    output.status.code().unwrap()
}

fn reply(status: u16, body: Value) -> Reply {
    Reply::Status(status, vec![], body.to_string())
}

fn posts(mock: &Mock) -> Vec<Request> {
    mock.requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect()
}

#[derive(Clone)]
struct Version {
    number: u64,
    status: String,
    source: Value,
    /// Path to text, as pushed.
    files: Value,
}

#[derive(Default)]
struct State {
    changes: BTreeMap<String, Vec<Version>>,
    links: BTreeMap<String, Vec<u64>>,
    /// Ids whose list rows leave out `change_requests`, as servers
    /// deployed before the list carried links do.
    unlinked_rows: std::collections::BTreeSet<String>,
}

type Shared = Arc<Mutex<State>>;

fn pushed_at(v: &Version) -> String {
    format!("2026-11-2{}T10:00:00Z", v.number % 10)
}

/// A version as the shipped server's `StoredChangeJSON.data/2` renders it.
fn data(id: &str, v: &Version) -> Value {
    json!({"change_id": id, "version": v.number, "status": v.status, "size": "small",
           "door": "two_way", "capabilities": ["auth"], "depends_on": [],
           "pushed_at": pushed_at(v), "source": v.source, "pushed_by": {"user_id": "u1"}})
}

/// The version's files as the server sends them: `[{path, text}]` by path.
fn file_list(v: &Version) -> Value {
    json!(v
        .files
        .as_object()
        .unwrap()
        .iter()
        .map(|(path, text)| json!({"path": path, "text": text}))
        .collect::<Vec<_>>())
}

fn links(state: &State, id: &str) -> Value {
    json!(state
        .links
        .get(id)
        .into_iter()
        .flatten()
        .map(|n| json!({"number": n, "state": "open"}))
        .collect::<Vec<_>>())
}

/// A list row as the shipped server's `listed/1` renders it.
fn listed(state: &State, id: &str) -> Value {
    let latest = state.changes[id].last().unwrap();
    let mut row = json!({"change_id": id, "version": latest.number, "status": latest.status,
           "size": "small", "door": "two_way", "capabilities": ["auth"], "depends_on": [],
           "last_pushed_at": pushed_at(latest)});
    if !state.unlinked_rows.contains(id) {
        row["change_requests"] = links(state, id);
    }
    row
}

fn field<'a>(text: &'a str, key: &str) -> Option<&'a str> {
    text.lines()
        .find_map(|l| l.strip_prefix(&format!("{key}: ")))
}

fn query(path: &str, key: &str) -> Option<String> {
    let query = path.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix(&format!("{key}=")).map(str::to_owned))
}

/// One page of `rows` from the `cursor` query, `page` rows long.
fn paged(rows: Vec<Value>, path: &str, page: usize) -> Reply {
    let start: usize = query(path, "cursor").map_or(0, |c| c.parse().unwrap());
    let end = (start + page).min(rows.len());
    let next = (end < rows.len()).then(|| end.to_string());
    reply(200, json!({"data": rows[start..end], "next_cursor": next}))
}

fn not_found() -> Reply {
    reply(404, json!({"errors": {"detail": "Not Found"}}))
}

/// The stored changes API of project goodcases as goodflows_web ships it
/// (StoredChangeController, StoredChangeJSON and FallbackController), in
/// memory, with `page` rows per page.
fn store(state: Shared, page: usize) -> Mock {
    Mock::start(move |r: &Request, _| {
        let mut state = state.lock().unwrap();
        let Some(path) = r.path.strip_prefix(PREFIX) else {
            return not_found();
        };
        let bare = path.split('?').next().unwrap();
        let segments: Vec<&str> = bare.trim_start_matches('/').split('/').collect();
        match (r.method.as_str(), segments.as_slice()) {
            ("GET", ["change-requests"]) => reply(200, json!({"data": [], "next_cursor": null})),
            ("POST", ["changes"]) => {
                let body = r.json();
                let files = body["files"].clone();
                let text = files["change.md"].as_str().unwrap().to_owned();
                let id = field(&text, "id").unwrap().to_owned();
                let status = field(&text, "status").unwrap().to_owned();
                let versions = state.changes.entry(id.clone()).or_default();
                let latest = versions.last().cloned();
                if let Some(latest) = &latest {
                    if latest.files == files {
                        let mut out = data(&id, latest);
                        out["duplicate"] = json!(true);
                        out["replaced_version"] = json!(latest.number);
                        return reply(200, json!({"data": out}));
                    }
                    if latest.status == "archived" {
                        let detail = format!(
                            "{id} is archived and takes no further pushes; give a new change a new id"
                        );
                        return reply(409, json!({"errors": {"detail": detail}}));
                    }
                }
                let version = Version {
                    number: latest.as_ref().map_or(1, |l| l.number + 1),
                    status,
                    source: body["source"].clone(),
                    files,
                };
                versions.push(version.clone());
                let mut out = data(&id, &version);
                out["duplicate"] = json!(false);
                out["replaced_version"] = json!(latest.map(|l| l.number));
                reply(201, json!({"data": out}))
            }
            ("GET", ["changes"]) => {
                let wanted = query(path, "status");
                let rows: Vec<Value> = state
                    .changes
                    .keys()
                    .map(|id| listed(&state, id))
                    .filter(|s| wanted.as_ref().is_none_or(|w| s["status"] == w.as_str()))
                    .collect();
                paged(rows, path, page)
            }
            ("GET", ["changes", id]) => match state.changes.get(*id) {
                None => not_found(),
                Some(versions) => {
                    let latest = versions.last().unwrap();
                    let mut out = data(id, latest);
                    out["files"] = file_list(latest);
                    out["change_requests"] = links(&state, id);
                    reply(200, json!({"data": out}))
                }
            },
            ("GET", ["changes", id, "versions"]) => match state.changes.get(*id) {
                None => not_found(),
                Some(versions) => {
                    let last = versions.len();
                    let rows = versions
                        .iter()
                        .enumerate()
                        .rev()
                        .map(|(i, v)| {
                            let mut out = data(id, v);
                            out["latest"] = json!(i + 1 == last);
                            out
                        })
                        .collect();
                    paged(rows, path, page)
                }
            },
            ("GET", ["changes", id, "versions", n]) => {
                let found = state.changes.get(*id).and_then(|versions| {
                    let n: u64 = n.parse().ok()?;
                    let latest = versions.last()?;
                    versions.iter().find(|v| v.number == n).map(|v| (v, latest))
                });
                match found {
                    None => not_found(),
                    Some((v, latest)) => {
                        let mut out = data(id, v);
                        out["files"] = file_list(v);
                        out["latest_status"] = json!(latest.status);
                        reply(200, json!({"data": out}))
                    }
                }
            }
            ("DELETE", ["changes", id]) => {
                if !state.changes.contains_key(*id) {
                    return not_found();
                }
                if state.links.get(*id).is_some_and(|l| !l.is_empty()) {
                    let crs = links(&state, id);
                    let numbers: Vec<String> = crs
                        .as_array()
                        .unwrap()
                        .iter()
                        .map(|cr| format!("#{} ({})", cr["number"], cr["state"].as_str().unwrap()))
                        .collect();
                    let detail = format!(
                        "Change requests {} link this change, so it cannot be deleted while they exist",
                        numbers.join(", ")
                    );
                    return reply(
                        409,
                        json!({"errors": {"detail": detail, "change_requests": crs}}),
                    );
                }
                state.changes.remove(*id);
                Reply::Status(204, vec![], String::new())
            }
            _ => not_found(),
        }
    })
}

/// Files as the server sends them back: `[{path, text}]` sorted by path.
fn files_list(files: &BTreeMap<String, String>) -> Value {
    json!(files
        .iter()
        .map(|(path, text)| json!({"path": path, "text": text}))
        .collect::<Vec<_>>())
}

fn files_value(files: &BTreeMap<String, String>) -> Value {
    Value::Object(
        files
            .iter()
            .map(|(k, v)| (k.clone(), json!(v)))
            .collect::<Map<_, _>>(),
    )
}

/// Stores `versions` of `id`, oldest first, each with its files.
fn seed(state: &Shared, id: &str, versions: &[(&str, BTreeMap<String, String>)]) {
    let mut state = state.lock().unwrap();
    let rows = versions
        .iter()
        .enumerate()
        .map(|(i, (status, files))| Version {
            number: i as u64 + 1,
            status: (*status).to_owned(),
            source: json!({"commit": "a".repeat(40), "branch": "main", "dirty": false}),
            files: files_value(files),
        })
        .collect();
    state.changes.insert(id.to_owned(), rows);
}

// Scenario 1
#[test]
fn push_refuses_stray_files_invalid_changes_and_a_reused_id_and_sends_nothing() {
    let p = checkout();
    let dir = write_change(&p, "add-login");
    let state = Shared::default();
    let mock = store(state.clone(), 50);

    std::fs::write(dir.join("notes.txt"), "x\n").unwrap();
    std::fs::create_dir_all(dir.join("specs/auth/extra")).unwrap();
    std::fs::write(dir.join("specs/auth/extra/readme.md"), "x\n").unwrap();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1, "{}", stderr(&output));
    assert!(stderr(&output).contains("notes.txt"), "{}", stderr(&output));
    assert!(
        stderr(&output).contains("specs/auth/extra/readme.md"),
        "{}",
        stderr(&output)
    );
    assert!(mock.requests().is_empty());
    std::fs::remove_file(dir.join("notes.txt")).unwrap();
    std::fs::remove_dir_all(dir.join("specs/auth/extra")).unwrap();

    // A validate error refuses the push.
    std::fs::write(
        dir.join("specs/auth/spec.md"),
        DELTA.replace("Refs: G1\n", ""),
    )
    .unwrap();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("Refs:"), "{}", stderr(&output));
    assert!(mock.requests().is_empty());
    std::fs::write(dir.join("specs/auth/spec.md"), DELTA).unwrap();

    // A symlinked file refuses the push.
    std::fs::rename(dir.join("qa-scenarios.md"), p.dir.join("qa.md")).unwrap();
    std::os::unix::fs::symlink(p.dir.join("qa.md"), dir.join("qa-scenarios.md")).unwrap();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("symlink"), "{}", stderr(&output));
    assert!(mock.requests().is_empty());
    std::fs::remove_file(dir.join("qa-scenarios.md")).unwrap();
    std::fs::rename(p.dir.join("qa.md"), dir.join("qa-scenarios.md")).unwrap();

    // A warning alone does not stop it.
    let warned = change_md("add-login", "proposed").replace(
        "decisions: []",
        "decisions: [{id: D1, question: Form?, answer: Plain, by: agent, why: Simple, rejected: []}]",
    );
    std::fs::write(dir.join("change.md"), warned).unwrap();
    let validate = p.run(&["change", "validate", "add-login", "--json"]);
    assert_eq!(code(&validate), 0);
    assert!(!event(&validate)["files"][0]["warnings"]
        .as_array()
        .unwrap()
        .is_empty());
    let output = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&output);
    assert_eq!(event(&output)["version"], 1);
    assert_eq!(event(&output)["status"], "created");

    // An archived directory beside the active one refuses the push.
    let archived = p.dir.join("goodflows/changes/archive/2026-11-20-add-login");
    write_files(&archived, &change_files("add-login", "archived"));
    let before = mock.requests().len();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("goodflows/changes/archive/2026-11-20-add-login"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("new id"), "{}", stderr(&output));
    assert_eq!(mock.requests().len(), before);

    // Several archived directories and no active one: each is named.
    std::fs::remove_dir_all(&dir).unwrap();
    let second = p.dir.join("goodflows/changes/archive/2026-11-22-add-login");
    write_files(&second, &change_files("add-login", "archived"));
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("2026-11-20-add-login"),
        "{}",
        stderr(&output)
    );
    assert!(
        stderr(&output).contains("2026-11-22-add-login"),
        "{}",
        stderr(&output)
    );
    assert_eq!(mock.requests().len(), before);

    // No directory at all.
    let output = run(&p, &mock, &["change", "push", "add-nothing"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("add-nothing"),
        "{}",
        stderr(&output)
    );
    assert_eq!(mock.requests().len(), before);

    // A fresh checkout without the archive: the server refuses, and push names the archived id.
    let fresh = checkout();
    write_change(&fresh, "add-login");
    let archived_state = Shared::default();
    seed(
        &archived_state,
        "add-login",
        &[("archived", change_files("add-login", "archived"))],
    );
    let server = store(archived_state.clone(), 50);
    let output = run(&fresh, &server, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("stored change add-login is archived"),
        "{}",
        stderr(&output)
    );
    assert_eq!(archived_state.lock().unwrap().changes["add-login"].len(), 1);
}

// Scenario 2
#[test]
fn push_finds_an_archived_change_and_checks_only_its_files() {
    let p = checkout();
    let dir = write_change(&p, "add-login");
    let state = Shared::default();
    let mock = store(state.clone(), 50);
    success(&run(&p, &mock, &["change", "push", "add-login"]));

    let archived = p.dir.join("goodflows/changes/archive/2026-11-20-add-login");
    std::fs::create_dir_all(archived.parent().unwrap()).unwrap();
    std::fs::rename(&dir, &archived).unwrap();
    std::fs::write(
        archived.join("change.md"),
        change_md("add-login", "archived"),
    )
    .unwrap();
    // Not valid as an active change (its delta would duplicate the durable spec), but archived
    // directories skip validate.
    std::fs::write(archived.join("specs/auth/spec.md"), "anything\n").unwrap();
    let output = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&output);
    let pushed = event(&output);
    assert_eq!(pushed["version"], 2);
    assert_eq!(pushed["change"]["status"], "archived");
    let show = run(&p, &mock, &["change", "show", "add-login"]);
    success(&show);
    assert!(stdout(&show).contains("archived"), "{}", stdout(&show));

    // An archived change.md for another id refuses.
    std::fs::write(
        archived.join("change.md"),
        change_md("add-other", "archived"),
    )
    .unwrap();
    let before = mock.requests().len();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("change.md"), "{}", stderr(&output));
    // So does one without front matter.
    std::fs::write(archived.join("change.md"), "# no front matter\n").unwrap();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    // And a file that is not UTF-8.
    std::fs::write(
        archived.join("change.md"),
        change_md("add-login", "archived"),
    )
    .unwrap();
    std::fs::write(archived.join("verification.md"), [0xff, 0xfe]).unwrap();
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("verification.md"),
        "{}",
        stderr(&output)
    );
    assert_eq!(mock.requests().len(), before);
}

// Scenario 3
#[test]
fn push_sends_every_file_and_its_provenance_in_one_request() {
    let p = checkout();
    let dir = write_change(&p, "add-login");
    git(&p, &["add", "."]);
    git(&p, &["commit", "-q", "-m", "plan add-login"]);
    let head = git(&p, &["rev-parse", "HEAD"]);
    let state = Shared::default();
    let mock = store(state.clone(), 50);

    let output = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&output);
    let requests = mock.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].method, "POST");
    assert_eq!(requests[0].path, format!("{PREFIX}/changes"));
    assert_eq!(requests[0].headers["authorization"], "Bearer sk_test");
    assert_eq!(
        requests[0].json(),
        json!({"files": files_value(&change_files("add-login", "proposed")),
               "source": {"commit": head, "branch": "plan", "dirty": false}})
    );
    let show = run(&p, &mock, &["--json", "change", "show", "add-login"]);
    success(&show);
    assert_eq!(event(&show)["files"], files_list(&read_dir(&dir)));

    // Uncommitted edit on a detached HEAD.
    std::fs::write(dir.join("change.md"), change_md("add-login", "approved")).unwrap();
    git(&p, &["checkout", "-q", "--detach"]);
    success(&run(&p, &mock, &["change", "push", "add-login"]));
    let source = posts(&mock).last().unwrap().json()["source"].clone();
    assert_eq!(
        source,
        json!({"commit": head, "branch": null, "dirty": true})
    );
    git(&p, &["checkout", "-q", "plan"]);
    git(&p, &["checkout", "-q", "--", "."]);

    // Back to exactly HEAD's files: clean.
    git(&p, &["checkout", "-q", "--", "."]);
    success(&run(&p, &mock, &["change", "push", "add-login"]));
    assert_eq!(
        posts(&mock).last().unwrap().json()["source"],
        json!({"commit": head, "branch": "plan", "dirty": false})
    );

    // Without the key nothing is sent.
    let before = mock.requests().len();
    let output = p
        .command()
        .env("GOODFLOWS_URL", &mock.url)
        .args(["change", "push", "add-login"])
        .output()
        .unwrap();
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("GOODFLOWS_URL and GOODFLOWS_API_KEY"),
        "{}",
        stderr(&output)
    );
    assert_eq!(mock.requests().len(), before);

    // A branch name the server refuses is refused before sending.
    let long = vec!["ab"; 130].join("/");
    git(&p, &["checkout", "-q", "-b", &long]);
    let output = run(&p, &mock, &["change", "push", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("256 bytes"), "{}", stderr(&output));
    assert_eq!(mock.requests().len(), before);

    // So is one with a control character; git refuses C0 in branch names
    // but takes C1, such as U+0085 and U+009F.
    for branch in ["plan\u{85}x", "plan\u{9f}x"] {
        git(&p, &["checkout", "-q", "-b", branch]);
        let output = run(&p, &mock, &["change", "push", "add-login"]);
        assert_eq!(code(&output), 1, "{branch:?}: {}", stderr(&output));
        assert!(
            stderr(&output).contains("control characters"),
            "{}",
            stderr(&output)
        );
        assert_eq!(mock.requests().len(), before, "{branch:?}");
    }
}

/// Archived directories skip validate, so any allowed file can be added.
/// Its status stays approved, as the server refuses new versions after an
/// archived one.
#[test]
fn only_exactly_heads_files_make_a_clean_push() {
    let p = checkout();
    let archived = p.dir.join("goodflows/changes/archive/2026-11-20-add-login");
    let mut files = change_files("add-login", "approved");
    files.insert("verification.md".into(), "v\n".into());
    write_files(&archived, &files);
    git(&p, &["add", "."]);
    git(&p, &["commit", "-q", "-m", "archive"]);
    let mock = store(Shared::default(), 50);
    let dirty = || {
        success(&run(&p, &mock, &["change", "push", "add-login"]));
        posts(&mock).last().unwrap().json()["source"]["dirty"].clone()
    };
    assert_eq!(dirty(), false);
    // A deleted tracked file.
    std::fs::remove_file(archived.join("verification.md")).unwrap();
    assert_eq!(dirty(), true);
    git(&p, &["checkout", "-q", "--", "."]);
    // An untracked file.
    std::fs::remove_file(archived.join("qa-scenarios.md")).unwrap();
    git(&p, &["commit", "-q", "-am", "drop qa"]);
    assert_eq!(dirty(), false);
    std::fs::write(archived.join("qa-scenarios.md"), "qa\n").unwrap();
    assert_eq!(dirty(), true);
    // An ignored file.
    std::fs::write(p.dir.join(".gitignore"), ".goodflows/\nqa-scenarios.md\n").unwrap();
    git(&p, &["commit", "-q", "-am", "ignore"]);
    assert_eq!(dirty(), true);
    std::fs::remove_file(archived.join("qa-scenarios.md")).unwrap();
    assert_eq!(dirty(), false);
}

#[test]
fn push_from_a_repository_without_commits_sends_a_null_commit() {
    let p = Project::new(CONFIG);
    git(&p, &["init", "-q", "-b", "fresh"]);
    write_change(&p, "add-login");
    let mock = store(Shared::default(), 50);
    success(&run(&p, &mock, &["change", "push", "add-login"]));
    assert_eq!(
        posts(&mock)[0].json()["source"],
        json!({"commit": null, "branch": "fresh", "dirty": true})
    );
}

// Scenario 4
#[test]
fn identical_push_is_unchanged_and_a_later_push_names_the_version_it_replaced() {
    let p = checkout();
    let dir = write_change(&p, "add-login");
    let state = Shared::default();
    let mock = store(state.clone(), 50);
    let first = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&first);
    let first = event(&first);
    assert_eq!(first["type"], "change_push");
    assert_eq!(first["schema_version"], 1);
    assert_eq!(first["id"], "add-login");
    assert_eq!(first["status"], "created");
    assert_eq!(first["replaced_version"], Value::Null);
    let second = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&second);
    let second = event(&second);
    assert_eq!(second["status"], "unchanged");
    assert_eq!(second["version"], first["version"]);
    assert_eq!(second["replaced_version"], first["version"]);
    assert_eq!(second["change"]["status"], "proposed");
    // `change` is the server's stored-change object without the push result fields.
    assert_eq!(second["change"]["change_id"], "add-login");
    assert_eq!(second["change"]["version"], first["version"]);
    assert!(second["change"].get("duplicate").is_none());
    assert!(second["change"].get("replaced_version").is_none());
    // From another branch the kept version still shows where it came from.
    git(&p, &["checkout", "-q", "-b", "other"]);
    let unchanged = run(&p, &mock, &["change", "push", "add-login"]);
    success(&unchanged);
    let text = stdout(&unchanged);
    assert!(text.contains("unchanged"), "{text}");
    let kept = text
        .lines()
        .find(|l| l.contains("version 1 was pushed from"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(kept.contains("branch plan"), "{text}");
    assert!(!kept.contains("branch other"), "{text}");
    assert!(
        text.lines()
            .any(|l| l.contains("this push came from") && l.contains("branch other")),
        "{text}"
    );
    git(&p, &["checkout", "-q", "plan"]);

    // Someone else pushes version 2 in between.
    {
        let mut state = state.lock().unwrap();
        let versions = state.changes.get_mut("add-login").unwrap();
        let mut other = versions[0].clone();
        other.number = 2;
        other.files["qa-scenarios.md"] = json!("other\n");
        versions.push(other);
    }
    std::fs::write(dir.join("change.md"), change_md("add-login", "approved")).unwrap();
    let output = run(&p, &mock, &["--json", "change", "push", "add-login"]);
    success(&output);
    let third = event(&output);
    assert_eq!(third["status"], "created");
    assert_eq!(third["version"], 3);
    assert_eq!(third["replaced_version"], 2);
    std::fs::write(dir.join("change.md"), change_md("add-login", "proposed")).unwrap();
    let text = run(&p, &mock, &["change", "push", "add-login"]);
    success(&text);
    assert!(stdout(&text).contains("version 4"), "{}", stdout(&text));
    assert!(
        stdout(&text).contains("replaced version 3"),
        "{}",
        stdout(&text)
    );
}

// Scenario 5
#[test]
fn list_follows_every_page_and_filters_by_status() {
    let p = checkout();
    let state = Shared::default();
    for id in ["add-a", "add-b", "add-c", "add-d"] {
        seed(&state, id, &[("proposed", change_files(id, "proposed"))]);
    }
    seed(
        &state,
        "add-e",
        &[
            ("proposed", change_files("add-e", "proposed")),
            ("approved", change_files("add-e", "approved")),
        ],
    );
    state
        .lock()
        .unwrap()
        .links
        .insert("add-e".into(), vec![7, 9]);
    state.lock().unwrap().links.insert("add-b".into(), vec![3]);
    // add-b's row comes from an older server, without the field.
    state.lock().unwrap().unlinked_rows.insert("add-b".into());
    let mock = store(state.clone(), 2);

    let output = run(&p, &mock, &["--json", "change", "list"]);
    success(&output);
    let listed = event(&output);
    assert_eq!(listed["type"], "change_list");
    let changes = listed["changes"].as_array().unwrap();
    let ids: Vec<&str> = changes
        .iter()
        .map(|c| c["change_id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["add-a", "add-b", "add-c", "add-d", "add-e"]);
    // The event carries each row's linked change requests as the server sent them.
    assert_eq!(
        changes[4]["change_requests"],
        json!([{"number": 7, "state": "open"}, {"number": 9, "state": "open"}])
    );
    assert_eq!(changes[0]["change_requests"], json!([]));
    assert!(changes[1].get("change_requests").is_none());
    let paths: Vec<String> = mock.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        paths,
        [
            format!("{PREFIX}/changes?limit=200"),
            format!("{PREFIX}/changes?limit=200&cursor=2"),
            format!("{PREFIX}/changes?limit=200&cursor=4"),
        ]
    );

    let text = run(&p, &mock, &["change", "list"]);
    success(&text);
    let lines: Vec<String> = stdout(&text).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert_eq!(
        lines[4],
        "add-e  approved  v2  2026-11-22T10:00:00Z  #7, #9"
    );
    // No linking change request, and a row without the field, both read `-`.
    assert_eq!(lines[0], "add-a  proposed  v1  2026-11-21T10:00:00Z  -");
    assert_eq!(lines[1], "add-b  proposed  v1  2026-11-21T10:00:00Z  -");

    let approved = run(
        &p,
        &mock,
        &["--json", "change", "list", "--status", "approved"],
    );
    success(&approved);
    let changes = event(&approved)["changes"].clone();
    assert_eq!(changes.as_array().unwrap().len(), 1);
    assert_eq!(changes[0]["change_id"], "add-e");
    assert!(mock
        .requests()
        .last()
        .unwrap()
        .path
        .contains("status=approved"));

    let before = mock.requests().len();
    let bad = run(&p, &mock, &["change", "list", "--status", "done"]);
    assert_eq!(code(&bad), 2);
    assert_eq!(mock.requests().len(), before);
}

// Scenario 6
#[test]
fn show_prints_metadata_an_older_version_or_one_file() {
    let p = checkout();
    let state = Shared::default();
    let mut v2 = change_files("add-login", "approved");
    v2.insert("verification.md".into(), "verified\n".into());
    seed(
        &state,
        "add-login",
        &[
            ("proposed", change_files("add-login", "proposed")),
            ("approved", v2),
        ],
    );
    state
        .lock()
        .unwrap()
        .links
        .insert("add-login".into(), vec![7]);
    let mock = store(state.clone(), 50);

    let old = run(
        &p,
        &mock,
        &["--json", "change", "show", "add-login", "--version", "1"],
    );
    success(&old);
    let shown = event(&old);
    assert_eq!(shown["type"], "change_show");
    assert_eq!(shown["version"], 1);
    assert_eq!(shown["status"], "proposed");
    // The version read has no linked change requests, so they come from the change (D52).
    assert_eq!(
        shown["change_requests"],
        json!([{"number": 7, "state": "open"}])
    );
    let reads: Vec<String> = mock.requests().iter().map(|r| r.path.clone()).collect();
    assert_eq!(
        reads,
        [
            format!("{PREFIX}/changes/add-login/versions/1"),
            format!("{PREFIX}/changes/add-login"),
        ]
    );
    let text = run(
        &p,
        &mock,
        &["change", "show", "add-login", "--version", "1"],
    );
    success(&text);
    let text = stdout(&text);
    assert!(text.contains("version 1"), "{text}");
    assert!(text.contains("proposed"), "{text}");
    assert!(text.contains("Change requests: #7"), "{text}");
    assert!(!text.contains("verification.md"), "{text}");

    let latest = run(&p, &mock, &["change", "show", "add-login"]);
    success(&latest);
    let latest = stdout(&latest);
    for part in [
        "version 2",
        "approved",
        "2026-11-22T10:00:00Z",
        "aaaaaaaaaaaa",
        "main",
        "#7",
        "verification.md",
        "qa-scenarios.md",
        "specs/auth/spec.md",
    ] {
        assert!(latest.contains(part), "{part}: {latest}");
    }

    let file = run(
        &p,
        &mock,
        &["change", "show", "add-login", "--file", "qa-scenarios.md"],
    );
    success(&file);
    assert_eq!(stdout(&file), qa("add-login"));

    let missing = run(
        &p,
        &mock,
        &["change", "show", "add-login", "--file", "nope.md"],
    );
    assert_eq!(code(&missing), 1);
    assert!(stderr(&missing).contains("nope.md"), "{}", stderr(&missing));
    assert!(
        stderr(&missing).contains("qa-scenarios.md"),
        "{}",
        stderr(&missing)
    );
    let before = mock.requests().len();
    assert_eq!(
        code(&run(
            &p,
            &mock,
            &["change", "show", "add-login", "--version", "0"]
        )),
        2
    );
    assert_eq!(mock.requests().len(), before);
    let unknown = run(&p, &mock, &["change", "show", "no-such-change"]);
    assert_eq!(code(&unknown), 1);
    assert!(
        stderr(&unknown).contains("change show failed: not found (HTTP 404)"),
        "{}",
        stderr(&unknown)
    );
    assert_eq!(
        code(&run(
            &p,
            &mock,
            &["change", "show", "add-login", "--version", "9"]
        )),
        1
    );
}

// Scenario 7
#[test]
fn versions_lists_the_history_newest_first() {
    let p = checkout();
    let state = Shared::default();
    seed(
        &state,
        "add-login",
        &[
            ("proposed", change_files("add-login", "proposed")),
            ("approved", change_files("add-login", "approved")),
            ("archived", change_files("add-login", "archived")),
        ],
    );
    let mock = store(state.clone(), 2);
    let output = run(&p, &mock, &["--json", "change", "versions", "add-login"]);
    success(&output);
    let listed = event(&output);
    assert_eq!(listed["type"], "change_versions");
    let versions = listed["versions"].as_array().unwrap();
    let numbers: Vec<u64> = versions
        .iter()
        .map(|v| v["version"].as_u64().unwrap())
        .collect();
    assert_eq!(numbers, [3, 2, 1]);
    assert_eq!(versions[0]["latest"], true);
    assert_eq!(versions[1]["latest"], false);
    for v in versions {
        assert!(v["pushed_at"].is_string() && v["source"]["commit"].is_string());
        assert!(v["source"]["branch"].is_string() && v["source"]["dirty"].is_boolean());
        assert_eq!(v["pushed_by"]["user_id"], "u1");
    }
    assert_eq!(mock.requests().len(), 2);
    let text = run(&p, &mock, &["change", "versions", "add-login"]);
    success(&text);
    let lines: Vec<String> = stdout(&text).lines().map(str::to_owned).collect();
    assert_eq!(lines.len(), 3, "{lines:?}");
    assert!(
        lines[0].starts_with("v3") && lines[0].contains("latest"),
        "{lines:?}"
    );
    assert!(
        lines[2].starts_with("v1") && !lines[2].contains("latest"),
        "{lines:?}"
    );
    assert!(
        lines[2].contains("main")
            && lines[2].contains("aaaaaaaaaaaa")
            && lines[2].contains("clean"),
        "{lines:?}"
    );
}

// Scenario 8
#[test]
fn pull_writes_a_version_and_refuses_over_differing_local_files() {
    let p = checkout();
    let state = Shared::default();
    let v1 = change_files("add-login", "proposed");
    let mut v2 = change_files("add-login", "approved");
    v2.insert("verification.md".into(), "verified\n".into());
    seed(
        &state,
        "add-login",
        &[("proposed", v1.clone()), ("approved", v2.clone())],
    );
    let mock = store(state.clone(), 50);
    let dir = p.dir.join("goodflows/changes/add-login");

    let output = run(&p, &mock, &["--json", "change", "pull", "add-login"]);
    success(&output);
    assert_eq!(event(&output)["type"], "change_pull");
    assert_eq!(read_dir(&dir), v2);
    let status = git(&p, &["status", "--porcelain"]);
    assert_eq!(status, "?? goodflows/");
    assert_eq!(git(&p, &["diff", "--cached", "--name-only"]), "");

    // The same files again: nothing to write, exit 0.
    success(&run(&p, &mock, &["change", "pull", "add-login"]));
    assert_eq!(read_dir(&dir), v2);

    std::fs::create_dir_all(dir.join("specs/extra")).unwrap();
    std::fs::write(dir.join("specs/extra/spec.md"), "mine\n").unwrap();
    std::fs::write(dir.join("change.md"), "edited\n").unwrap();
    let before = read_dir(&dir);
    let output = run(&p, &mock, &["change", "pull", "add-login"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("specs/extra/spec.md"),
        "{}",
        stderr(&output)
    );
    assert!(stderr(&output).contains("change.md"), "{}", stderr(&output));
    assert!(stderr(&output).contains("--force"), "{}", stderr(&output));
    assert_eq!(read_dir(&dir), before);

    success(&run(&p, &mock, &["change", "pull", "add-login", "--force"]));
    assert_eq!(read_dir(&dir), v2);
    assert!(!dir.join("specs/extra").exists());

    success(&run(
        &p,
        &mock,
        &["change", "pull", "add-login", "--version", "1", "--force"],
    ));
    assert_eq!(read_dir(&dir), v1);
    let before = mock.requests().len();
    assert_eq!(
        code(&run(
            &p,
            &mock,
            &["change", "pull", "add-login", "--version", "0"]
        )),
        2
    );
    assert_eq!(mock.requests().len(), before);

    // Nothing is left under goodflows/ besides the change.
    let leftovers: Vec<_> = files_under(&p.dir.join("goodflows"))
        .into_iter()
        .filter(|f| !f.starts_with(&dir))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    assert!(
        std::fs::read_dir(p.dir.join("goodflows/changes"))
            .unwrap()
            .count()
            == 1
    );

    // From a subdirectory, it writes under the checkout's top level.
    std::fs::remove_dir_all(&dir).unwrap();
    std::fs::create_dir_all(p.dir.join("lib")).unwrap();
    let output = p
        .command()
        .current_dir(p.dir.join("lib"))
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", "sk_test")
        .args([
            "--config",
            p.dir.join("goodflows.toml").to_str().unwrap(),
            "change",
            "pull",
            "add-login",
        ])
        .output()
        .unwrap();
    success(&output);
    assert_eq!(read_dir(&dir), v2);
}

// Scenario 9
#[test]
fn pull_refuses_a_change_whose_latest_version_is_archived() {
    let p = checkout();
    let state = Shared::default();
    seed(
        &state,
        "add-login",
        &[
            ("approved", change_files("add-login", "approved")),
            ("archived", change_files("add-login", "archived")),
        ],
    );
    let mock = store(state.clone(), 50);
    let dir = p.dir.join("goodflows/changes/add-login");
    for args in [
        &["change", "pull", "add-login", "--force"][..],
        &["change", "pull", "add-login", "--version", "1", "--force"][..],
    ] {
        let output = run(&p, &mock, args);
        assert_eq!(code(&output), 1, "{args:?}");
        assert!(stderr(&output).contains("archived"), "{}", stderr(&output));
        assert!(!dir.exists());
    }

    // A local archive of the id refuses even an active stored change.
    seed(
        &state,
        "add-login",
        &[("approved", change_files("add-login", "approved"))],
    );
    let archived = p.dir.join("goodflows/changes/archive/2026-11-20-add-login");
    write_files(&archived, &change_files("add-login", "archived"));
    let output = run(&p, &mock, &["change", "pull", "add-login", "--force"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("goodflows/changes/archive/2026-11-20-add-login"),
        "{}",
        stderr(&output)
    );
    assert!(!dir.exists());
}

/// `GET /changes/add-login` as the shipped server answers it, with one file.
fn version_data(status: &str) -> Value {
    json!({"change_id": "add-login", "version": 1, "status": status, "size": "small",
           "door": "two_way", "capabilities": [], "depends_on": [], "pushed_at": "t",
           "source": {"commit": null, "branch": null, "dirty": true}, "pushed_by": {"user_id": "u"},
           "files": [{"path": "change.md", "text": "new\n"}], "change_requests": []})
}

// Scenario 10
#[test]
fn pull_refuses_a_version_with_a_path_outside_the_change_files() {
    let p = checkout();
    let config = p.read(".git/config");
    let dir = p.dir.join("goodflows/changes/add-login");
    write_files(&dir, &change_files("add-login", "proposed"));
    let local = read_dir(&dir);
    let outside = std::env::temp_dir().join(format!("goodflows-outside-{}", std::process::id()));
    for (path, text) in [
        ("specs/../../../.git/config", json!("pwned")),
        (outside.to_str().unwrap(), json!("pwned")),
        ("notes.txt", json!("pwned")),
        ("specs//spec.md", json!("pwned")),
        ("specs/./spec.md", json!("pwned")),
        ("specs/Auth/spec.md", json!("pwned")),
        ("qa-scenarios.md", Value::Null),
        ("qa-scenarios.md", json!(7)),
        ("qa-scenarios.md", json!("missing")),
        ("change.md", json!("a second change.md")),
    ] {
        let mut entry = json!({"path": path, "text": text});
        if text == "missing" {
            entry.as_object_mut().unwrap().remove("text");
        }
        let files = json!([{"path": "change.md", "text": "---\nid: add-login\n---\n"}, entry]);
        let mock = Mock::start(move |r: &Request, _| {
            let mut data = version_data("approved");
            data["files"] = files.clone();
            assert_eq!(r.method, "GET");
            reply(200, json!({"data": data}))
        });
        let output = run(&p, &mock, &["change", "pull", "add-login", "--force"]);
        assert_eq!(code(&output), 1, "{path}: {}", stderr(&output));
        assert!(
            stderr(&output).contains(path),
            "{path}: {}",
            stderr(&output)
        );
        assert_eq!(read_dir(&dir), local, "{path}");
        assert_eq!(p.read(".git/config"), config);
        assert!(!outside.exists());
        assert!(!p.dir.join("goodflows/changes/add-login/notes.txt").exists());
    }

    // A version that does not say its change's status is refused (D51).
    let unknown = Mock::start(|_: &Request, _| {
        let mut data = version_data("approved");
        data.as_object_mut().unwrap().remove("status");
        reply(200, json!({"data": data}))
    });
    let output = run(&p, &unknown, &["change", "pull", "add-login", "--force"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("archived"), "{}", stderr(&output));
    assert_eq!(read_dir(&dir), local);

    // For --version N the latest version's status is latest_status; the
    // version's own status does not stand in for it.
    let older = Mock::start(|_: &Request, _| reply(200, json!({"data": version_data("approved")})));
    let output = run(
        &p,
        &older,
        &["change", "pull", "add-login", "--version", "1", "--force"],
    );
    assert_eq!(code(&output), 1, "{}", stderr(&output));
    assert!(stderr(&output).contains("archived"), "{}", stderr(&output));
    assert_eq!(read_dir(&dir), local);
    let paths: Vec<String> = older.requests().into_iter().map(|r| r.path).collect();
    assert_eq!(paths, [format!("{PREFIX}/changes/add-login/versions/1")]);

    // A symlink in the local directory refuses the pull.
    let good = Mock::start(|_: &Request, _| reply(200, json!({"data": version_data("approved")})));
    std::fs::write(p.dir.join("target.md"), "keep\n").unwrap();
    std::os::unix::fs::symlink(p.dir.join("target.md"), dir.join("verification.md")).unwrap();
    let output = run(&p, &good, &["change", "pull", "add-login", "--force"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("symlink"), "{}", stderr(&output));
    assert_eq!(p.read("target.md"), "keep\n");
    assert!(dir.join("specs/auth/spec.md").exists());
    std::fs::remove_file(dir.join("verification.md")).unwrap();

    // So does a symlinked change directory.
    let elsewhere = p.dir.join("elsewhere");
    std::fs::rename(&dir, &elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, &dir).unwrap();
    let output = run(&p, &good, &["change", "pull", "add-login", "--force"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("symlink"), "{}", stderr(&output));
    assert_eq!(read_dir(&elsewhere), local);
}

// Scenario 11, the signal half; the write failure half is a unit test in
// src/change_requests/stored.rs.
#[test]
fn an_interrupted_pull_exits_128_plus_the_signal_and_writes_nothing() {
    let p = checkout();
    let dir = write_change(&p, "add-login");
    let local = read_dir(&dir);
    let mock = Mock::start(|_: &Request, _| Reply::Hang(std::time::Duration::from_secs(20)));
    let child = p
        .command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", "sk_test")
        .args(["change", "pull", "add-login", "--force"])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    let start = std::time::Instant::now();
    while mock.requests().is_empty() && start.elapsed().as_secs() < 10 {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(code(&output), 130, "{}", stderr(&output));
    assert_eq!(read_dir(&dir), local);
}

// Scenario 11, the write failure half through pull itself: a read-only
// goodflows/changes blocks the swap, while files in the change directory
// stay writable, so a pull that wrote in place would change them.
#[test]
fn a_pull_whose_write_fails_leaves_the_directory_as_it_was() {
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        return; // Permissions do not stop root.
    }
    use std::os::unix::fs::PermissionsExt;
    let p = checkout();
    let dir = write_change(&p, "add-login");
    let local = read_dir(&dir);
    let state = Shared::default();
    let mut files = change_files("add-login", "approved");
    files.insert("verification.md".into(), "verified\n".into());
    seed(&state, "add-login", &[("approved", files)]);
    let mock = store(state, 50);
    let changes = p.dir.join("goodflows/changes");
    std::fs::set_permissions(&changes, std::fs::Permissions::from_mode(0o555)).unwrap();
    let output = run(&p, &mock, &["change", "pull", "add-login", "--force"]);
    std::fs::set_permissions(&changes, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(code(&output), 1, "{}", stderr(&output));
    assert!(
        stderr(&output).contains("nothing was written"),
        "{}",
        stderr(&output)
    );
    assert_eq!(read_dir(&dir), local);
    let left: Vec<String> = std::fs::read_dir(p.dir.join("goodflows"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name != "changes")
        .collect();
    assert!(left.is_empty(), "staging left behind: {left:?}");
}

// Scenario 12
#[test]
fn delete_needs_yes_and_names_the_linking_change_requests() {
    let p = checkout();
    let state = Shared::default();
    seed(
        &state,
        "add-login",
        &[("approved", change_files("add-login", "approved"))],
    );
    seed(
        &state,
        "drop-me",
        &[("proposed", change_files("drop-me", "proposed"))],
    );
    state
        .lock()
        .unwrap()
        .links
        .insert("add-login".into(), vec![7]);
    let mock = store(state.clone(), 50);

    let output = run(&p, &mock, &["change", "delete", "add-login"]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("--yes"), "{}", stderr(&output));
    assert!(mock.requests().is_empty());

    let output = run(&p, &mock, &["change", "delete", "add-login", "--yes"]);
    assert_eq!(code(&output), 1);
    assert!(stderr(&output).contains("#7"), "{}", stderr(&output));
    assert_eq!(mock.requests()[0].method, "DELETE");
    assert_eq!(
        mock.requests()[0].path,
        format!("{PREFIX}/changes/add-login")
    );
    assert!(state.lock().unwrap().changes.contains_key("add-login"));

    let output = run(
        &p,
        &mock,
        &["--json", "change", "delete", "drop-me", "--yes"],
    );
    success(&output);
    let deleted = event(&output);
    assert_eq!(deleted["type"], "change_delete");
    assert_eq!(deleted["id"], "drop-me");
    assert_eq!(code(&run(&p, &mock, &["change", "show", "drop-me"])), 1);
    let text = run(&p, &mock, &["change", "delete", "drop-me", "--yes"]);
    assert_eq!(code(&text), 1);
    assert!(
        stderr(&text).contains("not found (HTTP 404)"),
        "{}",
        stderr(&text)
    );
}

// Scenario 13
#[test]
fn an_older_server_says_it_does_not_store_changes_yet() {
    let p = checkout();
    write_change(&p, "add-login");
    let older = Mock::start(|r: &Request, _| {
        if r.path.starts_with(&format!("{PREFIX}/change-requests")) {
            reply(200, json!({"data": [], "next_cursor": null}))
        } else {
            reply(404, json!({"errors": {"detail": "Not Found"}}))
        }
    });
    for args in [
        &["change", "push", "add-login"][..],
        &["change", "list"][..],
    ] {
        let output = run(&p, &older, args);
        assert_eq!(code(&output), 1, "{args:?}");
        assert!(
            stderr(&output).contains("does not store changes yet"),
            "{}",
            stderr(&output)
        );
    }
    let calls: Vec<String> = older
        .requests()
        .iter()
        .map(|r| format!("{} {}", r.method, r.path.split('?').next().unwrap()))
        .collect();
    assert_eq!(
        calls,
        [
            format!("POST {PREFIX}/changes"),
            format!("GET {PREFIX}/change-requests"),
            format!("GET {PREFIX}/changes"),
            format!("GET {PREFIX}/change-requests"),
        ]
    );

    // A missing project: the probe fails too, so the 404 stays.
    let missing =
        Mock::start(|_: &Request, _| reply(404, json!({"errors": {"detail": "Not Found"}})));
    let output = run(&p, &missing, &["change", "list"]);
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains(
            r#"change list failed: not found (HTTP 404): {"errors":{"detail":"Not Found"}}"#
        ),
        "{}",
        stderr(&output)
    );
    assert_eq!(missing.requests().len(), 2);

    // show never probes.
    let routes = store(Shared::default(), 50);
    let output = run(&p, &routes, &["change", "show", "no-such-change"]);
    assert_eq!(code(&output), 1);
    assert!(
        !stderr(&output).contains("does not store"),
        "{}",
        stderr(&output)
    );
    assert_eq!(routes.requests().len(), 1);
}

// Scenario 14 and 15
#[test]
fn stored_change_commands_follow_the_change_request_conventions() {
    let p = checkout();
    write_change(&p, "add-login");
    let state = Shared::default();
    let mock = store(state.clone(), 50);
    let commands = [
        (&["change", "push", "add-login"][..], "change_push"),
        (&["change", "list"][..], "change_list"),
        (&["change", "show", "add-login"][..], "change_show"),
        (
            &["change", "show", "add-login", "--version", "1"][..],
            "change_show",
        ),
        (&["change", "versions", "add-login"][..], "change_versions"),
        (
            &["change", "pull", "add-login", "--force"][..],
            "change_pull",
        ),
        (
            &["change", "pull", "add-login", "--version", "1", "--force"][..],
            "change_pull",
        ),
        (
            &["change", "delete", "add-login", "--yes"][..],
            "change_delete",
        ),
    ];
    // Every request of every command goes to the [reporting] project with
    // the key from token_env as a bearer token.
    let each_command = |config: &[&str], env: &[(&str, &str)], key: &str| {
        for (args, kind) in commands {
            let before = mock.requests().len();
            let mut command = p.command();
            command.args(config).arg("--json").args(args);
            for (name, value) in env {
                command.env(name, value);
            }
            let output = command.output().unwrap();
            success(&output);
            let e = event(&output);
            assert_eq!(e["type"], kind);
            assert_eq!(e["schema_version"], 1);
            let sent = &mock.requests()[before..];
            assert!(!sent.is_empty(), "{args:?} sent nothing");
            for r in sent {
                assert!(
                    r.path.starts_with(&format!("{PREFIX}/")),
                    "{args:?}: {}",
                    r.path
                );
                assert_eq!(
                    r.headers["authorization"],
                    format!("Bearer {key}"),
                    "{args:?}"
                );
            }
            if args.contains(&"--version") {
                let path = format!("{PREFIX}/changes/add-login/versions/1");
                assert!(sent.iter().any(|r| r.path == path), "{args:?}");
            }
        }
    };
    each_command(
        &[],
        &[
            ("GOODFLOWS_URL", &mock.url),
            ("GOODFLOWS_API_KEY", "sk_test"),
        ],
        "sk_test",
    );

    // Custom url_env and token_env names are honored, and the default
    // variables are ignored.
    std::fs::write(
        p.dir.join("custom.toml"),
        CONFIG.replace(
            "project = 'goodcases'",
            "project = 'goodcases'\nurl_env = 'STORE_URL'\ntoken_env = 'STORE_KEY'",
        ),
    )
    .unwrap();
    let custom = ["--config", "custom.toml"];
    each_command(
        &custom,
        &[
            ("STORE_URL", &mock.url),
            ("STORE_KEY", "sk_custom"),
            ("GOODFLOWS_URL", "http://127.0.0.1:1"),
            ("GOODFLOWS_API_KEY", "sk_default"),
        ],
        "sk_custom",
    );
    let before = mock.requests().len();
    let output = p
        .command()
        .args(custom)
        .args(["change", "list"])
        .env("STORE_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", "sk_default")
        .output()
        .unwrap();
    assert_eq!(code(&output), 1);
    assert!(
        stderr(&output).contains("STORE_URL and STORE_KEY"),
        "{}",
        stderr(&output)
    );
    assert_eq!(mock.requests().len(), before);
    std::fs::remove_file(p.dir.join("custom.toml")).unwrap();

    // Unreachable server.
    let output = p
        .command()
        .env("GOODFLOWS_URL", "http://127.0.0.1:1")
        .env("GOODFLOWS_API_KEY", "sk_test")
        .args(["--json", "change", "list"])
        .output()
        .unwrap();
    assert_eq!(code(&output), 1);
    assert_eq!(
        event(&output),
        json!({"code": "runtime", "message": "change list failed: could not reach the server", "schema_version": 1, "type": "error"})
    );

    // --config works for the server commands, and not for the offline ones.
    std::fs::rename(p.dir.join("goodflows.toml"), p.dir.join("other.toml")).unwrap();
    success(&run(
        &p,
        &mock,
        &["--config", "other.toml", "change", "list"],
    ));
    let output = p.run(&["--config", "other.toml", "change", "validate", "add-login"]);
    assert_eq!(code(&output), 2);
    assert!(stderr(&output).contains("--config cannot be used with change commands"));

    // No goodflows.toml: validate needs none, list exits 2.
    let before = mock.requests().len();
    success(&p.run(&["change", "validate", "add-login"]));
    let output = run(&p, &mock, &["change", "list"]);
    assert_eq!(code(&output), 2, "{}", stderr(&output));
    assert_eq!(mock.requests().len(), before);
    std::fs::rename(p.dir.join("other.toml"), p.dir.join("goodflows.toml")).unwrap();

    // A run-only flag is a usage error.
    assert_eq!(code(&run(&p, &mock, &["change", "list", "--env", "ci"])), 2);

    // A key that cannot read file contents gets the ordinary 403.
    let forbidden = Mock::start(|r: &Request, _| {
        if r.path.starts_with(&format!("{PREFIX}/changes?")) {
            reply(200, json!({"data": [], "next_cursor": null}))
        } else {
            Reply::Status(403, vec![], String::new())
        }
    });
    success(&run(&p, &forbidden, &["change", "list"]));
    let dir = p.dir.join("goodflows/changes/add-login");
    let local = read_dir(&dir);
    for args in [
        &["change", "show", "add-login", "--file", "change.md"][..],
        &["change", "pull", "add-login", "--force"][..],
    ] {
        let output = run(&p, &forbidden, args);
        assert_eq!(code(&output), 1);
        let name = format!("change {} failed: forbidden (HTTP 403)", args[1]);
        assert!(
            stderr(&output).trim_end().ends_with(&name),
            "{}",
            stderr(&output)
        );
    }
    assert_eq!(read_dir(&dir), local);
}
