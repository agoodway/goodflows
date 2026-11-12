//! `goodflows skills install` and `goodflows mcp`, through the real binary.

mod common;
use common::*;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::Stdio;

/// `skills install` in `p` with only `vars` in the environment.
fn install(p: &Project, vars: &[(&str, &str)], args: &[&str]) -> std::process::Output {
    let mut command = p.command();
    command.env_clear().envs(vars.iter().copied());
    command
        .args(["skills", "install"])
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn skills_install_in_a_claude_code_session_writes_both_skills_under_claude() {
    let p = Project::new("");
    let output = install(&p, &[("CLAUDECODE", "1")], &[]);
    success(&output);
    for name in ["create-ci-skill", "maintain-ci-skill"] {
        let installed = p.read(&format!(".claude/skills/{name}/SKILL.md"));
        let repo = std::fs::read_to_string(format!("skills/{name}/SKILL.md")).unwrap();
        assert_eq!(installed, repo, "{name}");
    }
    assert!(!p.dir.join(".agents").exists());
    let text = stdout(&output);
    assert!(
        text.starts_with("Installed skills for claude in .claude/skills\n"),
        "{text}"
    );

    // Rerunning refreshes an edited copy.
    std::fs::write(
        p.dir.join(".claude/skills/create-ci-skill/SKILL.md"),
        "edited",
    )
    .unwrap();
    success(&install(&p, &[("CLAUDECODE", "1")], &[]));
    assert!(p
        .read(".claude/skills/create-ci-skill/SKILL.md")
        .starts_with("---\nname: create-ci-skill\n"));
}

#[test]
fn skills_install_harness_flag_unknown_fallback_and_ambiguity() {
    let p = Project::new("");
    success(&install(&p, &[("CLAUDECODE", "1")], &["--harness", "grok"]));
    assert!(p
        .dir
        .join(".grok/skills/maintain-ci-skill/SKILL.md")
        .exists());
    assert!(!p.dir.join(".claude").exists());

    let output = install(&p, &[], &["--json"]);
    success(&output);
    let event: Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(event["type"], "skills_installed");
    assert_eq!(event["directory"], ".agents/skills");
    assert_eq!(
        (&event["source"], &event["harnesses"]),
        (&json!("default"), &json!([]))
    );
    assert!(p
        .dir
        .join(".agents/skills/create-ci-skill/SKILL.md")
        .exists());

    let output = install(&p, &[("CLAUDECODE", "1"), ("AGENT", "amp")], &["--json"]);
    assert_eq!(output.status.code(), Some(2));
    let event: Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    assert_eq!(
        (&event["type"], &event["code"]),
        (&json!("error"), &json!("ambiguous_harness"))
    );

    let output = install(&p, &[], &["--harness", "gemini", "--config", "x.toml"]);
    assert_eq!(output.status.code(), Some(2));
    let output = install(&p, &[], &["--harness", "vim"]);
    assert_eq!(output.status.code(), Some(2));
}

#[test]
fn mcp_rejects_flags_it_does_not_use() {
    let p = Project::new("");
    for flag in [
        &["--json"][..],
        &["--env", "ci"],
        &["--jobs", "a"],
        &["--log-dir", "l"],
    ] {
        let output = p
            .command()
            .arg("mcp")
            .args(flag)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2), "{flag:?}");
    }
}

/// A `goodflows mcp` server in a project, spoken to over its stdio.
struct Client {
    child: std::process::Child,
    lines: std::io::Lines<BufReader<std::process::ChildStdout>>,
    next: u64,
}

impl Client {
    fn start(p: &Project) -> Self {
        let mut child = p
            .command()
            .arg("mcp")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let lines = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut client = Self {
            child,
            lines,
            next: 0,
        };
        let info = client.request(
            "initialize",
            json!({"protocolVersion":"2025-06-18", "capabilities":{}, "clientInfo":{"name":"test", "version":"1"}}),
        );
        assert_eq!(info["serverInfo"]["name"], "goodflows");
        client.send(json!({"jsonrpc":"2.0", "method":"notifications/initialized"}));
        client
    }

    fn send(&mut self, message: Value) {
        let stdin = self.child.stdin.as_mut().unwrap();
        writeln!(stdin, "{message}").unwrap();
        stdin.flush().unwrap();
    }

    fn request(&mut self, method: &str, params: Value) -> Value {
        self.next += 1;
        let id = self.next;
        self.send(json!({"jsonrpc":"2.0", "id":id, "method":method, "params":params}));
        loop {
            let line = self.lines.next().expect("server closed stdout").unwrap();
            let message: Value = serde_json::from_str(&line).unwrap();
            if message["id"] == id {
                return message["result"].clone();
            }
        }
    }

    /// The text blocks of a tool call's result, and whether it is an error.
    fn call(&mut self, tool: &str, arguments: Value) -> (Vec<String>, bool) {
        let result = self.request("tools/call", json!({"name":tool, "arguments":arguments}));
        let texts = result["content"]
            .as_array()
            .unwrap()
            .iter()
            .map(|block| block["text"].as_str().unwrap().to_owned())
            .collect();
        (texts, result["isError"] == true)
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

const CALLS: &str = "[[workflows.ci.jobs]]\nname = 'prep'\ncommand = 'echo PREP-OUTPUT'\n\
[[workflows.ci.jobs]]\nname = 'call-suite'\nworkflow = 'suite'\ndepends_on = ['prep']\n\
[[workflows.suite.jobs]]\nname = 'work'\n\
command = 'm=EARLY; echo $m-MARKER; head -c 20000 /dev/zero | tr \"\\\\0\" x; echo boom >&2; exit 3'\n\
[[workflows.ok.jobs]]\nname = 'fine'\ncommand = 'echo FINE-OUTPUT'\n";

#[test]
fn mcp_run_returns_only_the_summary_and_failures_including_called_workflows() {
    let p = Project::new(CALLS);
    let mut client = Client::start(&p);
    let tools = client.request("tools/list", json!({}));
    let mut names: Vec<_> = tools["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|tool| tool["name"].as_str().unwrap())
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["cancel", "last", "list", "run", "status", "validate", "wait"]
    );

    let (texts, error) = client.call("run", json!({"workflow":"ci"}));
    assert!(!error);
    let all = texts.join("\n");
    for absent in ["PREP-OUTPUT", "EARLY-MARKER", "job_output", "output_head"] {
        assert!(!all.contains(absent), "{absent}:\n{all}");
    }
    assert!(all.len() < 20_000, "{}", all.len());
    assert_eq!(texts[0], "GoodFlows checks failed (exit code 1).");
    let diagnostics: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(
        (&diagnostics["passed"], &diagnostics["exit_code"]),
        (&json!(false), &json!(1))
    );
    let results = diagnostics["results"].as_array().unwrap();
    let names: Vec<_> = results
        .iter()
        .map(|r| r["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["call-suite", "work"]);
    let called = &results[1]["called_run"];
    assert_eq!(
        (&called["workflow"], &called["caller_job"]),
        (&json!("suite"), &json!("call-suite"))
    );
    assert_eq!(called["parent_run_id"], diagnostics["run_id"]);
    assert_eq!(results[1]["exit_code"], 3);
    let logs = texts[2]
        .strip_prefix("Failure log excerpts (JSON):\n")
        .unwrap();
    let logs: Value = serde_json::from_str(logs).unwrap();
    assert_eq!(logs[1]["stderr_tail"], "boom\n");
    assert!(logs[1]["stdout_tail"].as_str().unwrap().ends_with("xxx"));
    assert_eq!(logs[1]["excerpts_truncated"], true);

    // `last` reports the same failures again without running.
    let (texts, error) = client.call("last", json!({}));
    assert!(!error);
    assert_eq!(texts[0], "The last GoodFlows run failed.");
    assert!(texts[1].contains("\"called_run\""));

    let (texts, error) = client.call(
        "run",
        json!({"workflow":"ok", "jobs":["fine"], "fail_fast":false}),
    );
    assert!(!error);
    assert_eq!(texts[0], "GoodFlows checks passed.");
    assert!(!texts.join("\n").contains("FINE-OUTPUT"));
}

#[test]
fn mcp_validate_and_list_report_and_config_errors_are_tool_errors() {
    let p = Project::new(CALLS);
    std::fs::write(
        p.dir.join("other.toml"),
        "[[jobs]]\nname = 'solo'\ncommand = 'true'\n",
    )
    .unwrap();
    let mut client = Client::start(&p);

    let (texts, error) = client.call("list", json!({"workflows":true}));
    assert!(!error);
    let event: Value = serde_json::from_str(&texts[0]).unwrap();
    assert_eq!(event["type"], "workflows");

    let (texts, error) = client.call("validate", json!({"workflow":"ci", "jobs":["nope"]}));
    assert!(error);
    assert!(
        texts[0].contains("exit code 2") && texts[0].contains("unknown job 'nope'"),
        "{texts:?}"
    );

    // A call's config replaces discovery, like --config.
    let (texts, error) = client.call("list", json!({"config":"other.toml"}));
    assert!(!error);
    assert!(texts[0].contains("\"solo\""), "{texts:?}");

    let (texts, error) = client.call("run", json!({"config":"missing.toml"}));
    assert!(error);
    assert!(texts[0].contains("exit code 2"), "{texts:?}");
}

/// `slow` waits for the file `release` (at most 30 seconds), then fails
/// with a message; `after` is skipped because it failed.
const SLOW: &str = "[settings]\nfail_fast = false\n\
[[jobs]]\nname = 'prep'\ncommand = 'true'\n\
[[jobs]]\nname = 'slow'\ndepends_on = ['prep']\n\
command = 'touch started; for i in $(seq 600); do test -f release && break; sleep 0.05; done; echo broken >&2; exit 1'\n\
[[jobs]]\nname = 'after'\ncommand = 'true'\ndepends_on = ['slow']\n";

/// A `run` or `wait` result's blocks, each JSON block without what differs
/// between two runs: identity, start time, host and timings.
fn stable(texts: &[String]) -> Vec<Value> {
    texts
        .iter()
        .map(|text| {
            let text = text
                .strip_prefix("Failure log excerpts (JSON):\n")
                .unwrap_or(text);
            let mut value = serde_json::from_str(text).unwrap_or_else(|_| json!(text));
            if let Some(object) = value.as_object_mut() {
                for key in [
                    "run_id",
                    "started_at",
                    "duration_ms",
                    "execute_ms",
                    "max_in_flight",
                    "host",
                ] {
                    object.remove(key);
                }
            }
            value
        })
        .collect()
}

#[test]
fn mcp_detached_run_polled_with_wait_gives_the_blocking_result() {
    let p = Project::new(SLOW);
    let mut client = Client::start(&p);
    std::fs::write(p.dir.join("release"), "").unwrap();
    let (blocking, error) = client.call("run", json!({}));
    assert!(!error, "{blocking:?}");
    assert_eq!(blocking[0], "GoodFlows checks failed (exit code 1).");
    for file in ["release", "started"] {
        std::fs::remove_file(p.dir.join(file)).unwrap();
    }

    let (texts, error) = client.call("run", json!({"detach":true}));
    assert!(!error, "{texts:?}");
    let event: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(event["type"], "run_detached");
    let id = event["run_id"].as_str().unwrap().to_owned();
    assert!(texts[0].contains(&id), "{texts:?}");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !p.dir.join("started").exists() {
        assert!(std::time::Instant::now() < deadline, "slow never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }

    // A short wait while it runs is a normal result, not an error.
    let (texts, error) = client.call("wait", json!({"run_id":id, "timeout":1}));
    assert!(!error, "{texts:?}");
    assert_eq!(
        texts[0],
        "The GoodFlows run is still running; call `wait` again."
    );
    let status: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(status["running"], json!(["slow"]));
    let (texts, error) = client.call("status", json!({"run_id":id}));
    assert!(!error, "{texts:?}");
    assert_eq!(texts[0], "The GoodFlows run is still running.");

    std::fs::write(p.dir.join("release"), "").unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    let done = loop {
        let (texts, error) = client.call("wait", json!({"run_id":id, "timeout":1}));
        assert!(!error, "{texts:?}");
        if !texts[0].contains("still running") {
            break texts;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the run never finished"
        );
    };
    assert_eq!(stable(&done), stable(&blocking));

    // Cancel: an unknown id is a tool error; a stopped run says so.
    let (texts, error) = client.call("cancel", json!({"run_id":id}));
    assert!(!error, "{texts:?}");
    assert_eq!(texts[0], "The GoodFlows run finished (exit code 1).");
    let event: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(event["was_running"], false);
    let unknown = "00000000-0000-4000-8000-000000000000";
    let (texts, error) = client.call("wait", json!({"run_id":unknown}));
    assert!(error && texts[0].contains("exit code 2"), "{texts:?}");
}

#[test]
fn mcp_detached_run_of_a_named_workflow_starts_it() {
    // The tool passes the workflow after `--`.
    let p = Project::new(CALLS);
    let mut client = Client::start(&p);
    let (texts, error) = client.call("run", json!({"workflow":"ok", "detach":true}));
    assert!(!error, "{texts:?}");
    let id = serde_json::from_str::<Value>(&texts[1]).unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let (texts, error) = client.call("wait", json!({"run_id":id, "timeout":30}));
    assert!(!error, "{texts:?}");
    assert_eq!(texts[0], "GoodFlows checks passed.");
    let diagnostics: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(diagnostics["workflow"], "ok");
}

#[test]
fn mcp_cancel_stops_a_detached_run() {
    let p = Project::new(SLOW);
    let mut client = Client::start(&p);
    let (texts, error) = client.call("run", json!({"detach":true}));
    assert!(!error, "{texts:?}");
    let id = serde_json::from_str::<Value>(&texts[1]).unwrap()["run_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while !p.dir.join("started").exists() {
        assert!(std::time::Instant::now() < deadline, "slow never started");
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let (texts, error) = client.call("cancel", json!({"run_id":id}));
    assert!(!error, "{texts:?}");
    assert_eq!(texts[0], "The GoodFlows run finished (exit code 143).");
    let event: Value = serde_json::from_str(&texts[1]).unwrap();
    assert_eq!(event["was_running"], true);
    assert_eq!(event["cancelled"], json!(["slow", "after"]));
    let (texts, _) = client.call("wait", json!({"run_id":id}));
    assert_eq!(texts[0], "GoodFlows checks failed (exit code 143).");
}

#[test]
fn agents_prints_a_guide_with_every_documented_exit_and_error_code() {
    let p = Project::new("");
    let output = p.run(&["agents"]);
    success(&output);
    let guide = stdout(&output);
    assert!(
        guide.starts_with(&format!("goodflows {}: ", env!("CARGO_PKG_VERSION"))),
        "{guide}"
    );
    // The README's exit code and error code tables: each row's first cell.
    let readme = std::fs::read_to_string("README.md").unwrap();
    let mut codes = Vec::new();
    for line in readme.lines() {
        let Some(cell) = line.strip_prefix("| ").and_then(|l| l.split(" |").next()) else {
            continue;
        };
        if cell.chars().all(|c| c.is_ascii_digit()) {
            codes.push(format!("`{cell}`"));
        } else if line.ends_with('|')
            && cell.starts_with('`')
            && line
                .split(" | ")
                .nth(1)
                .is_some_and(|exit| exit == "1" || exit == "2")
        {
            codes.push(cell.to_owned());
        }
    }
    assert!(codes.contains(&"`75`".to_owned()), "{codes:?}");
    assert!(codes.contains(&"`no_last_run`".to_owned()), "{codes:?}");
    for code in &codes {
        assert!(guide.contains(code.as_str()), "{code} is not in the guide");
    }
    assert_eq!(codes.len(), 6 + 13, "{codes:?}");
    // `skills install` documents this one outside the table.
    assert!(guide.contains("`ambiguous_harness`"));
    for flag in [&["--json"][..], &["--config", "x.toml"], &["--jobs", "a"]] {
        let output = p.run(&[&["agents"], flag].concat());
        assert_eq!(output.status.code(), Some(2), "{flag:?}");
    }
}
