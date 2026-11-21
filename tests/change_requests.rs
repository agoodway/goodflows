mod common;
use common::mock::{Mock, Reply, Request};
use common::*;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::process::Output;

const CONFIG: &str =
    "[reporting]\nproject = 'goodcases'\n\n[[jobs]]\nname = 'test'\ncommand = 'true'\n";

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

/// A checkout on `feat/login` two commits ahead of `main`: one adds and
/// edits files, one renames a file.
fn checkout() -> Project {
    let p = Project::new(CONFIG);
    git(&p, &["init", "-q", "-b", "main"]);
    std::fs::write(p.dir.join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
    std::fs::write(p.dir.join(".gitignore"), ".goodflows/\n.verify/\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-q", "-m", "base"]);
    git(&p, &["checkout", "-q", "-b", "feat/login"]);
    std::fs::create_dir_all(p.dir.join("lib")).unwrap();
    std::fs::write(p.dir.join("lib/login.ex"), "a\nb\n").unwrap();
    git(&p, &["add", "."]);
    git(&p, &["commit", "-q", "-m", "add login"]);
    std::fs::create_dir_all(p.dir.join("docs")).unwrap();
    git(&p, &["mv", "old.txt", "docs/new.txt"]);
    git(&p, &["commit", "-q", "-m", "move"]);
    p
}

fn run(p: &Project, mock: &Mock, args: &[&str]) -> Output {
    p.command()
        .env("GOODFLOWS_URL", &mock.url)
        .env("GOODFLOWS_API_KEY", "sk_test")
        .args(args)
        .output()
        .unwrap()
}

fn last_event(output: &Output) -> Value {
    serde_json::from_str(stdout(output).lines().last().unwrap()).unwrap()
}

fn ok(body: Value) -> Reply {
    Reply::Status(200, vec![], body.to_string())
}

fn created(body: Value) -> Reply {
    Reply::Status(201, vec![], body.to_string())
}

fn cr(number: u64, state: &str, openspec: Option<&str>) -> Value {
    json!({"number": number, "title": "Add login", "state": state, "source_branch": "feat/login",
           "target_branch": "main", "openspec_change": openspec, "head": null, "merge": null})
}

/// A change request as POST create, git-facts and claims return it: with
/// its verdict, description and history.
fn full(mut cr: Value, head: Value) -> Value {
    cr["head"] = head.clone();
    cr["verdict"] = json!({"state": "pending", "head": head, "policy_version": 0, "reasons": []});
    cr["description"] = Value::Null;
    cr["history"] = json!([]);
    cr
}

/// Routes the change request API of project goodcases, with `existing` as
/// the change requests listed for the branch.
fn server(existing: Vec<Value>) -> Mock {
    Mock::start(move |r: &Request, _| {
        let path = r.path.strip_prefix("/api/v1/projects/goodcases").unwrap();
        match (r.method.as_str(), path.split('?').next().unwrap()) {
            ("GET", "/change-requests") => {
                let state = ["draft", "open", "closed"]
                    .into_iter()
                    .find(|s| path.contains(&format!("state={s}")))
                    .unwrap();
                let rows: Vec<_> = existing
                    .iter()
                    .filter(|c| c["state"] == state)
                    .cloned()
                    .collect();
                ok(json!({"data": rows, "next_cursor": null}))
            }
            ("POST", "/change-requests") => {
                let mut body = r.json();
                body["number"] = json!(7);
                body["state"] = json!("draft");
                body["target_branch"] = json!("main");
                created(json!({"data": full(body, Value::Null)}))
            }
            ("GET", p) if p.starts_with("/change-requests/") && !p.ends_with("/verdict") => ok(
                json!({"data": existing.first().cloned().unwrap_or_else(|| cr(7, "draft", None))}),
            ),
            ("POST", p) if p.ends_with("/git-facts") => {
                let head = r.json()["head"].clone();
                created(json!({"data": full(cr(7, "draft", None), head)}))
            }
            ("POST", p) if p.ends_with("/claims") => {
                let head = git_head(&r.body);
                // The claims add a reason, so this verdict differs from the git-facts one.
                let mut stored = full(cr(7, "draft", None), json!(head));
                stored["verdict"]["state"] = json!("needs_review");
                stored["description"] = json!(String::from_utf8_lossy(&r.body));
                created(json!({"data": stored}))
            }
            _ => Reply::Status(404, vec![], String::new()),
        }
    })
}

/// The `head` of a claims file body.
fn git_head(body: &[u8]) -> String {
    let text = String::from_utf8_lossy(body);
    text.lines()
        .find_map(|l| l.strip_prefix("head: "))
        .unwrap_or_default()
        .to_owned()
}

fn claims_file(head: &str) -> String {
    format!(
        "---\nformat: change-claims/2\ncr: null\nhead: {head}\ndoor: two_way\ndoor_reason: \"Easy to revert\"\nreach: \"Login page\"\nsafe_because:\n  - {{fact: Tested, location: \"lib/login.ex:1\", proof: stated}}\nleft_open: []\nquestions: []\ndecisions: []\n---\n\n# Add login\n\n## What changed\n\nA login page.\n"
    )
}

#[test]
fn open_creates_a_draft_reports_facts_and_uploads_claims_with_its_number() {
    let p = checkout();
    let head = git(&p, &["rev-parse", "HEAD"]);
    let base = git(&p, &["rev-parse", "main"]);
    let first = git(&p, &["rev-parse", "HEAD~1"]);
    let path = run(&p, &server(vec![]), &["cr", "path"]);
    success(&path);
    let file = stdout(&path).trim().to_owned();
    assert_eq!(
        file,
        p.dir
            .join(".goodflows/change-requests/feat/login/cr.md")
            .to_str()
            .unwrap()
    );
    std::fs::create_dir_all(std::path::Path::new(&file).parent().unwrap()).unwrap();
    std::fs::write(&file, claims_file(&head)).unwrap();

    let mock = server(vec![]);
    let output = run(&p, &mock, &["cr", "open", "--json"]);
    success(&output);
    let requests = mock.requests();
    let calls: Vec<_> = requests
        .iter()
        .map(|r| format!("{} {}", r.method, r.path))
        .collect();
    let prefix = "/api/v1/projects/goodcases/change-requests";
    assert_eq!(
        calls,
        [
            format!("GET {prefix}?state=draft&source_branch=feat%2Flogin&limit=200"),
            format!("GET {prefix}?state=open&source_branch=feat%2Flogin&limit=200"),
            format!("POST {prefix}"),
            format!("POST {prefix}/7/git-facts"),
            format!("POST {prefix}/7/claims"),
        ]
    );
    assert_eq!(requests[0].headers["authorization"], "Bearer sk_test");
    // The title comes from the claims file's heading.
    assert_eq!(
        requests[2].json(),
        json!({"title": "Add login", "source_branch": "feat/login"})
    );
    let facts = requests[3].json();
    assert_eq!(facts["format"], "git-facts/1");
    assert_eq!(facts["head"], head);
    assert_eq!(
        facts["target"],
        json!({"branch": "main", "merge_base": base})
    );
    assert_eq!(
        facts["commits"],
        json!([
            {"sha": first, "parents": [base], "changed_files": [
                {"path": "lib/login.ex", "status": "added", "additions": 2, "deletions": 0}]},
            {"sha": head, "parents": [first], "changed_files": [
                {"path": "docs/new.txt", "status": "renamed", "additions": 0, "deletions": 0, "old_path": "old.txt"}]},
        ])
    );
    assert_eq!(
        facts["diff"]["files"],
        json!([
            {"path": "docs/new.txt", "status": "renamed", "additions": 0, "deletions": 0, "old_path": "old.txt"},
            {"path": "lib/login.ex", "status": "added", "additions": 2, "deletions": 0},
        ])
    );
    // The number is written into the file before the upload, so the
    // stored claims and the file stay equal.
    let written = std::fs::read_to_string(&file).unwrap();
    assert_eq!(written, claims_file(&head).replacen("cr: null", "cr: 7", 1));
    assert_eq!(
        requests[4].headers["content-type"],
        "text/markdown; charset=utf-8"
    );
    assert_eq!(requests[4].body, written.as_bytes());
    let event = last_event(&output);
    assert_eq!(event["type"], "change_request");
    assert_eq!(event["created"], true);
    assert_eq!(event["claims"]["wrote_cr"], true);
    assert_eq!(event["git_facts"]["status"], "reported");
    // The verdict and head come from the responses, not another read.
    assert_eq!(event["verdict"]["state"], "needs_review");
    assert_eq!(event["verdict"]["head"], head);
    assert_eq!(event["change_request"]["head"], head);
    assert_eq!(event["change_request"]["verdict"]["state"], "needs_review");
    assert_eq!(event["change_request"]["description"], written.as_str());

    // Text output says what happened.
    let again = server(vec![cr(7, "draft", None)]);
    let text = run(&p, &again, &["cr", "claims"]);
    success(&text);
    let text = stdout(&text);
    assert!(
        text.contains("#7 draft  feat/login -> main  Add login"),
        "{text}"
    );
    assert!(text.contains("Claims: uploaded from"), "{text}");
    assert!(text.contains("Verdict: needs_review"), "{text}");
}

#[test]
fn reopen_finds_the_closed_change_request_of_the_branch() {
    let p = checkout();
    let mock = Mock::start(|r: &Request, _| {
        let path = r.path.strip_prefix("/api/v1/projects/goodcases").unwrap();
        match (r.method.as_str(), path.split('?').next().unwrap()) {
            ("GET", "/change-requests") if path.contains("state=closed") => {
                ok(json!({"data": [cr(4, "closed", None)], "next_cursor": null}))
            }
            ("GET", "/change-requests") => ok(json!({"data": [], "next_cursor": null})),
            ("POST", "/change-requests/4/reopen") => ok(json!({"data": cr(4, "draft", None)})),
            _ => Reply::Status(404, vec![], String::new()),
        }
    });
    let output = run(&p, &mock, &["cr", "reopen"]);
    success(&output);
    assert!(stdout(&output).contains("#4 draft"), "{}", stdout(&output));
    // Ready still looks only at draft and open change requests.
    let output = run(&p, &mock, &["cr", "ready"]);
    assert!(!output.status.success());
}

#[test]
fn open_without_claims_creates_a_linked_draft_before_code() {
    let p = checkout();
    let mock = server(vec![]);
    let output = run(
        &p,
        &mock,
        &[
            "cr",
            "open",
            "--openspec-change",
            "add-login",
            "--title",
            "Add login",
            "--json",
        ],
    );
    success(&output);
    let requests = mock.requests();
    assert_eq!(
        requests.len(),
        3,
        "no facts or claims without a claims file"
    );
    assert_eq!(
        requests[2].json(),
        json!({"title": "Add login", "source_branch": "feat/login", "openspec_change": "add-login"})
    );
    let event = last_event(&output);
    assert_eq!(event["change_request"]["number"], 7);
    assert_eq!(event["claims"]["status"], "missing");

    // Without a claims file the title is required.
    let untitled = run(&p, &server(vec![]), &["cr", "open"]);
    assert_eq!(untitled.status.code(), Some(1));
    assert!(
        stderr(&untitled).contains("pass --title"),
        "{}",
        stderr(&untitled)
    );
}

#[test]
fn open_reuses_the_branch_change_request_and_refuses_ambiguity() {
    let p = checkout();
    let reuse = server(vec![cr(3, "draft", Some("add-login"))]);
    let output = run(
        &p,
        &reuse,
        &["cr", "open", "--openspec-change", "add-login", "--json"],
    );
    success(&output);
    assert!(
        reuse.requests().iter().all(|r| r.method == "GET"),
        "nothing created"
    );
    assert_eq!(last_event(&output)["created"], false);
    assert_eq!(last_event(&output)["change_request"]["number"], 3);

    let other = run(
        &p,
        &server(vec![cr(3, "draft", Some("add-login"))]),
        &["cr", "open", "--openspec-change", "add-export"],
    );
    assert_eq!(other.status.code(), Some(1));
    assert!(
        stderr(&other).contains("links OpenSpec change add-login, not add-export"),
        "{}",
        stderr(&other)
    );

    let several = run(
        &p,
        &server(vec![cr(3, "draft", None), cr(5, "open", None)]),
        &["cr", "open", "--title", "x"],
    );
    assert_eq!(several.status.code(), Some(1));
    assert!(
        stderr(&several).contains("(#3, #5)"),
        "{}",
        stderr(&several)
    );
}

#[test]
fn claims_for_another_head_are_refused_before_anything_is_sent() {
    let p = checkout();
    std::fs::write(
        p.dir.join("cr.md"),
        claims_file(&"a".repeat(40)).replacen("cr: null", "cr: 7", 1),
    )
    .unwrap();
    let mock = server(vec![cr(7, "draft", None)]);
    let output = run(&p, &mock, &["cr", "claims", "cr.md"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains("but HEAD is"),
        "{}",
        stderr(&output)
    );
    assert!(mock.requests().is_empty(), "nothing was sent");
}

#[test]
fn server_errors_name_the_command_and_status() {
    let p = checkout();
    let mock = Mock::start(|_, _| {
        Reply::Status(
            409,
            vec![],
            r#"{"errors":{"detail":"invalid transition"}}"#.into(),
        )
    });
    let output = run(&p, &mock, &["cr", "ready", "-n", "4", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let event = last_event(&output);
    assert_eq!(
        event["message"],
        r#"cr ready failed: conflict (HTTP 409): {"errors":{"detail":"invalid transition"}}"#
    );
    assert_eq!(
        mock.requests()[0].path,
        "/api/v1/projects/goodcases/change-requests/4/ready"
    );

    let missing = p
        .command()
        .args(["cr", "status", "-n", "4"])
        .output()
        .unwrap();
    assert_eq!(missing.status.code(), Some(1));
    assert!(
        stderr(&missing).contains("GOODFLOWS_URL and GOODFLOWS_API_KEY"),
        "{}",
        stderr(&missing)
    );
}

const SET: &str = "---\r\nformat: qa-scenarios/2\r\nsource: intent\r\nchange: add-login\r\nspecs: uncommitted\r\nreview: null\r\nscenarios:\r\n  - {id: 1, name: Sign in, priority: must}\r\n  - {id: 2, name: Wrong password, priority: should}\r\n---\r\n# QA: add-login  \r\n\r\n## Scenarios\r\n\r\n### 1. Sign in (Must)\r\n\r\n1. Click **Sign in**.\r\n\r\n### 2. Wrong password (Should)\r\n\r\n1. Enter `x`.\r\n";

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn scenario_sets_are_uploaded_byte_for_byte_with_section_digests() {
    let p = checkout();
    std::fs::write(p.dir.join("qa.md"), SET).unwrap();
    let digest = sha256(SET.as_bytes());
    let reply = json!({"data": {"version": 1, "digest": digest, "source": "intent"}});
    let mock = Mock::start(move |r, _| {
        if r.method == "GET" && r.path.contains("state=draft") {
            ok(json!({"data": [cr(7, "draft", Some("add-login"))], "next_cursor": null}))
        } else if r.method == "GET" {
            ok(json!({"data": [], "next_cursor": null}))
        } else {
            created(reply.clone())
        }
    });
    let output = run(&p, &mock, &["cr", "scenarios", "upload", "qa.md", "--json"]);
    success(&output);
    let upload = mock.requests().pop().unwrap();
    assert_eq!(
        upload.path,
        "/api/v1/projects/goodcases/change-requests/7/scenario-sets"
    );
    let body = upload.json();
    assert_eq!(
        body["content"].as_str().unwrap(),
        SET,
        "CRLF and trailing spaces kept"
    );
    let one = &SET[SET.find("### 1.").unwrap()..SET.find("### 2.").unwrap()];
    let two = &SET[SET.find("### 2.").unwrap()..];
    assert_eq!(
        body["sections"],
        json!([{"id": "1", "sha256": sha256(one.as_bytes())}, {"id": "2", "sha256": sha256(two.as_bytes())}])
    );
    assert_eq!(last_event(&output)["digest"], digest);

    // Approving a file approves its digest.
    let approve = Mock::start(|_, _| created(json!({"data": {"decision": "approve"}})));
    success(&run(
        &p,
        &approve,
        &["cr", "scenarios", "approve", "qa.md", "-n", "7"],
    ));
    let request = approve.requests().pop().unwrap();
    assert_eq!(
        request.path,
        "/api/v1/projects/goodcases/change-requests/7/scenario-sets/intent/approvals"
    );
    assert_eq!(request.json(), json!({"digest": sha256(SET.as_bytes())}));
}

#[test]
fn a_run_folder_uploads_as_verify_run_with_its_artifacts() {
    let p = checkout();
    let head = git(&p, &["rev-parse", "HEAD"]);
    let set_sha = sha256(SET.as_bytes());
    let folder = p.dir.join(".verify/web/20261119T143000Z-a1b2c3d");
    std::fs::create_dir_all(folder.join("shots")).unwrap();
    std::fs::write(folder.join("scenarios.md"), SET).unwrap();
    std::fs::write(folder.join("shots/01.png"), b"png bytes").unwrap();
    std::fs::write(
        folder.join("run.md"),
        format!("---\nformat: \"verify-run-notes/1\"\nset: {{path: qa.md, sha256: {set_sha}}}\ncommit: {head}\nbranch: feat/login\ndirty: false\nstarted_at: 2026-11-19T14:30:00Z\nfinished_at: 2026-11-19T14:35:00Z\nresults:\n  - {{id: 1, result: passed, evidence: [.verify/web/20261119T143000Z-a1b2c3d/shots/01.png]}}\n  - {{id: 2, result: not_run, reason: No seed data}}\nfeatures:\n  - {{id: sign-in, entry_point: /login, result: passed}}\n---\n# Run\n"),
    )
    .unwrap();
    let url = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
    let base = url.clone();
    let mock = Mock::start(move |r, _| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/api/v1/projects/goodcases/verify-runs") => {
            let body = r.json();
            created(
                json!({"data": {"run_id": body["run_id"], "status": "uploading", "uploads": [{
                    "artifact_id": body["artifacts"][0]["id"],
                    "upload_url": format!("{}/bucket/obj-1?sig=x", base.lock().unwrap()),
                    "upload_headers": {"content-type": "image/png", "x-amz-checksum-sha256": "abc"},
                }]}}),
            )
        }
        ("PUT", _) => Reply::Status(200, vec![], String::new()),
        ("POST", p) if p.ends_with("/commit") => ok(json!({"data": {"status": "final"}})),
        _ => ok(json!({"data": [], "next_cursor": null})),
    });
    *url.lock().unwrap() = mock.url.clone();
    let output = run(
        &p,
        &mock,
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
            "--json",
        ],
    );
    success(&output);
    let requests = mock.requests();
    let document = requests[0].json();
    assert_eq!(document["format"], "verify-run/1");
    assert_eq!(
        document["scenario_set"],
        json!({"digest": sha256(SET.as_bytes()), "source": "intent"})
    );
    assert_eq!(
        document["source"],
        json!({"commit": head, "branch": "feat/login", "dirty": false, "changed_files": []})
    );
    assert_eq!(
        document["results"],
        json!([
            {"id": "1", "result": "passed", "failed_step": null, "reason": null, "evidence": ["shots-01.png"]},
            {"id": "2", "result": "not_run", "failed_step": null, "reason": "No seed data", "evidence": []},
        ])
    );
    assert_eq!(
        document["artifacts"],
        json!([{"id": "shots-01.png", "path": "shots/01.png", "content_type": "image/png", "size": 9, "sha256": sha256(b"png bytes")}])
    );
    assert_eq!(
        document["features"],
        json!([{"id": "sign-in", "entry_point": "/login", "result": "passed"}])
    );
    let run_id = document["run_id"].as_str().unwrap();
    let put = &requests[1];
    assert_eq!(
        (put.method.as_str(), put.path.as_str()),
        ("PUT", "/bucket/obj-1?sig=x")
    );
    assert_eq!(put.body, b"png bytes");
    assert_eq!(put.headers["x-amz-checksum-sha256"], "abc");
    assert!(
        !put.headers.contains_key("authorization"),
        "the API key never goes to the object store"
    );
    assert_eq!(
        requests[2].path,
        format!("/api/v1/projects/goodcases/verify-runs/{run_id}/commit")
    );
    let event = last_event(&output);
    assert_eq!(event["status"], "final");
    assert_eq!(
        event["results"],
        json!({"passed": 1, "failed": 0, "not_run": 1})
    );

    // The same folder is the same run.
    let again = run(
        &p,
        &Mock::start(|r, _| match r.method.as_str() {
            "GET" => ok(json!({"data": [], "next_cursor": null})),
            _ => ok(json!({"data": {"status": "final"}})),
        }),
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
            "--json",
        ],
    );
    success(&again);
    assert_eq!(last_event(&again)["run_id"], run_id);

    // A result missing for a scenario of the set is refused locally.
    std::fs::write(folder.join("run.md"), format!("---\nformat: \"verify-run-notes/1\"\nset: {{path: qa.md, sha256: {set_sha}}}\ncommit: {head}\ndirty: false\nstarted_at: 2026-11-19T14:30:00Z\nfinished_at: 2026-11-19T14:35:00Z\nresults:\n  - {{id: 1, result: passed}}\n---\n")).unwrap();
    let partial = run(
        &p,
        &mock,
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
        ],
    );
    assert_eq!(partial.status.code(), Some(1));
    assert!(
        stderr(&partial).contains("missing: 2"),
        "{}",
        stderr(&partial)
    );

    // A stated set digest must be scenarios.md's.
    let notes = std::fs::read_to_string(folder.join("run.md")).unwrap();
    std::fs::write(
        folder.join("run.md"),
        notes.replace(&set_sha, &"0".repeat(64)),
    )
    .unwrap();
    let changed = run(
        &p,
        &mock,
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
        ],
    );
    assert_eq!(changed.status.code(), Some(1));
    assert!(
        stderr(&changed).contains("copy of the set changed"),
        "{}",
        stderr(&changed)
    );
}

#[test]
fn features_sync_uploads_feature_files_with_paths() {
    let p = checkout();
    let dir = p.dir.join(".agents/skills/verify-web/features");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("README.md"), "# Features\n").unwrap();
    std::fs::write(
        dir.join("sign-in.md"),
        "---\npaths: [\"lib/login/**\", \"lib/session.ex\"]\n---\n# Sign in\n",
    )
    .unwrap();
    std::fs::write(dir.join("api-keys.md"), "# API keys\n").unwrap();
    let mock = Mock::start(|_, _| created(json!({"data": {"version": 4}})));
    let output = run(&p, &mock, &["features", "sync", "--json"]);
    success(&output);
    let request = mock.requests().pop().unwrap();
    assert_eq!(request.path, "/api/v1/projects/goodcases/feature-index");
    assert_eq!(
        request.json(),
        json!({"features": [{"id": "sign-in", "title": "Sign in", "paths": ["lib/login/**", "lib/session.ex"]}]})
    );
    let event = last_event(&output);
    assert_eq!(event["version"], 4);
    assert_eq!(event["skipped"], json!(["api-keys"]));
}

#[test]
fn policy_set_sends_the_toml_file_as_json_after_the_current_version() {
    let p = checkout();
    std::fs::write(
        p.dir.join("policy.toml"),
        "[evidence]\nrequired_workflows = [\"ci\"]\n\n[[paths]]\nglob = \"app/priv/**\"\noutcome = \"needs_review\"\n",
    )
    .unwrap();
    let mock = Mock::start(|r, _| match r.method.as_str() {
        "GET" => ok(json!({"data": {"version": 3}})),
        _ => created(
            json!({"data": {"version": 4, "previous_version": 3, "policy": r.json()["policy"]}}),
        ),
    });
    let output = run(
        &p,
        &mock,
        &[
            "policy",
            "set",
            "policy.toml",
            "--note",
            "Require ci",
            "--json",
        ],
    );
    success(&output);
    let write = mock.requests().pop().unwrap();
    assert_eq!(
        (write.method.as_str(), write.path.as_str()),
        ("PUT", "/api/v1/projects/goodcases/review-policy")
    );
    assert_eq!(
        write.json(),
        json!({"previous_version": 3, "note": "Require ci", "policy": {
            "format": "review-policy/1",
            "evidence": {"required_workflows": ["ci"]},
            "paths": [{"glob": "app/priv/**", "outcome": "needs_review"}]}})
    );
    assert_eq!(last_event(&output)["written"], true);
}

#[test]
fn claims_path_follows_the_branch_or_the_detached_head() {
    let p = checkout();
    let output = p.run(&["cr", "path", "--json"]);
    success(&output);
    assert_eq!(
        last_event(&output)["path"],
        ".goodflows/change-requests/feat/login/cr.md"
    );
    git(&p, &["checkout", "-q", "--detach"]);
    let short = git(&p, &["rev-parse", "--short", "HEAD"]);
    let output = p.run(&["cr", "path", "--json"]);
    success(&output);
    assert_eq!(
        last_event(&output)["path"],
        format!(".goodflows/change-requests/.detached/{short}/cr.md")
    );
    assert_eq!(last_event(&output)["branch"], Value::Null);
}

#[test]
fn help_says_which_command_creates_and_which_updates() {
    let p = checkout();
    let top = stdout(&p.run(&["--help"]));
    assert!(top.contains("cr open"), "{top}");
    let help = stdout(&p.run(&["cr", "--help"]));
    assert!(
        help.contains("CREATE a change request from a claims (description) file"),
        "{help}"
    );
    assert!(
        help.contains("UPDATE a change request's description"),
        "{help}"
    );
}

#[test]
fn events_follow_advances_an_integer_cursor_and_restarts_after_410() {
    let p = checkout();
    let mock = Mock::start(|_, index| {
        match index {
        0 => ok(json!({"data": [{"id": 5, "type": "change_request_opened", "number": 7,
            "data": {}, "actor": null, "inserted_at": "2026-11-21T00:00:00Z"}], "next_cursor": 5})),
        1 => Reply::Status(
            410,
            vec![],
            json!({"errors": {"detail": "Events after this cursor were deleted; resync from current state"},
                   "latest_cursor": 90})
            .to_string(),
        ),
        // Ends the otherwise endless --follow.
        _ => Reply::Status(401, vec![], r#"{"errors":{"detail":"Unauthorized"}}"#.into()),
    }
    });
    let output = run(&p, &mock, &["events", "--after", "3", "--follow", "--json"]);
    assert_eq!(output.status.code(), Some(1));
    let paths: Vec<_> = mock.requests().into_iter().map(|r| r.path).collect();
    let prefix = "/api/v1/projects/goodcases/events";
    assert_eq!(
        paths,
        [
            format!("{prefix}?after=3&wait=30"),
            format!("{prefix}?after=5&wait=30"),
            format!("{prefix}?after=90&wait=30"),
        ]
    );
    let events: Vec<Value> = stdout(&output)
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(events[0]["events"][0]["id"], 5);
    assert_eq!(events[1]["skipped"], true);
    assert_eq!(events[1]["next_cursor"], 90);
    assert!(
        events[1]["note"].as_str().unwrap().contains("skipped"),
        "{}",
        events[1]
    );

    // Without --follow, a deleted cursor fails naming latest_cursor.
    let gone = Mock::start(|_, _| {
        Reply::Status(
            410,
            vec![],
            r#"{"errors":{"detail":"gone"},"latest_cursor":90}"#.into(),
        )
    });
    let once = run(&p, &gone, &["events", "--after", "3"]);
    assert_eq!(once.status.code(), Some(1));
    assert!(
        stderr(&once).contains("--after 90 (latest_cursor)"),
        "{}",
        stderr(&once)
    );
    assert_eq!(gone.requests().len(), 1);
}

#[test]
fn policy_set_skips_writing_the_current_document_again() {
    let p = checkout();
    std::fs::write(
        p.dir.join("policy.toml"),
        "[evidence]\nrequired_workflows = [\"ci\"]\n",
    )
    .unwrap();
    let current = json!({"data": {"version": 3, "previous_version": 2, "note": "Require ci",
        "policy": {"format": "review-policy/1", "evidence": {"required_workflows": ["ci"]}}}});
    let reply = current.clone();
    let mock = Mock::start(move |r, _| match r.method.as_str() {
        "GET" => ok(reply.clone()),
        _ => created(json!({"data": {"version": 4, "previous_version": 3}})),
    });
    let output = run(&p, &mock, &["policy", "set", "policy.toml", "--json"]);
    success(&output);
    assert_eq!(mock.requests().len(), 1, "no PUT");
    let event = last_event(&output);
    assert_eq!(event["written"], false);
    assert_eq!(event["policy"]["version"], 3);

    let text = run(
        &p,
        &mock,
        &["policy", "set", "policy.toml", "--note", "Require ci"],
    );
    success(&text);
    assert!(
        stdout(&text).contains("version 3 was already written"),
        "{}",
        stdout(&text)
    );
    assert_eq!(mock.requests().len(), 2, "still no PUT");

    // Another note is a new version.
    let noted = run(
        &p,
        &mock,
        &["policy", "set", "policy.toml", "--note", "Other"],
    );
    success(&noted);
    let write = mock.requests().pop().unwrap();
    assert_eq!(write.method, "PUT");
    assert_eq!(write.json()["previous_version"], 3);
}

#[test]
fn lease_acquire_names_the_holder_of_a_taken_slot() {
    let p = checkout();
    let mock = Mock::start(|_, _| {
        Reply::Status(
            409,
            vec![],
            json!({"errors": {"detail": "Another lease holds the merge slot", "error": "slot_taken",
                "holder": {"change_request": 3, "expires_at": "2026-11-21T10:10:00Z"}}})
            .to_string(),
        )
    });
    let output = run(
        &p,
        &mock,
        &["cr", "lease", "acquire", "-n", "7", "--base", "main"],
    );
    assert_eq!(output.status.code(), Some(1));
    let error = stderr(&output);
    assert!(
        error.contains("cr lease acquire failed: another lease holds the merge slot")
            && error.contains("change request #3 holds it until 2026-11-21T10:10:00Z"),
        "{error}"
    );
    let request = mock.requests().pop().unwrap();
    assert_eq!(
        request.path,
        "/api/v1/projects/goodcases/change-requests/7/merge-lease"
    );
    assert_eq!(request.json()["base"], git(&p, &["rev-parse", "main"]));
}

#[test]
fn runs_upload_retries_a_commit_the_server_cannot_check_yet() {
    let p = checkout();
    let head = git(&p, &["rev-parse", "HEAD"]);
    let folder = p.dir.join(".verify/web/20261119T143000Z-a1b2c3d");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("scenarios.md"), SET).unwrap();
    std::fs::write(
        folder.join("run.md"),
        format!("---\nformat: \"verify-run-notes/1\"\ncommit: {head}\nbranch: feat/login\ndirty: false\nstarted_at: 2026-11-19T14:30:00Z\nfinished_at: 2026-11-19T14:35:00Z\nresults:\n  - {{id: 1, result: passed}}\n  - {{id: 2, result: passed}}\n---\n"),
    )
    .unwrap();
    let commits = std::sync::Arc::new(std::sync::Mutex::new(0));
    let seen = commits.clone();
    let mock = Mock::start(move |r, _| match (r.method.as_str(), r.path.as_str()) {
        ("POST", "/api/v1/projects/goodcases/verify-runs") => created(
            json!({"data": {"run_id": r.json()["run_id"], "status": "uploading", "uploads": []}}),
        ),
        ("POST", p) if p.ends_with("/commit") => {
            let mut seen = seen.lock().unwrap();
            *seen += 1;
            if *seen < 3 {
                Reply::Status(
                    503,
                    vec![("retry-after".into(), "0".into())],
                    r#"{"errors":{"detail":"The object store is unavailable"}}"#.into(),
                )
            } else {
                ok(json!({"data": {"status": "final"}}))
            }
        }
        _ => ok(json!({"data": [], "next_cursor": null})),
    });
    let output = run(
        &p,
        &mock,
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
            "--json",
        ],
    );
    success(&output);
    assert_eq!(*commits.lock().unwrap(), 3);
    assert_eq!(last_event(&output)["status"], "final");

    // Past five retries the 503 is the failure.
    let down = Mock::start(|r, _| match r.method.as_str() {
        "POST" if r.path.ends_with("/commit") => Reply::Status(
            503,
            vec![("retry-after".into(), "0".into())],
            r#"{"errors":{"detail":"down"}}"#.into(),
        ),
        "POST" => created(
            json!({"data": {"run_id": r.json()["run_id"], "status": "uploading", "uploads": []}}),
        ),
        _ => ok(json!({"data": [], "next_cursor": null})),
    });
    let failed = run(
        &p,
        &down,
        &[
            "cr",
            "runs",
            "upload",
            ".verify/web/20261119T143000Z-a1b2c3d",
        ],
    );
    assert_eq!(failed.status.code(), Some(1));
    assert!(stderr(&failed).contains("HTTP 503"), "{}", stderr(&failed));
    let tries = down
        .requests()
        .iter()
        .filter(|r| r.path.ends_with("/commit"))
        .count();
    assert_eq!(tries, 6, "one try and five retries");
}

#[test]
fn claims_fetch_the_target_into_origin_in_a_single_branch_clone() {
    let p = checkout();
    let head = git(&p, &["rev-parse", "HEAD"]);
    let base = git(&p, &["rev-parse", "main"]);
    let origin = p.dir.join(".goodflows/origin.git");
    git(
        &p,
        &["clone", "-q", "--bare", ".", origin.to_str().unwrap()],
    );
    git(&p, &["remote", "add", "origin", origin.to_str().unwrap()]);
    // A single-branch clone fetches only its own branch by default, and has
    // neither main nor origin/main.
    git(
        &p,
        &[
            "config",
            "remote.origin.fetch",
            "+refs/heads/feat/login:refs/remotes/origin/feat/login",
        ],
    );
    git(&p, &["branch", "-q", "-D", "main"]);
    std::fs::write(
        p.dir.join("cr.md"),
        claims_file(&head).replacen("cr: null", "cr: 7", 1),
    )
    .unwrap();
    let mock = server(vec![cr(7, "draft", None)]);
    let output = run(&p, &mock, &["cr", "claims", "cr.md", "--json"]);
    success(&output);
    let facts = mock
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/git-facts"))
        .unwrap()
        .json();
    assert_eq!(facts["target"]["merge_base"], base);
}

#[test]
fn claims_fetch_the_target_first_and_explain_a_moved_merge_base() {
    let p = checkout();
    let head = git(&p, &["rev-parse", "HEAD"]);
    std::fs::write(
        p.dir.join("cr.md"),
        claims_file(&head).replacen("cr: null", "cr: 7", 1),
    )
    .unwrap();
    git(
        &p,
        &["remote", "add", "origin", "/nonexistent/goodflows-origin"],
    );
    let mock = server(vec![cr(7, "draft", None)]);
    let failed = run(&p, &mock, &["cr", "claims", "cr.md"]);
    assert_eq!(failed.status.code(), Some(1));
    assert!(
        stderr(&failed).contains("git fetch origin main failed")
            && stderr(&failed).contains("pass --no-fetch"),
        "{}",
        stderr(&failed)
    );
    assert!(
        mock.requests().iter().all(|r| r.method == "GET"),
        "no facts sent"
    );
    success(&run(&p, &mock, &["cr", "claims", "cr.md", "--no-fetch"]));

    let conflict = Mock::start(|r, _| match r.method.as_str() {
        "POST" => Reply::Status(
            409,
            vec![],
            r#"{"errors":{"detail":"Different Git facts are stored for this head and target"}}"#
                .into(),
        ),
        _ => ok(json!({"data": cr(7, "draft", None)})),
    });
    let output = run(&p, &conflict, &["cr", "claims", "cr.md", "--no-fetch"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stderr(&output).contains(&format!(
            "cr claims failed: Git facts for HEAD {} against main were already reported with a different merge base; make a new commit (or retarget) and report again",
            &head[..12]
        )),
        "{}",
        stderr(&output)
    );
}
