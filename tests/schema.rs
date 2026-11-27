//! `goodflows schema`, through the real binary: every template it prints
//! passes the validator it names once its placeholders are filled.

mod common;
use common::*;
use serde_json::Value;
use sha2::{Digest, Sha256};

/// `schema show` as JSON, in a project without a config or API key.
fn show(p: &Project, args: &[&str]) -> Value {
    let output = p.run(&[&["schema", "show"], args, &["--json"]].concat());
    success(&output);
    let text = stdout(&output);
    assert_eq!(text.lines().count(), 1, "{text}");
    serde_json::from_str(&text).unwrap()
}

fn sha256(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// The template with each `[placeholder]` (a `[` then a letter) filled in
/// as its wording asks; YAML lists such as `[]` and `["a.png"]` stay.
fn fill(template: &str, scenarios_sha256: &str) -> String {
    let mut out = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('[') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let end = after
            .find(']')
            .filter(|_| after.starts_with(|c: char| c.is_ascii_alphabetic()));
        let Some(end) = end else {
            out.push('[');
            rest = after;
            continue;
        };
        let placeholder = after[..end].to_lowercase();
        out.push_str(if placeholder.contains("sha-256") {
            scenarios_sha256
        } else if placeholder.contains("short sha") {
            "a1b2c3d"
        } else if placeholder.contains("sha") {
            "0123456789abcdef0123456789abcdef01234567"
        } else if placeholder.contains("rfc 3339") {
            "2026-11-19T14:30:00Z"
        } else {
            "filled-text"
        });
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    assert!(!out.contains("filled-text]"), "{out}");
    out
}

fn template(event: &Value) -> &str {
    event["template"].as_str().unwrap()
}

#[test]
fn every_filled_template_passes_the_validator_it_names() {
    let p = Project::new("");
    std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
    let validate = |args: &[&str]| {
        let output = p.run(args);
        assert_eq!(
            output.status.code(),
            Some(0),
            "{args:?}\n{}{}",
            stdout(&output),
            stderr(&output)
        );
    };

    let mut sets = Vec::new();
    for source in ["intent", "diff"] {
        let event = show(&p, &["qa-scenarios/2", "--source", source]);
        assert_eq!(event["validate"], "goodflows cr validate <file>");
        let set = fill(template(&event), "");
        let file = format!("{source}-qa-scenarios.md");
        std::fs::write(p.dir.join(&file), &set).unwrap();
        validate(&["cr", "validate", &file]);
        sets.push(set);
    }

    let claims = show(&p, &["change-claims/2"]);
    std::fs::write(p.dir.join("cr.md"), fill(template(&claims), "")).unwrap();
    validate(&["cr", "validate", "cr.md"]);
    // Nor does the server-only part of the claims format warn.
    let output = p.run(&["cr", "validate", "cr.md", "--json"]);
    let event: Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    let warnings = &event["files"][0]["warnings"];
    assert!(
        warnings
            .as_array()
            .unwrap()
            .iter()
            .all(|w| w.as_str().unwrap().contains("HEAD")),
        "{warnings}"
    );

    // The run notes grade the filled diff set, whose ids they list.
    let notes = show(&p, &["verify-run-notes/1"]);
    assert_eq!(notes["validate"], "goodflows cr validate <run folder>");
    let run = p.dir.join(".verify/web/20261119T143000Z-a1b2c3d");
    std::fs::create_dir_all(&run).unwrap();
    std::fs::write(run.join("scenarios.md"), &sets[1]).unwrap();
    std::fs::write(
        run.join("run.md"),
        fill(template(&notes), &sha256(&sets[1])),
    )
    .unwrap();
    for evidence in ["01-scenario-1.png", "02-scenario-2-step-3.png"] {
        std::fs::write(run.join(evidence), "png").unwrap();
    }
    validate(&["cr", "validate", ".verify/web/20261119T143000Z-a1b2c3d"]);

    let feature = show(&p, &["feature-file/1"]);
    assert_eq!(feature["validate"], "goodflows features validate");
    let dir = p.dir.join(".agents/skills/verify-web/features");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("login.md"), fill(template(&feature), "")).unwrap();
    validate(&[
        "features",
        "validate",
        "--dir",
        ".agents/skills/verify-web/features",
    ]);
}

#[test]
fn list_and_show_need_no_config_and_check_names() {
    let p = Project::new("");
    std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
    let output = p.run(&["schema", "list", "--json"]);
    success(&output);
    let event: Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    let formats: Vec<_> = event["formats"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["format"].as_str().unwrap(),
                f["validate"].as_str().unwrap(),
            )
        })
        .collect();
    assert_eq!(
        formats,
        [
            ("qa-scenarios/2", "goodflows cr validate <file>"),
            ("verify-run-notes/1", "goodflows cr validate <run folder>"),
            ("change-claims/2", "goodflows cr validate <file>"),
            ("feature-file/1", "goodflows features validate"),
            (
                "review-policy/1",
                "goodflows policy set <file> (the server checks it and stores a version only when it passes)"
            ),
        ]
    );

    let event = show(&p, &["qa-scenarios", "--source", "intent"]);
    assert_eq!(
        (&event["type"], &event["format"], &event["source"]),
        (&"schema".into(), &"qa-scenarios/2".into(), &"intent".into())
    );
    for key in ["fields", "body", "handling"] {
        assert!(event[key].as_array().unwrap().iter().all(Value::is_string));
    }

    // Every variant has rules in each section, as JSON and as text.
    let text = stdout(&p.run(&["schema", "list"]));
    for (args, file) in [
        (
            &["qa-scenarios/2", "--source", "intent"][..],
            "qa-scenarios.md",
        ),
        (&["qa-scenarios/2", "--source", "diff"], "qa-scenarios.md"),
        (&["verify-run-notes/1"], "run.md"),
        (&["change-claims/2"], "cr.md"),
        (&["feature-file/1"], "features/<feature>.md"),
        (&["review-policy/1"], "goodflows/review-policy.toml"),
    ] {
        assert!(
            text.contains(&format!("{}\n", args[0])) || text.contains(&format!("{}  (", args[0])),
            "{text}"
        );
        assert!(text.contains(file), "{file}: {text}");
        let validate = show(&p, args)["validate"].as_str().unwrap().to_owned();
        assert!(
            text.contains(&format!("validate: {validate}\n")),
            "{validate}: {text}"
        );
        let event = show(&p, args);
        for key in ["fields", "body", "handling"] {
            assert!(!event[key].as_array().unwrap().is_empty(), "{args:?} {key}");
        }
        let output = p.run(&[&["schema", "show"], args].concat());
        success(&output);
        let shown = stdout(&output);
        let rules = |title: &str| {
            let section = shown
                .split(&format!("\n{title}:\n"))
                .nth(1)
                .unwrap_or_default();
            section.lines().take_while(|l| l.starts_with("- ")).count()
        };
        for (title, key) in [
            ("Fields", "fields"),
            ("Body", "body"),
            ("Handling", "handling"),
        ] {
            assert_eq!(
                rules(title),
                event[key].as_array().unwrap().len(),
                "{args:?} {title}\n{shown}"
            );
        }
        assert!(
            shown.contains(&format!(
                "```{}\n{}```\n",
                event["syntax"].as_str().unwrap(),
                template(&event)
            )),
            "{shown}"
        );
        assert!(shown.ends_with(&format!(
            "\nValidate: {}\n",
            event["validate"].as_str().unwrap()
        )));
    }

    for (args, needle) in [
        (&["qa-scenarios/2"][..], "--source intent or --source diff"),
        (
            &["qa-scenarios/1", "--source", "diff"],
            "only qa-scenarios/2",
        ),
        (&["nope"], "unknown format nope"),
        (&["change-claims", "--source", "diff"], "no variants"),
    ] {
        let output = p.run(&[&["schema", "show"], args].concat());
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert_eq!(stdout(&output), "", "{args:?}");
        assert!(
            stderr(&output).contains(needle),
            "{args:?}: {}",
            stderr(&output)
        );
    }
}

#[test]
fn the_policy_template_is_version_0_with_every_key_the_server_accepts() {
    // Version 0 and the allowed keys as goodflows_web's review-policy spec
    // states them; the server, not this CLI, refuses any other key.
    let version_0: toml::Value = toml::from_str(
        r#"
format = "review-policy/1"
[scenarios]
require_approved_intent = false
should_failures = "needs_review"
spec_gaps = "ignore"
approver = "any"
diff_only = "needs_review"
[evidence]
required_workflows = []
ignore_paths = ["docs/**", "**/*.md"]
[claims]
one_way_door = "needs_review"
min_proof_level = "ran"
open_items = "needs_review"
[size]
max_changed_files = 40
max_changed_lines = 1500
[coverage]
unmapped_areas = "ignore"
undriven_features = "ignore"
[merge]
lease_ttl = "10m"
regrade_merge_result = true
"#,
    )
    .unwrap();
    let p = Project::new("");
    let event = show(&p, &["review-policy"]);
    assert_eq!(
        (&event["format"], &event["syntax"]),
        (&"review-policy/1".into(), &"toml".into())
    );
    let printed: toml::Value = toml::from_str(template(&event)).unwrap();
    assert_eq!(printed, version_0);
    // Every key, and paths, which version 0 leaves empty, has a field rule.
    let fields = event["fields"].as_array().unwrap();
    let rules = fields
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    for (section, keys) in version_0.as_table().unwrap() {
        for key in keys.as_table().into_iter().flat_map(|t| t.keys()) {
            assert!(
                rules.contains(&format!("{section}.{key}")),
                "{section}.{key}"
            );
        }
    }
    assert!(rules.contains("[[paths]] entries of only glob and outcome"));
}

/// `skills install --json` in `p` with only `vars` in the environment.
fn install(p: &Project, vars: &[(&str, &str)]) -> Value {
    let mut command = p.command();
    command
        .env_clear()
        .envs(vars.iter().copied())
        .args(["skills", "install", "--json"]);
    let output = command.output().unwrap();
    success(&output);
    serde_json::from_str::<Value>(stdout(&output).trim()).unwrap()
}

/// The guidance files an install event reports, after the skill files.
fn guidance(event: &Value) -> Vec<&str> {
    event["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|f| !f.contains("/skills/"))
        .collect()
}

#[test]
fn skills_install_writes_the_formats_block_and_keeps_the_rest_of_agents_md() {
    let p = Project::new("");
    assert_eq!(guidance(&install(&p, &[])), ["AGENTS.md"]);
    let block = p.read("AGENTS.md");
    assert!(block.starts_with("<!-- goodflows:formats -->\n"), "{block}");
    assert!(block.ends_with("<!-- /goodflows:formats -->\n"), "{block}");
    for name in [
        "goodflows schema show",
        "qa-scenarios/2",
        "verify-run-notes/1",
        "change-claims/2",
        "feature-file/1",
    ] {
        assert!(block.contains(name), "{name}");
    }
    // Without Claude Code, no CLAUDE.md; an unchanged AGENTS.md is not reported.
    assert!(!p.dir.join("CLAUDE.md").exists());
    assert_eq!(guidance(&install(&p, &[])), Vec::<&str>::new());

    let edited = format!(
        "# Project rules\n\n{}More rules\n",
        block.replace("File formats", "Stale heading")
    );
    std::fs::write(p.dir.join("AGENTS.md"), edited).unwrap();
    assert_eq!(guidance(&install(&p, &[])), ["AGENTS.md"]);
    assert_eq!(
        p.read("AGENTS.md"),
        format!("# Project rules\n\n{block}More rules\n")
    );

    // A start line with no end line is the project's text: no run deletes
    // what follows it.
    let stray = "<!-- goodflows:formats -->\nKeep this line.\n";
    std::fs::write(p.dir.join("AGENTS.md"), stray).unwrap();
    install(&p, &[]);
    let once = p.read("AGENTS.md");
    assert_eq!(once, format!("{stray}\n{block}"));
    assert_eq!(guidance(&install(&p, &[])), Vec::<&str>::new());
    assert_eq!(p.read("AGENTS.md"), once);
}

#[test]
fn skills_install_for_claude_code_puts_the_block_in_claude_md() {
    let p = Project::new("");
    std::fs::write(p.dir.join("CLAUDE.md"), "# Claude rules\n").unwrap();
    let event = install(&p, &[("CLAUDECODE", "1")]);
    assert_eq!(guidance(&event), ["AGENTS.md", "CLAUDE.md"]);
    let block = p.read("AGENTS.md");
    assert_eq!(p.read("CLAUDE.md"), format!("# Claude rules\n\n{block}"));

    // A CLAUDE.md that imports AGENTS.md already sees the block.
    let q = Project::new("");
    std::fs::write(q.dir.join("CLAUDE.md"), "@AGENTS.md\n").unwrap();
    let event = install(&q, &[]);
    assert_eq!(guidance(&event), ["AGENTS.md"]);
    let event = q
        .command()
        .env_clear()
        .args(["skills", "install", "--json", "--harness", "claude"])
        .output()
        .unwrap();
    success(&event);
    assert_eq!(q.read("CLAUDE.md"), "@AGENTS.md\n");
}

#[test]
fn the_agent_guide_points_at_schema_list() {
    let output = Project::new("").run(&["agents"]);
    success(&output);
    let guide = stdout(&output);
    assert!(guide.contains("## File formats: `goodflows schema`"));
    assert!(guide.contains("`goodflows schema list`"));
    assert!(guide.contains("`goodflows schema show <format>`"));

    // The installed skills defer to the schema and keep no template of their own.
    let p = Project::new("");
    install(&p, &[]);
    for (skill, command, template) in [
        (
            "gf-change-write",
            "goodflows schema show qa-scenarios/2 --source intent",
            "format: qa-scenarios/2",
        ),
        (
            "gf-change-request",
            "goodflows schema show change-claims/2",
            "format: change-claims/2",
        ),
    ] {
        let text = p.read(&format!(".agents/skills/{skill}/SKILL.md"));
        assert!(text.contains(command), "{skill}");
        assert!(!text.contains(template), "{skill}");
    }
}

#[test]
fn cr_validate_warns_about_server_only_claims_rules_without_failing() {
    let p = Project::new("");
    std::fs::remove_file(p.dir.join("goodflows.toml")).unwrap();
    std::fs::write(
        p.dir.join("cr.md"),
        "---\nformat: change-claims/2\ncr: null\nhead: abc\ndoor: maybe\ndoor_reason: r\nnote: x\nsafe_because:\n  - {fact: f, location: a:1, proof: ran}\nleft_open: []\nquestions:\n  - {id: \"1\", text: q, options: [a, b], recommendation: c}\ndecisions: []\n---\n# T\n",
    )
    .unwrap();
    let output = p.run(&["cr", "validate", "cr.md", "--json"]);
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    let event: Value = serde_json::from_str(stdout(&output).trim()).unwrap();
    let file = &event["files"][0];
    assert_eq!(
        (&file["ok"], &file["errors"]),
        (&Value::Bool(true), &serde_json::json!([]))
    );
    let warnings: Vec<&str> = file["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .filter(|w| w.contains("so the server refuses the upload"))
        .collect();
    for needle in [
        "has no reach",
        "note is not a claims key",
        "door must be",
        "proof: ran and no evidence",
        "recommendation must be one of its options",
    ] {
        assert_eq!(
            warnings.iter().filter(|w| w.contains(needle)).count(),
            1,
            "{needle}: {warnings:?}"
        );
    }
    assert_eq!(warnings.len(), 5, "{warnings:?}");
}
