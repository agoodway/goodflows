//! Text rendering of change request, policy, feature index and event
//! commands. Every value comes from the server or a local file and is
//! untrusted: strings pass through [`visible`].

use super::visible;
use anyhow::Result;
use serde_json::Value;
use std::io::Write;

fn s(value: &Value) -> String {
    match value {
        Value::String(text) => visible(text).into_owned(),
        Value::Null => "-".to_owned(),
        other => visible(&other.to_string()).into_owned(),
    }
}

fn short(value: &Value) -> String {
    value.as_str().map_or_else(
        || "-".to_owned(),
        |sha| visible(&sha[..sha.len().min(12)]).into_owned(),
    )
}

/// Whether this module renders events of this type.
pub fn renders(kind: &str) -> bool {
    matches!(
        kind,
        "change_request"
            | "change_request_verdict"
            | "change_request_path"
            | "change_request_tasks"
            | "change_request_task"
            | "scenario_set"
            | "scenario_sets"
            | "scenario_decision"
            | "scenario_changes"
            | "verify_run"
            | "review_packet"
            | "review_answer"
            | "review_policy"
            | "review_policy_versions"
            | "calibration"
            | "feature_index"
            | "cr_validation"
            | "feature_validation"
            | "change_validation"
            | "change_archive"
            | "change_push"
            | "change_list"
            | "change_show"
            | "change_pull"
            | "change_versions"
            | "change_delete"
            | "project_events"
            | "merge_lease"
            | "defect"
    )
}

pub fn render(event: &Value, out: &mut impl Write) -> Result<()> {
    match event["type"].as_str().unwrap_or_default() {
        "change_request" => change_request(event, out)?,
        "change_request_verdict" => {
            verdict(&event["verdict"], out)?;
            let head = &event["verdict"]["head"];
            if event["local_head"].is_string() && !head.is_null() && event["local_head"] != *head {
                writeln!(
                    out,
                    "Local HEAD {} is not the reported head; run goodflows cr push or goodflows cr claims.",
                    short(&event["local_head"])
                )?;
            }
            if event["timed_out"] == true {
                writeln!(
                    out,
                    "Timed out before the verdict reached {}.",
                    list(&event["until"])
                )?;
            }
        }
        "change_request_path" => writeln!(out, "{}", s(&event["absolute_path"]))?,
        "scenario_set" => writeln!(
            out,
            "{} {} set of change request #{} as version {} (digest {})",
            if event["duplicate"] == true {
                "Already stored:"
            } else {
                "Uploaded"
            },
            s(&event["source"]),
            event["number"],
            s(&event["version"]),
            s(&event["digest"])
        )?,
        "scenario_decision" => {
            let decision = &event["decision"];
            let verb = if decision["decision"] == "reject" {
                "Rejected"
            } else {
                "Approved"
            };
            writeln!(
                out,
                "{verb} intent set digest {} of change request #{}{}",
                s(&event["digest"]),
                event["number"],
                if event["repeated"] == true {
                    " (already recorded)"
                } else {
                    ""
                }
            )?;
            if let Some(note) = decision["note"].as_str() {
                writeln!(out, "  note: {}", visible(note))?;
            }
        }
        "scenario_changes" => scenario_changes(event, out)?,
        "scenario_sets" => scenario_sets(event, out)?,
        "verify_run" => {
            let r = &event["results"];
            writeln!(
                out,
                "Verify run {}: {}{}  passed {}, failed {}, not run {}; {} artifacts{}",
                s(&event["run_id"]),
                s(&event["status"]),
                if event["duplicate"] == true {
                    " (already stored)"
                } else {
                    ""
                },
                r["passed"],
                r["failed"],
                r["not_run"],
                event["artifacts"],
                if event["dirty"] == true {
                    "; dirty tree, so it never counts"
                } else {
                    ""
                }
            )?;
            writeln!(
                out,
                "  commit {}  {} set {}",
                short(&event["commit"]),
                s(&event["scenario_set"]["source"]),
                s(&event["scenario_set"]["digest"])
            )?;
            if event["verdict"].is_object() {
                write!(out, "Change request #{}: ", event["number"])?;
                verdict(&event["verdict"], out)?;
            }
        }
        "feature_index" => {
            writeln!(
                out,
                "Feature index version {}: {} features ({})",
                s(&event["version"]),
                event["features"].as_array().map_or(0, Vec::len),
                list(&event["features"])
            )?;
            if event["skipped"].as_array().is_some_and(|a| !a.is_empty()) {
                writeln!(
                    out,
                    "Skipped, no paths in front matter: {}",
                    list(&event["skipped"])
                )?;
            }
        }
        "cr_validation" | "change_validation" => validation(&event["files"], &[], out)?,
        "change_archive" => {
            writeln!(
                out,
                "Archived change {} to {}",
                s(&event["id"]),
                s(&event["archived_to"])
            )?;
            for spec in event["specs"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "  {}{}: {} added, {} modified, {} removed, {} renamed",
                    s(&spec["path"]),
                    if spec["created"] == true {
                        " (new)"
                    } else {
                        ""
                    },
                    spec["added"],
                    spec["modified"],
                    spec["removed"],
                    spec["renamed"]
                )?;
            }
        }
        "change_push" => {
            let unchanged = event["status"] == "unchanged";
            write!(
                out,
                "{} change {} as version {}",
                if unchanged {
                    "Already stored, unchanged:"
                } else {
                    "Pushed"
                },
                s(&event["id"]),
                s(&event["version"])
            )?;
            if !unchanged && !event["replaced_version"].is_null() {
                write!(out, "; replaced version {}", s(&event["replaced_version"]))?;
            }
            writeln!(out, " (status {})", s(&event["change"]["status"]))?;
            if unchanged {
                // The kept version keeps the provenance of the push that made it.
                writeln!(
                    out,
                    "  version {} was pushed from {}",
                    s(&event["version"]),
                    provenance(&event["change"]["source"])
                )?;
                writeln!(
                    out,
                    "  this push came from {}",
                    provenance(&event["source"])
                )?;
            } else {
                writeln!(out, "  from {}", provenance(&event["source"]))?;
            }
        }
        "change_list" => {
            let changes = event["changes"].as_array().cloned().unwrap_or_default();
            for change in &changes {
                writeln!(
                    out,
                    "{}  {}  v{}  {}  {}",
                    s(&change["change_id"]),
                    s(&change["status"]),
                    s(&change["version"]),
                    s(&change["last_pushed_at"]),
                    linked(&change["change_requests"])
                )?;
            }
        }
        "change_show" if event["file"].is_string() => {
            // Lines kept as they are; other control characters escaped.
            for line in event["text"]
                .as_str()
                .unwrap_or_default()
                .split_inclusive('\n')
            {
                let (body, end) = match line.strip_suffix('\n') {
                    Some(body) => (body, "\n"),
                    None => (line, ""),
                };
                let (body, cr) = match body.strip_suffix('\r') {
                    Some(body) => (body, "\r"),
                    None => (body, ""),
                };
                write!(out, "{}{cr}{end}", visible(body))?;
            }
        }
        "change_show" => {
            let v = event;
            writeln!(
                out,
                "{} version {}{}  {}",
                s(&event["id"]),
                s(&v["version"]),
                if v["latest"] == true { " (latest)" } else { "" },
                s(&v["status"])
            )?;
            writeln!(
                out,
                "Pushed {} from {}",
                s(&v["pushed_at"]),
                provenance(&v["source"])
            )?;
            writeln!(out, "Change requests: {}", linked(&v["change_requests"]))?;
            writeln!(out, "Files:")?;
            for file in v["files"].as_array().into_iter().flatten() {
                writeln!(out, "  {}", s(&file["path"]))?;
            }
        }
        "change_pull" => {
            if event["written"] == true {
                writeln!(
                    out,
                    "Wrote version {} of {} to {} ({} files)",
                    s(&event["version"]),
                    s(&event["id"]),
                    s(&event["dir"]),
                    event["files"].as_array().map_or(0, Vec::len)
                )?;
                if event["deleted"].as_array().is_some_and(|d| !d.is_empty()) {
                    writeln!(out, "Deleted: {}", list(&event["deleted"]))?;
                }
            } else {
                writeln!(
                    out,
                    "{} already matches version {} of {}; nothing written",
                    s(&event["dir"]),
                    s(&event["version"]),
                    s(&event["id"])
                )?;
            }
        }
        "change_versions" => {
            for v in event["versions"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "v{}  {}  {}{}",
                    s(&v["version"]),
                    s(&v["pushed_at"]),
                    provenance(&v["source"]),
                    if v["latest"] == true { "  latest" } else { "" }
                )?;
            }
        }
        "change_delete" => writeln!(
            out,
            "Deleted stored change {} and every version",
            s(&event["id"])
        )?,
        "feature_validation" => validation(
            &event["files"],
            event["errors"].as_array().map_or(&[][..], Vec::as_slice),
            out,
        )?,
        "review_policy" => policy(event, out)?,
        "review_policy_versions" => {
            for version in event["versions"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "v{}  {}  {}  {}",
                    version["version"],
                    s(&version["inserted_at"]),
                    s(&version["written_by"]["user_id"]),
                    s(&version["note"])
                )?;
            }
            next_cursor(event, out)?;
        }
        "review_packet" => packet(event, out)?,
        "change_request_tasks" => {
            let tasks = event["tasks"].as_array().cloned().unwrap_or_default();
            if tasks.is_empty() {
                writeln!(out, "No tasks.")?;
            }
            for task in &tasks {
                writeln!(out, "- [{}] {}", s(&task["state"]), s(&task["id"]))?;
                dump(task, 2, out)?;
            }
        }
        "project_events" => {
            if let Some(note) = event["note"].as_str() {
                writeln!(out, "Note: {}", visible(note))?;
            }
            for e in event["events"].as_array().into_iter().flatten() {
                writeln!(
                    out,
                    "{}  {}  {}  {}",
                    s(&e["inserted_at"]),
                    s(&e["type"]),
                    if e["number"].is_null() {
                        "-".to_owned()
                    } else {
                        format!("#{}", e["number"])
                    },
                    visible(&e["data"].to_string())
                )?;
            }
            next_cursor(event, out)?;
        }
        "calibration" => calibration(&event["report"], out)?,
        "review_answer" | "change_request_task" | "merge_lease" | "defect" => {
            let key = match event["type"].as_str() {
                Some("review_answer") => "answer",
                Some("change_request_task") => "task",
                Some("merge_lease") => "lease",
                _ => "defect",
            };
            dump(&event[key], 0, out)?;
        }
        _ => {}
    }
    Ok(())
}

/// Where a stored version was pushed from: commit, branch and dirty flag.
fn provenance(source: &Value) -> String {
    format!(
        "commit {}  branch {}  {}",
        short(&source["commit"]),
        if source["branch"].is_null() {
            "(detached)".to_owned()
        } else {
            s(&source["branch"])
        },
        if source["dirty"] == true {
            "dirty"
        } else {
            "clean"
        }
    )
}

/// The numbers of the change requests that link a stored change.
fn linked(crs: &Value) -> String {
    let numbers: Vec<String> = crs
        .as_array()
        .into_iter()
        .flatten()
        .map(|cr| format!("#{}", s(&cr["number"])))
        .collect();
    if numbers.is_empty() {
        "-".to_owned()
    } else {
        numbers.join(", ")
    }
}

const LANES: [&str; 3] = ["auto", "reviewed", "bypass"];

/// The calibration report: the counts first, then one line per defect,
/// newest first. The defect section appears only when the server sends it.
fn calibration(report: &Value, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "Calibration {} to {}",
        s(&report["from"]),
        s(&report["to"])
    )?;
    for lane in LANES {
        let stats = &report["lanes"][lane];
        let rate = stats["defect_rate"]
            .as_f64()
            .map_or("-".to_owned(), |r| format!("{}%", (r * 100.0).round()));
        writeln!(
            out,
            "  {lane}  {} merged  {} defective  {} defects  {rate}",
            s(&stats["merged_count"]),
            s(&stats["defective_count"]),
            s(&stats["defect_count"])
        )?;
    }
    writeln!(out, "Policy versions:")?;
    for version in report["policy_versions"].as_array().into_iter().flatten() {
        let lanes: Vec<String> = LANES
            .iter()
            .map(|lane| {
                let stats = &version["lanes"][*lane];
                format!(
                    "{lane} {}/{}",
                    s(&stats["merged_count"]),
                    s(&stats["defective_count"])
                )
            })
            .collect();
        writeln!(out, "  v{}  {}", s(&version["version"]), lanes.join("  "))?;
    }
    writeln!(out, "Rules:")?;
    for rule in report["rules"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "  {}  fired {}, with defects {}",
            s(&rule["rule"]),
            s(&rule["fired_count"]),
            s(&rule["fired_with_defects_count"])
        )?;
    }
    writeln!(out, "Gaps: {}", list(&report["gaps"]))?;
    writeln!(out, "Globs:")?;
    for glob in report["globs"].as_array().into_iter().flatten() {
        writeln!(
            out,
            "  {}  {} defects",
            s(&glob["glob"]),
            s(&glob["defect_count"])
        )?;
    }
    writeln!(
        out,
        "  unmatched  {} defects",
        s(&report["unmatched_defect_count"])
    )?;
    if let Some(defects) = report["defects"].as_array() {
        if defects.is_empty() {
            writeln!(out, "Defects: none")?;
        } else {
            writeln!(out, "Defects:")?;
        }
        let joined = |value: &Value| {
            let items: Vec<String> = value.as_array().into_iter().flatten().map(s).collect();
            if items.is_empty() {
                "-".to_owned()
            } else {
                items.join(",")
            }
        };
        for defect in defects {
            let number = match &defect["change_request"] {
                Value::Null => "-".to_owned(),
                n => format!("#{}", s(n)),
            };
            writeln!(
                out,
                "  {}  {number}  {}  {}  {}  v{}  {}  {}",
                s(&defect["recorded_at"]),
                s(&defect["severity"]),
                s(&defect["ticket"]),
                s(&defect["lane"]),
                s(&defect["policy_version"]),
                joined(&defect["rules"]),
                joined(&defect["globs"])
            )?;
        }
        if report["defects_truncated"] == true {
            writeln!(
                out,
                "Showing the {} newest defects; more exist. Narrow the window with --from and --to.",
                defects.len()
            )?;
        }
    }
    if report.get("unattributed_defect_count").is_some() {
        writeln!(
            out,
            "Unattributed defects recorded in the window: {}",
            s(&report["unattributed_defect_count"])
        )?;
    }
    Ok(())
}

/// One line per file, its errors and warnings under it, then problems of
/// the files together and a count.
fn validation(files: &Value, errors: &[Value], out: &mut impl Write) -> Result<()> {
    let files = files.as_array().map_or(&[][..], Vec::as_slice);
    let mut failed = 0;
    for file in files {
        let ok = file["ok"] == true;
        failed += usize::from(!ok);
        let kind = file["kind"]
            .as_str()
            .map_or(String::new(), |k| format!(" ({})", visible(k)));
        writeln!(
            out,
            "{} {}{kind}",
            if ok { "ok    " } else { "failed" },
            s(&file["file"])
        )?;
        for (label, key) in [("error", "errors"), ("warning", "warnings")] {
            for problem in file[key].as_array().into_iter().flatten() {
                writeln!(out, "  {label}: {}", s(problem))?;
            }
        }
    }
    for problem in errors {
        writeln!(out, "error: {}", s(problem))?;
    }
    writeln!(
        out,
        "{} checked: {} ok, {failed} failed{}",
        files.len(),
        files.len() - failed,
        if errors.is_empty() {
            ""
        } else {
            "; the files together have errors"
        }
    )?;
    Ok(())
}

fn list(value: &Value) -> String {
    let items: Vec<String> = value.as_array().into_iter().flatten().map(s).collect();
    if items.is_empty() {
        "none".to_owned()
    } else {
        items.join(", ")
    }
}

fn next_cursor(event: &Value, out: &mut impl Write) -> Result<()> {
    match &event["next_cursor"] {
        Value::String(cursor) => writeln!(out, "next cursor: {}", visible(cursor))?,
        Value::Number(cursor) => writeln!(out, "next cursor: {cursor}")?,
        _ => {}
    }
    Ok(())
}

fn change_request(event: &Value, out: &mut impl Write) -> Result<()> {
    let cr = &event["change_request"];
    match event["action"].as_str().unwrap_or_default() {
        "open" if event["created"] == true => {
            writeln!(out, "Created change request #{}", cr["number"])?
        }
        "open" => writeln!(out, "Using change request #{} of this branch", cr["number"])?,
        _ => {}
    }
    writeln!(
        out,
        "#{} {}  {} -> {}  {}",
        cr["number"],
        s(&cr["state"]),
        s(&cr["source_branch"]),
        s(&cr["target_branch"]),
        s(&cr["title"])
    )?;
    let mut details = vec![format!("head {}", short(&cr["head"]))];
    if let Some(change) = cr["change_id"].as_str() {
        details.push(format!("change {}", visible(change)));
    }
    if let Some(parent) = cr["parent"].as_u64() {
        details.push(format!("parent #{parent}"));
    }
    writeln!(out, "  {}", details.join("  "))?;
    if let Some(url) = cr["url"].as_str() {
        writeln!(out, "  {}", visible(url))?;
    }
    if cr["merge"].is_object() {
        let merge = &cr["merge"];
        writeln!(
            out,
            "  merged {} into {}; verdict at merge {}{}",
            short(&merge["merged_commit"]),
            s(&merge["target_branch"]),
            s(&merge["verdict_state"]),
            if merge["merged_without_mergeable_verdict"] == true {
                " (merged without a mergeable verdict)"
            } else {
                ""
            }
        )?;
    }
    let facts = &event["git_facts"];
    if facts.is_object() {
        writeln!(
            out,
            "Git facts: {} for {} ({} commits, {} files against {})",
            s(&facts["status"]),
            short(&facts["head"]),
            facts["commits"],
            facts["files"],
            s(&facts["target"]["branch"])
        )?;
    }
    let dependencies = &event["dependencies"];
    if dependencies.is_object() {
        writeln!(
            out,
            "Dependencies: {} from {}: {}",
            s(&dependencies["status"]),
            s(&dependencies["file"]),
            list(&dependencies["change_ids"])
        )?;
    }
    let claims = &event["claims"];
    match claims["status"].as_str() {
        Some("missing") => writeln!(
            out,
            "Claims: no file at {}; when it exists, goodflows cr claims uploads it",
            s(&claims["file"])
        )?,
        Some(status) => writeln!(
            out,
            "Claims: {status} from {}{}",
            s(&claims["file"]),
            if claims["wrote_cr"] == true {
                format!(" (wrote cr: {} into it)", cr["number"])
            } else {
                String::new()
            }
        )?,
        None => {}
    }
    let verdict_value = if event["verdict"].is_object() {
        &event["verdict"]
    } else {
        &cr["verdict"]
    };
    if verdict_value.is_object() {
        verdict(verdict_value, out)?;
    }
    if event["action"] == "status" {
        for entry in cr["history"].as_array().into_iter().flatten() {
            writeln!(
                out,
                "  {}  {} -> {}  {}",
                s(&entry["inserted_at"]),
                s(&entry["from_state"]),
                s(&entry["to_state"]),
                s(&entry["user_id"])
            )?;
        }
    }
    Ok(())
}

fn verdict(verdict: &Value, out: &mut impl Write) -> Result<()> {
    writeln!(
        out,
        "Verdict: {}  (head {}, policy v{})",
        s(&verdict["state"]),
        short(&verdict["head"]),
        s(&verdict["policy_version"])
    )?;
    for reason in verdict["reasons"].as_array().into_iter().flatten() {
        let subject = list(&reason["subject"]);
        write!(out, "  {}  {}", s(&reason["outcome"]), s(&reason["rule"]))?;
        if subject != "none" {
            write!(out, "  [{subject}]")?;
        }
        writeln!(out)?;
        if let Some(message) = reason["message"].as_str() {
            writeln!(out, "      {}", visible(message))?;
        }
    }
    for source in ["intent", "diff"] {
        let set = &verdict["scenarios"][source];
        if set.is_object() {
            scenario_set_line(source, set, out)?;
        }
    }
    coverage_line(&verdict["coverage"], out)?;
    if let Some(runs) = verdict["counted_runs"].as_array().filter(|r| !r.is_empty()) {
        writeln!(out, "  counted runs: {}", runs.len())?;
    }
    let review = &verdict["review"];
    if review["items"].as_u64().is_some_and(|items| items > 0) {
        writeln!(
            out,
            "  review: {}/{} items cleared",
            review["cleared"], review["items"]
        )?;
    }
    Ok(())
}

/// One verdict scenario set: `{version, digest, approved, graded,
/// scenarios: [{id, priority, result, run_id}]}`, with a null result when
/// no counted run graded the scenario.
fn scenario_set_line(source: &str, set: &Value, out: &mut impl Write) -> Result<()> {
    write!(
        out,
        "  {source} set v{} {}",
        s(&set["version"]),
        short(&set["digest"])
    )?;
    // Only intent sets are approved; a diff set's approved is always false.
    if let Some(approved) = set["approved"].as_bool().filter(|_| source == "intent") {
        write!(
            out,
            "  {}",
            if approved { "approved" } else { "not approved" }
        )?;
    }
    // A diff set is accepted through diff_set_weakened answers; weakening is
    // measured from its baseline, the newest accepted version or v1.
    if source == "diff" {
        if set["accepted"] == true {
            write!(out, "  accepted")?;
        }
        if set["baseline_version"].is_u64() && set["baseline_version"] != set["version"] {
            write!(out, "  baseline v{}", set["baseline_version"])?;
        }
    }
    if set["graded"] == false {
        writeln!(out, "  not graded")?;
        return Ok(());
    }
    let scenarios = set["scenarios"].as_array().cloned().unwrap_or_default();
    let with = |result: &str| -> Vec<String> {
        scenarios
            .iter()
            .filter(|sc| sc["result"].as_str().unwrap_or("not_run") == result)
            .map(|sc| s(&sc["id"]))
            .collect()
    };
    let (passed, failed, not_run) = (with("passed"), with("failed"), with("not_run"));
    write!(
        out,
        "  passed {}, failed {}, not run {}",
        passed.len(),
        failed.len(),
        not_run.len()
    )?;
    let mut details = Vec::new();
    if !failed.is_empty() {
        details.push(format!("failed: {}", failed.join(", ")));
    }
    if !not_run.is_empty() {
        details.push(format!("not run: {}", not_run.join(", ")));
    }
    if !details.is_empty() {
        write!(out, "  ({})", details.join("; "))?;
    }
    writeln!(out)?;
    Ok(())
}

/// The verdict coverage: `{version, unmapped_areas, features: [{id,
/// driven}]}`; public keys see `unmapped_areas` and feature `id`s as null.
fn coverage_line(coverage: &Value, out: &mut impl Write) -> Result<()> {
    if coverage["version"].is_null() {
        return Ok(());
    }
    let mut parts = vec![format!("feature index v{}", s(&coverage["version"]))];
    if let Some(areas) = coverage["unmapped_areas"]
        .as_array()
        .filter(|a| !a.is_empty())
    {
        parts.push(format!(
            "{} unmapped areas ({})",
            areas.len(),
            list(&coverage["unmapped_areas"])
        ));
    }
    let features = coverage["features"].as_array().cloned().unwrap_or_default();
    let undriven: Vec<&Value> = features.iter().filter(|f| f["driven"] == false).collect();
    if !undriven.is_empty() {
        let ids: Vec<String> = undriven
            .iter()
            .filter(|f| !f["id"].is_null())
            .map(|f| s(&f["id"]))
            .collect();
        let mut part = format!(
            "{} of {} features not driven",
            undriven.len(),
            features.len()
        );
        if !ids.is_empty() {
            part.push_str(&format!(": {}", ids.join(", ")));
        }
        parts.push(part);
    }
    writeln!(out, "  coverage: {}", parts.join("; "))?;
    Ok(())
}

/// `cr scenarios list`: each set's versions, newest first, marking the
/// current one, approved intent and accepted diff versions, and the diff
/// baseline.
fn scenario_sets(event: &Value, out: &mut impl Write) -> Result<()> {
    for source in event["sources"].as_array().into_iter().flatten() {
        let source = source.as_str().unwrap_or_default();
        let set = &event["sets"][source];
        let versions = set["versions"].as_array().cloned().unwrap_or_default();
        if versions.is_empty() {
            writeln!(
                out,
                "{source} set of change request #{}: no versions",
                event["number"]
            )?;
            continue;
        }
        writeln!(out, "{source} set of change request #{}:", event["number"])?;
        for version in &versions {
            write!(
                out,
                "  v{}  {}  {}",
                s(&version["version"]),
                short(&version["digest"]),
                s(&version["inserted_at"])
            )?;
            let mut marks = Vec::new();
            if version["version"] == set["current"]["version"] {
                marks.push("current");
            }
            if source == "intent" && version["approved"] == true {
                marks.push("approved");
            }
            if source == "diff" && version["accepted"] == true {
                marks.push("accepted");
            }
            if source == "diff" && version["version"] == set["baseline_version"] {
                marks.push("baseline");
            }
            if !marks.is_empty() {
                write!(out, "  {}", marks.join(", "))?;
            }
            writeln!(out)?;
        }
    }
    Ok(())
}

fn scenario_changes(event: &Value, out: &mut impl Write) -> Result<()> {
    let comparison = &event["comparison"];
    writeln!(
        out,
        "{} set of change request #{}: version {} against {}{}",
        s(&event["source"]),
        event["number"],
        s(&comparison["to"]),
        s(&comparison["from"]),
        if event["from_baseline"] == true {
            " (the baseline)"
        } else {
            ""
        }
    )?;
    let changes = comparison["changes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if changes.is_empty() {
        writeln!(out, "  no changes")?;
    }
    for change in &changes {
        let mark = match change["change"].as_str() {
            _ if change["weakening"] == true => "!",
            Some("added") => "+",
            Some("removed") => "-",
            _ => "~",
        };
        write!(
            out,
            "  {mark} {}  {}",
            s(&change["id"]),
            s(&change["change"])
        )?;
        if change["from_priority"] != change["to_priority"] {
            write!(
                out,
                "  {} -> {}",
                s(&change["from_priority"]),
                s(&change["to_priority"])
            )?;
        }
        if change["weakening"] == true {
            write!(out, "  (weakening)")?;
        }
        writeln!(out)?;
    }
    Ok(())
}

fn policy(event: &Value, out: &mut impl Write) -> Result<()> {
    let policy = &event["policy"];
    let written_by = &policy["written_by"]["user_id"];
    if event["written"] == true {
        writeln!(out, "# Wrote review policy version {}", policy["version"])?;
    } else if event.get("written").is_some() {
        writeln!(
            out,
            "# Review policy version {} was already written",
            policy["version"]
        )?;
    }
    writeln!(
        out,
        "# review-policy version {} (previous {}), written {} by {}",
        s(&policy["version"]),
        s(&policy["previous_version"]),
        s(&policy["inserted_at"]),
        s(written_by)
    )?;
    if let Some(note) = policy["note"].as_str() {
        writeln!(out, "# note: {}", visible(note))?;
    }
    if policy["active_settings"].is_array() {
        writeln!(
            out,
            "# active settings: {}",
            list(&policy["active_settings"])
        )?;
    }
    let document = if event["effective"] == true {
        &policy["effective"]
    } else {
        &policy["policy"]
    };
    match toml::Value::try_from(document)
        .ok()
        .and_then(|v| toml::to_string(&v).ok())
    {
        Some(text) => write!(out, "{}", visible_lines(&text))?,
        None => writeln!(out, "{}", visible(&serde_json::to_string_pretty(document)?))?,
    }
    Ok(())
}

/// Escapes control characters but keeps the line breaks.
fn visible_lines(text: &str) -> String {
    text.split_inclusive('\n')
        .map(|line| {
            let bare = line.trim_end_matches('\n');
            format!("{}{}", visible(bare), &line[bare.len()..])
        })
        .collect()
}

fn packet(event: &Value, out: &mut impl Write) -> Result<()> {
    let packet = &event["packet"];
    let number = &event["number"];
    writeln!(out, "# Review: change request #{number}\n")?;
    writeln!(
        out,
        "Verdict **{}** at head `{}` against `{}` (merge base `{}`), policy v{}.",
        s(&packet["verdict"]),
        short(&packet["head"]),
        s(&packet["base"]["branch"]),
        short(&packet["base"]["merge_base"]),
        s(&packet["policy_version"])
    )?;
    let passed = &packet["passed"];
    if passed.is_object() {
        writeln!(
            out,
            "\nPassed: {} Must and {} Should scenarios; workflows: {}.",
            s(&passed["must"]),
            s(&passed["should"]),
            list(&passed["workflows"])
        )?;
    }
    for (key, title) in [("blocked", "Blocked"), ("pending", "Pending")] {
        let entries = packet[key].as_array().cloned().unwrap_or_default();
        if entries.is_empty() {
            continue;
        }
        writeln!(out, "\n## {title} (no answer clears these)\n")?;
        for entry in &entries {
            writeln!(
                out,
                "- `{}` {}: {}",
                s(&entry["rule"]),
                list(&entry["subject"]),
                s(&entry["message"])
            )?;
        }
    }
    let items = packet["items"].as_array().cloned().unwrap_or_default();
    writeln!(out, "\n## Items to answer ({})", items.len())?;
    let head = packet["head"].as_str();
    for item in &items {
        writeln!(out, "\n### {}: {}\n", s(&item["kind"]), s(&item["message"]))?;
        writeln!(out, "- id: `{}`", s(&item["id"]))?;
        if item["answer"].is_object() {
            writeln!(
                out,
                "- answered: {}{}",
                s(&item["answer"]["answer"]),
                item["answer"]["note"]
                    .as_str()
                    .map(|n| format!(" ({})", visible(n)))
                    .unwrap_or_default()
            )?;
        }
        item_data(&item["data"], out)?;
        for answer in item["answers"].as_array().into_iter().flatten() {
            let answer = s(answer);
            let extra = match answer.as_str() {
                "choose" => " --option <option>",
                "reject" | "request_change" => " --note '<why>'",
                _ => "",
            };
            writeln!(
                out,
                "- `goodflows cr answer -n {number} {} {answer}{extra}{}`",
                s(&item["id"]),
                head.map(|h| format!(" --head {}", visible(h)))
                    .unwrap_or_default()
            )?;
        }
    }
    Ok(())
}

/// An item's data: hunks as diff blocks, artifacts as images or links, the
/// rest as fields.
fn item_data(data: &Value, out: &mut impl Write) -> Result<()> {
    let Some(fields) = data.as_object() else {
        return Ok(());
    };
    for (key, value) in fields {
        match (key.as_str(), value) {
            ("decisions", Value::Array(decisions)) => {
                for decision in decisions {
                    writeln!(
                        out,
                        "\nDecision `{}` in `{}`: {}\n",
                        s(&decision["id"]),
                        s(&decision["path"]),
                        s(&decision["note"])
                    )?;
                    writeln!(
                        out,
                        "```diff\n{}\n```",
                        visible_lines(decision["hunk"].as_str().unwrap_or_default())
                    )?;
                }
            }
            ("artifacts", Value::Array(artifacts)) => {
                for artifact in artifacts {
                    let id = s(&artifact["id"]);
                    match artifact["url"].as_str() {
                        Some(url)
                            if artifact["content_type"]
                                .as_str()
                                .is_some_and(|t| t.starts_with("image/")) =>
                        {
                            writeln!(out, "\n![{id}]({})", visible(url))?
                        }
                        Some(url) => writeln!(out, "\n[{id}]({})", visible(url))?,
                        None => writeln!(out, "\n{id}: expired")?,
                    }
                }
            }
            (_, Value::Object(_) | Value::Array(_)) => {
                writeln!(out, "- {}:", visible(key))?;
                dump(value, 2, out)?;
            }
            _ => writeln!(out, "- {}: {}", visible(key), s(value))?,
        }
    }
    Ok(())
}

/// Any JSON value as indented `key: value` lines.
fn dump(value: &Value, indent: usize, out: &mut impl Write) -> Result<()> {
    let pad = " ".repeat(indent);
    match value {
        Value::Object(fields) => {
            for (key, value) in fields {
                match value {
                    Value::Object(inner) if !inner.is_empty() => {
                        writeln!(out, "{pad}{}:", visible(key))?;
                        dump(value, indent + 2, out)?;
                    }
                    Value::Array(items) if items.iter().any(|i| i.is_object() || i.is_array()) => {
                        writeln!(out, "{pad}{}:", visible(key))?;
                        dump(value, indent + 2, out)?;
                    }
                    Value::Array(items) => writeln!(
                        out,
                        "{pad}{}: [{}]",
                        visible(key),
                        items.iter().map(s).collect::<Vec<_>>().join(", ")
                    )?,
                    _ => writeln!(out, "{pad}{}: {}", visible(key), s(value))?,
                }
            }
        }
        Value::Array(items) => {
            for item in items {
                match item {
                    Value::Object(_) | Value::Array(_) => {
                        writeln!(out, "{pad}-")?;
                        dump(item, indent + 2, out)?;
                    }
                    _ => writeln!(out, "{pad}- {}", s(item))?,
                }
            }
        }
        _ => writeln!(out, "{pad}{}", s(value))?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn text(event: Value) -> String {
        let mut out = Vec::new();
        render(&event, &mut out).unwrap();
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn verdict_lists_each_reason_with_its_subject() {
        let rendered = text(
            json!({"type": "change_request_verdict", "number": 42, "local_head": "b".repeat(40),
            "verdict": {"state": "pending", "head": "a".repeat(40), "policy_version": 3, "reasons": [
                {"rule": "required_workflow_missing", "outcome": "pending", "subject": ["ci"], "message": "No ci run"},
                {"rule": "one_way_door", "outcome": "needs_review", "subject": [], "message": null}]}}),
        );
        assert_eq!(
            rendered,
            "Verdict: pending  (head aaaaaaaaaaaa, policy v3)\n  pending  required_workflow_missing  [ci]\n      No ci run\n  needs_review  one_way_door\nLocal HEAD bbbbbbbbbbbb is not the reported head; run goodflows cr push or goodflows cr claims.\n"
        );
    }

    #[test]
    fn verdict_summarises_grading_after_the_reasons() {
        let run = "0193a1b2-c3d4-7e5f-8a9b-0c1d2e3f4a5b";
        let rendered = text(json!({"type": "change_request_verdict", "number": 42,
            "verdict": {"state": "needs_review", "head": "a".repeat(40), "policy_version": 3,
            "reasons": [{"rule": "undriven_feature", "outcome": "needs_review", "subject": ["exports"],
                         "message": "Feature exports was not driven"}],
            "inputs": {"git_facts": "f".repeat(64), "claims": null, "required_workflows": {},
                       "approved_intent_digest": "d".repeat(64), "counted_runs": [run],
                       "feature_index_version": 5},
            "scenarios": {
                "intent": {"version": 2, "digest": "d".repeat(64), "approved": true, "graded": true,
                    "scenarios": [
                        {"id": "s1", "priority": "must", "result": "passed", "run_id": run},
                        {"id": "s2", "priority": "should", "result": "failed", "run_id": run},
                        {"id": "s3", "priority": "should", "result": null, "run_id": run}]},
                "diff": {"version": 4, "digest": "e".repeat(64), "approved": false, "accepted": false,
                    "baseline_version": 2, "graded": false,
                    "scenarios": [{"id": "d1", "priority": "must", "result": null, "run_id": null}]}},
            "coverage": {"version": 5, "unmapped_areas": ["scripts/release.sh"],
                "features": [{"id": "imports", "driven": true}, {"id": "exports", "driven": false}]},
            "counted_runs": [run],
            "review": {"items": 2, "cleared": 1},
            "computed_at": "2026-11-21T10:00:00Z"}}));
        assert_eq!(
            rendered,
            "Verdict: needs_review  (head aaaaaaaaaaaa, policy v3)\n  needs_review  undriven_feature  [exports]\n      Feature exports was not driven\n  intent set v2 dddddddddddd  approved  passed 1, failed 1, not run 1  (failed: s2; not run: s3)\n  diff set v4 eeeeeeeeeeee  baseline v2  not graded\n  coverage: feature index v5; 1 unmapped areas (scripts/release.sh); 1 of 2 features not driven: exports\n  counted runs: 1\n  review: 1/2 items cleared\n"
        );
    }

    #[test]
    fn status_prints_history_states_and_public_coverage() {
        let rendered = text(json!({"type": "change_request", "action": "status",
            "change_request": {"number": 7, "state": "merged", "source_branch": "feat", "target_branch": "main",
                "title": "Exports", "head": "c".repeat(40), "change_id": null, "parent": null, "merge": null,
                "verdict": {"state": "pending", "head": null, "policy_version": 1, "reasons": [],
                    "scenarios": {"intent": null, "diff": null},
                    "coverage": {"version": 4, "unmapped_areas": null,
                        "features": [{"id": null, "driven": false}, {"id": null, "driven": true}]},
                    "counted_runs": [], "review": {"items": 0, "cleared": 0}},
                "history": [{"from_state": "open", "to_state": "merged", "user_id": null,
                             "inserted_at": "2026-11-21T10:00:00Z"}]}}));
        assert_eq!(
            rendered,
            "#7 merged  feat -> main  Exports\n  head cccccccccccc\nVerdict: pending  (head -, policy v1)\n  coverage: feature index v4; 1 of 2 features not driven\n  2026-11-21T10:00:00Z  open -> merged  -\n"
        );
    }

    #[test]
    fn change_request_prints_the_server_url_escaped() {
        let rendered = text(
            json!({"type": "change_request", "action": "open", "created": true,
            "change_request": {"number": 7, "state": "draft", "source_branch": "feat", "target_branch": "main",
                "title": "Exports", "head": null, "url": "https://gf.example/cr/7\u{1b}[2J"}}),
        );
        assert_eq!(
            rendered,
            "Created change request #7\n#7 draft  feat -> main  Exports\n  head -\n  https://gf.example/cr/7\\u{1b}[2J\n"
        );
    }

    #[test]
    fn policy_prints_valid_toml_after_comment_lines() {
        let rendered = text(
            json!({"type": "review_policy", "effective": false, "policy": {
            "version": 2, "previous_version": 1, "note": "Require ci", "written_by": {"user_id": "u1"},
            "inserted_at": "2026-11-21T10:00:00Z", "active_settings": ["claims.one_way_door"],
            "policy": {"format": "review-policy/1", "evidence": {"required_workflows": ["ci"]},
                       "paths": [{"glob": "app/priv/**", "outcome": "needs_review"}]}}}),
        );
        let parsed: toml::Value = toml::from_str(&rendered).unwrap();
        assert_eq!(
            parsed["evidence"]["required_workflows"][0].as_str(),
            Some("ci")
        );
        assert_eq!(parsed["paths"][0]["glob"].as_str(), Some("app/priv/**"));
        assert!(
            rendered.starts_with("# review-policy version 2 (previous 1)"),
            "{rendered}"
        );
    }

    #[test]
    fn packet_prints_answer_commands_for_the_head() {
        let rendered = text(json!({"type": "review_packet", "number": 42, "packet": {
            "verdict": "needs_review", "head": "a".repeat(40), "base": {"branch": "main", "merge_base": "b".repeat(40)},
            "policy_version": 7, "passed": {"must": 6, "should": 2, "workflows": ["ci"]},
            "pending": [], "blocked": [],
            "items": [{"id": "question:9c1b", "kind": "question", "rule": "open_items", "subject": ["q1"],
                "message": "Open question q1", "answers": ["choose", "request_change"], "answer": null,
                "data": {"id": "q1", "text": "Keep the old export?", "options": ["a", "b"], "recommendation": "a"}}]}}));
        assert!(
            rendered.contains("### question: Open question q1"),
            "{rendered}"
        );
        assert!(
            rendered.contains("- options:\n  - a\n  - b\n") || rendered.contains("- options:"),
            "{rendered}"
        );
        assert!(rendered.contains(&format!(
            "`goodflows cr answer -n 42 question:9c1b choose --option <option> --head {}`",
            "a".repeat(40)
        )));
    }

    fn report() -> Value {
        let stats = |m: u64, d: u64, n: u64, rate: Value| json!({"merged_count": m, "defective_count": d, "defect_count": n, "defect_rate": rate});
        json!({
            "from": "2026-08-24T00:00:00Z", "to": "2026-11-22T00:00:00Z",
            "lanes": {"auto": stats(20, 3, 5, json!(0.15)),
                      "reviewed": stats(3, 1, 1, json!(0.3333333333333333)),
                      "bypass": stats(0, 0, 0, Value::Null)},
            "policy_versions": [{"version": 7, "lanes": {"auto": stats(20, 3, 5, json!(0.15)),
                "reviewed": stats(3, 1, 1, json!(0.3)), "bypass": stats(0, 0, 0, Value::Null)}}],
            "rules": [{"rule": "path_match", "fired_count": 2, "fired_with_defects_count": 1}],
            "gaps": [46, 51],
            "globs": [{"glob": "app/priv/**", "defect_count": 2}],
            "unmatched_defect_count": 1
        })
    }

    #[test]
    fn calibration_prints_the_counts_then_the_defects() {
        let mut report = report();
        report["defects"] = json!([
            {"recorded_at": "2026-11-03T09:00:00Z", "change_request": 43, "severity": "high",
             "ticket": "GOO-512", "lane": "reviewed", "policy_version": 7,
             "rules": ["max_changed_lines", "path_match"], "globs": ["app/priv/repo/migrations/**"]},
            {"recorded_at": "2026-11-02T09:00:00Z", "change_request": 46, "severity": null,
             "ticket": null, "lane": "auto", "policy_version": 7, "rules": [], "globs": []}
        ]);
        report["defects_truncated"] = json!(true);
        report["unattributed_defect_count"] = json!(2);
        let rendered = text(json!({"type": "calibration", "report": report}));
        assert_eq!(
            rendered,
            "Calibration 2026-08-24T00:00:00Z to 2026-11-22T00:00:00Z
  auto  20 merged  3 defective  5 defects  15%
  reviewed  3 merged  1 defective  1 defects  33%
  bypass  0 merged  0 defective  0 defects  -
Policy versions:
  v7  auto 20/3  reviewed 3/1  bypass 0/0
Rules:
  path_match  fired 2, with defects 1
Gaps: 46, 51
Globs:
  app/priv/**  2 defects
  unmatched  1 defects
Defects:
  2026-11-03T09:00:00Z  #43  high  GOO-512  reviewed  v7  max_changed_lines,path_match  app/priv/repo/migrations/**
  2026-11-02T09:00:00Z  #46  -  -  auto  v7  -  -
Showing the 2 newest defects; more exist. Narrow the window with --from and --to.
Unattributed defects recorded in the window: 2
"
        );
    }

    #[test]
    fn calibration_without_the_defect_list_prints_only_the_counts() {
        let rendered = text(json!({"type": "calibration", "report": report()}));
        assert!(rendered.contains("  auto  20 merged"), "{rendered}");
        assert!(rendered.ends_with("  unmatched  1 defects\n"), "{rendered}");
        assert!(!rendered.contains("Defects") && !rendered.contains("Unattributed"));

        let mut empty = report();
        empty["defects"] = json!([]);
        empty["defects_truncated"] = json!(false);
        let rendered = text(json!({"type": "calibration", "report": empty}));
        assert!(rendered.ends_with("Defects: none\n"), "{rendered}");
    }

    #[test]
    fn calibration_escapes_server_text() {
        let mut report = report();
        report["defects"] = json!([{"recorded_at": "t", "change_request": 1, "severity": "low",
            "ticket": "GOO-1\u{1b}[2J", "lane": "auto", "policy_version": 1,
            "rules": ["r\u{1b}"], "globs": []}]);
        let rendered = text(json!({"type": "calibration", "report": report}));
        assert!(!rendered.contains('\u{1b}'), "{rendered:?}");
        assert!(rendered.contains("GOO-1"), "{rendered}");
    }
}
