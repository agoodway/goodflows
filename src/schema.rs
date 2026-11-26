//! `goodflows schema`: the formats of the files goodflows reads from a
//! checkout, printed so an agent can write a file that passes the
//! validator. Every format name, enumeration and limit the CLI checks
//! comes from the constants its parsers use; rules only the server checks
//! are stated as prose.

use crate::change_requests::{markdown, verify_run};
use crate::output::Output;
use anyhow::Result;
use serde_json::json;

/// A schema-only name for a verify skill's feature files, which have no
/// `format` key.
pub const FEATURE_FORMAT: &str = "feature-file/1";

const EXAMPLES: &str = "Examples:
  goodflows schema list                                  # every format, its file and its validator
  goodflows schema show qa-scenarios/2 --source diff     # how to write a diff scenario set
  goodflows schema show verify-run-notes/1               # how to write a run folder's run.md
  goodflows schema show change-claims --json             # the newest claims format as one JSON event";

#[derive(clap::Subcommand, Debug)]
pub enum Command {
    /// List every format this goodflows checks, with the file it applies to and its validator
    #[command(after_help = EXAMPLES)]
    List,
    /// Print what an agent needs to write a file: a template, the field, body and handling rules, and the validator
    #[command(after_help = EXAMPLES)]
    Show {
        /// The format, such as qa-scenarios/2, or its name alone for the newest version
        format: String,
        /// The qa-scenarios/2 variant: intent, written from a goodflows change before code, or diff, written from the branch's change
        #[arg(long, value_enum)]
        source: Option<Source>,
    },
}

#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Intent,
    Diff,
}

impl Source {
    fn name(self) -> &'static str {
        match self {
            Self::Intent => markdown::SET_SOURCES[0],
            Self::Diff => markdown::SET_SOURCES[1],
        }
    }
}

struct Format {
    format: &'static str,
    file: &'static str,
    validate: &'static str,
    sources: &'static [&'static str],
}

const FORMATS: [Format; 4] = [
    Format {
        format: markdown::SET_FORMAT,
        file: "a QA scenario set: goodflows/changes/<id>/qa-scenarios.md (intent) or .goodflows/change-requests/<branch>/qa-scenarios.md (diff)",
        validate: "goodflows cr validate <file>",
        sources: &markdown::SET_SOURCES,
    },
    Format {
        format: verify_run::NOTES_FORMAT,
        file: "run.md in a .verify/<app>/<run-id>/ run folder",
        validate: "goodflows cr validate <run folder>",
        sources: &[],
    },
    Format {
        format: markdown::CLAIMS_FORMAT,
        file: "a change request's claims file, .goodflows/change-requests/<branch>/cr.md",
        validate: "goodflows cr validate <file>",
        sources: &[],
    },
    Format {
        format: FEATURE_FORMAT,
        file: "a verify skill's feature file, <skills>/verify-<app>/features/<feature>.md (no format key)",
        validate: "goodflows features validate",
        sources: &[],
    },
];

pub async fn command(command: &Command, output: &Output) -> Result<u8> {
    match command {
        Command::List => {
            let formats: Vec<_> = FORMATS
                .iter()
                .map(|f| {
                    let (name, version) = split(f.format);
                    json!({
                        "format": f.format,
                        "name": name,
                        "version": version,
                        "accepted": [f.format],
                        "file": f.file,
                        "validate": f.validate,
                        "sources": f.sources,
                    })
                })
                .collect();
            output
                .emit(json!({
                    "type": "schema_list",
                    "goodflows_version": env!("CARGO_PKG_VERSION"),
                    "formats": formats,
                }))
                .await?;
        }
        Command::Show { format, source } => {
            let (format, source) = resolve(format, *source)?;
            let schema = schema(format.format, source);
            output
                .emit(json!({
                    "type": "schema",
                    "goodflows_version": env!("CARGO_PKG_VERSION"),
                    "format": format.format,
                    "source": source.map(Source::name),
                    "template": schema.template,
                    "fields": schema.fields,
                    "body": schema.body,
                    "handling": schema.handling,
                    "validate": format.validate,
                }))
                .await?;
        }
    }
    Ok(0)
}

/// `name/version` as its name and version.
fn split(format: &str) -> (&str, &str) {
    format.split_once('/').unwrap_or((format, ""))
}

fn resolve(given: &str, source: Option<Source>) -> Result<(&'static Format, Option<Source>)> {
    let (name, version) = match given.split_once('/') {
        Some((name, version)) => (name, Some(version)),
        None => (given, None),
    };
    let names: Vec<_> = FORMATS.iter().map(|f| f.format).collect();
    let Some(format) = FORMATS.iter().find(|f| split(f.format).0 == name) else {
        return Err(crate::usage(format!(
            "unknown format {given}; the formats are {} (goodflows schema list)",
            names.join(", ")
        )));
    };
    if version.is_some_and(|v| v != split(format.format).1) {
        return Err(crate::usage(format!(
            "this goodflows reads only {} for {name}, not {given}",
            format.format
        )));
    }
    match (format.sources.is_empty(), source) {
        (true, Some(_)) => Err(crate::usage(format!(
            "{} has no variants; leave out --source",
            format.format
        ))),
        (false, None) => Err(crate::usage(format!(
            "{} has two variants with different keys; pass {}",
            format.format,
            format
                .sources
                .iter()
                .map(|s| format!("--source {s}"))
                .collect::<Vec<_>>()
                .join(" or ")
        ))),
        _ => Ok((format, source)),
    }
}

struct Schema {
    template: String,
    fields: Vec<String>,
    body: Vec<String>,
    handling: Vec<String>,
}

fn schema(format: &str, source: Option<Source>) -> Schema {
    match (format, source) {
        (markdown::SET_FORMAT, Some(source)) => scenario_set(source),
        (verify_run::NOTES_FORMAT, _) => run_notes(),
        (markdown::CLAIMS_FORMAT, _) => claims(),
        _ => feature_file(),
    }
}

fn lines(items: &[&str]) -> Vec<String> {
    items.iter().map(|s| (*s).to_owned()).collect()
}

const FRONT_MATTER: &str = "The file starts with a line holding only ---, and the YAML front matter ends at the next such line. Nothing comes before it.";
const QUOTING: &str = "Quote every string with double quotes and escape a double quote in one as \\\". Numbers, true, false, null and the lowercase values named here stay bare. YAML anchors, aliases, tags and non-integer numbers are refused.";

fn scenario_set(source: Source) -> Schema {
    let set = markdown::SET_FORMAT;
    let notes = verify_run::NOTES_FORMAT;
    let intent = source == Source::Intent;
    let (this, other) = if intent {
        (Source::Intent.name(), Source::Diff.name())
    } else {
        (Source::Diff.name(), Source::Intent.name())
    };
    let template = if intent {
        format!(
            r#"---
format: {set}
source: {this}
change: "[goodflows change id]"
specs: uncommitted
review: null
scenarios:
  - id: 1
    name: "[Short name of the behavior]"
    priority: must
    covers:
      - capability: "[capability]"
        requirement: "[Requirement name exactly as in the spec delta]"
        implied: true
  - id: 2
    name: "[Short name of another behavior]"
    priority: should
    covers:
      - capability: "[capability]"
        requirement: "[Requirement name exactly as in the spec delta]"
        scenario: "[Spec scenario name exactly as in the spec delta]"
---

# QA: [goodflows change id]

[One line on what to run these against: the server, the data, the accounts.]

## Scenarios

### 1. [Short name of the behavior] (Must)

1. [One action, with the exact command, label or input]
   - Expect: [What the person sees: an exit code, a value, a stored state]

### 2. [Short name of another behavior] (Should)

1. [One action]
   - Expect: [What the person sees]
"#
        )
    } else {
        format!(
            r#"---
format: {set}
source: {this}
branch: "[branch]"
base: "[base branch]"
revision: "[short SHA of the commit the set covers]"
review: null
scenarios:
  - id: 1
    name: "[Short name of the behavior]"
    priority: must
  - id: 2
    name: "Regression: [Flow that did not change but calls changed code]"
    priority: should
regressions:
  - "[Flow that did not change but calls changed code]: scenario 2"
---

# QA: [branch]

## What changed

[Two or three sentences in product terms.]

## Setup

- Run: [how to reach the branch]
- Accounts: [each account or role, and what it is for]

## Checklist

- [ ] 1. [Short name of the behavior] (Must)
- [ ] 2. Regression: [Flow that did not change but calls changed code] (Should)

## Scenarios

### 1. [Short name of the behavior] (Must)

Why: [One sentence: what a user hits if this is wrong.]

1. [One action per step, with the exact label: Click **Save**.]
   - Expect: [What the tester sees after this step: exact text, a value, a count or a state.]

### 2. Regression: [Flow that did not change but calls changed code] (Should)

Why: [The changed behavior this flow depends on.]

1. [One action]
   - Expect: [What the tester sees]

## Regression checks

- [Flow that did not change but calls changed code]: scenario 2
"#
        )
    };
    let mut fields = vec![
        "Write exactly these keys; the server refuses any other key. Key order is not checked.".to_owned(),
        QUOTING.to_owned(),
        format!("format: {set}."),
        format!("source: {this}. The {other} set has other keys: goodflows schema show {set} --source {other}."),
    ];
    if intent {
        fields.extend(lines(&[
            "change: the goodflows change id, the directory name under goodflows/changes/, 1 to 255 characters.",
            "specs: uncommitted until the change's specs are committed, then that commit's SHA, 7 to 64 lowercase hex characters.",
        ]));
    } else {
        fields.extend(lines(&[
            "branch and base: the branch and the branch it merges into, 1 to 255 characters each.",
            "revision: the short SHA of the commit the set covers, 7 to 64 lowercase hex characters and nothing else. A revision ending +dirty is refused.",
        ]));
    }
    fields.extend(lines(&[
        "review: optional; null, or a string of 1 to 2,048 characters naming the change under review: a goodflows change request number or a merge or pull request URL. Never a branch or a commit.",
    ]));
    fields.push(format!(
        "scenarios: 1 to 200 entries, one per scenario in the body, in the same order. Each has id, name and priority{}, and no other key.",
        if intent { ", and optional covers" } else { "" }
    ));
    fields.extend(lines(&[
        "id: a positive integer, or a string matching [A-Za-z0-9][A-Za-z0-9._-]{0,63}. Ids are unique as strings, so 1 and \"1\" collide.",
        "name: the scenario's heading without its id and priority, 1 to 200 characters.",
        "priority: must, should or could.",
    ]));
    if intent {
        fields.extend(lines(&[
            "covers: 1 to 100 entries, each with capability and requirement, copied exactly from the spec delta (1 to 200 characters), plus exactly one of scenario: <spec scenario name> or implied: true. implied: true is the usual case.",
            "coverage: optional, up to 1,000 rows of capability, requirement, scenario, status (covered or no_check), scenarios (ids of this set) and optional reason (up to 1,000 characters, or null). A covered row names at least one scenario, a no_check row none, and no two rows share capability, requirement and scenario. Leave it out unless the change needs it; covers does its job.",
        ]));
    } else {
        fields.push("regressions: optional, up to 200 strings of 1 to 1,000 characters, one per line under ## Regression checks, each naming a flow that calls changed code and the scenario that checks it, such as \"Invoice export: scenario 5\". Use [] for none.".to_owned());
    }
    fields.push("Never add run results: a top-level run key, or result, failed_step, evidence or reason on a scenario. The server refuses a set that holds them.".to_owned());

    let mut body = vec![
        FRONT_MATTER.to_owned(),
        "The body has a \"## Scenarios\" heading. Under it, before the next level 1 or 2 heading, each scenario has exactly one heading: \"### <id>. <name> (<Priority>)\", or \"#### <id>. <name> (<Priority>)\" under a group heading, such as \"### Free plan\", that has no \"<id>. \".".to_owned(),
        "Headings inside ``` or ~~~ fences do not count.".to_owned(),
        "A scenario's section runs from its heading to the next heading at its level or higher. Its SHA-256 is uploaded with the set, so the server can tell which scenarios changed between versions.".to_owned(),
        "Each step is one action. Each result is an \"- Expect:\" line under the step that produces it, naming something a person can check: an exit code, a value, a stored state, what is or is not sent. \"It works\" is not a result.".to_owned(),
    ];
    if intent {
        body.push("Steps say what a person does and sees in terms of behavior, not exact output text. Every requirement the change adds or modifies is covered by at least one must scenario.".to_owned());
    } else {
        body.push("Other sections, such as What changed, Setup, Checklist, Regression checks and Not covered by these scenarios, are for the tester; goodflows does not read them. Each Regression checks line matches an entry of regressions.".to_owned());
    }

    let mut handling = vec![
        "Upload it with goodflows cr scenarios upload <file>. The file is sent byte for byte, never reformatted, and approval is tied to the SHA-256 of its bytes, so any edit needs a new approval.".to_owned(),
        format!("Never write run results into the set. They go in a run folder's run.md: goodflows schema show {notes}."),
    ];
    if intent {
        handling.extend(lines(&[
            "Write it at goodflows/changes/<id>/qa-scenarios.md. goodflows change validate checks its covers against the change, and the set is approved before code with goodflows cr scenarios approve.",
        ]));
    } else {
        handling.extend(lines(&[
            "Write it at qa-scenarios.md next to the branch's claims file; goodflows cr path --json prints the path as scenarios_path, and goodflows cr scenarios upload uploads it without arguments.",
            "A diff set names one commit. After every commit, write the set again for the new commit and upload it.",
        ]));
    }
    Schema {
        template,
        fields,
        body,
        handling,
    }
}

fn run_notes() -> Schema {
    use verify_run::*;
    let notes = NOTES_FORMAT;
    let set = markdown::SET_FORMAT;
    let results = RESULTS.join(", ");
    let mib = MAX_ARTIFACT_BYTES / (1024 * 1024);
    let template = format!(
        r#"---
format: "{notes}"
set:
  path: "[path of the scenario set file from the repository root]"
  sha256: "[SHA-256 of scenarios.md in this run folder]"
commit: "[full commit SHA the app was built from]"
branch: "[branch, or null without the quotes]"
dirty: false
changed_files: []
started_at: "[RFC 3339 UTC time the run started]"
finished_at: "[RFC 3339 UTC time the run finished]"
results:
  - id: 1
    result: passed
    failed_step: null
    reason: null
    evidence: ["01-scenario-1.png"]
  - id: 2
    result: failed
    failed_step: "3"
    reason: "[what failed: the Expect line and what was seen instead]"
    evidence: ["02-scenario-2-step-3.png"]
features:
  - id: "[feature id from the verify skill's feature map]"
    entry_point: "[entry point driven]"
    result: passed
drift: []
---

# Verify run [run id]

[One line: the scenario set driven, where it came from, and the commit the app was built from.]

- 1. [Short name of the behavior] (Must): passed. Evidence: 01-scenario-1.png
- 2. [Short name of another behavior] (Should): failed at step 3, [what failed]. Evidence: 02-scenario-2-step-3.png
"#
    );
    let fields = vec![
        QUOTING.to_owned(),
        format!("format: \"{notes}\"."),
        "set: optional; path, the scenario set file from the repository root, and sha256, the lowercase SHA-256 of the run folder's scenarios.md. A sha256 that differs from scenarios.md is refused. Leave set out when the run drove no set from a file.".to_owned(),
        format!(
            "commit: required; the full commit SHA the app was built from, {} or {} lowercase hex characters.",
            COMMIT_LENGTHS[0], COMMIT_LENGTHS[1]
        ),
        format!("branch: the branch, at most {MAX_BRANCH_BYTES} bytes without control characters, or null."),
        "dirty: required; true when git status --porcelain printed anything, else false.".to_owned(),
        format!("changed_files: when dirty, the files git status listed, as relative paths of 1 to {MAX_PATH_BYTES} bytes, at most {MAX_CHANGED_FILES}; otherwise []."),
        "started_at and finished_at: required; RFC 3339 times such as 2026-11-19T14:30:00Z.".to_owned(),
        format!("results: required; exactly one entry per scenario id in scenarios.md, at most {MAX_RESULTS}. Each has id, result ({results}), failed_step, reason and evidence."),
        format!("failed_step: the step number as a quoted string, such as \"3\", or null. A bare number does not parse as a step. reason: what failed or why the scenario did not run, or null for a passed one. Each at most {MAX_RESULT_TEXT_BYTES} bytes."),
        format!("evidence: the files that prove the result, relative to the run folder or as .verify/<app>/<run-id>/<file> of this folder. Each file holds 1 byte to {mib} MiB; a run names at most {MAX_ARTIFACTS} files."),
        format!("features: optional, at most {MAX_FEATURES}; each feature driven, with id (1 to {MAX_FEATURE_ID_BYTES} bytes), entry_point (a string of at most {MAX_TEXT_BYTES} bytes, or null) and result ({results})."),
        format!("drift: optional, at most {MAX_DRIFT}; each feature map problem the run found, as a string of at most {MAX_TEXT_BYTES} bytes."),
        "run_id: optional; a lowercase canonical UUID. Without it, the id is derived from the run folder's name and commit, so uploading the same folder again is a duplicate.".to_owned(),
        "merge_result, links and workflow_run_id: optional; sent to the server as written.".to_owned(),
    ];
    let body = vec![
        "The run folder is .verify/<app>/<run-id>/ at the repository root, where <run-id> sorts by time, such as 20261119T143000Z-<short SHA>.".to_owned(),
        format!("scenarios.md is the {set} set the run drove, copied byte for byte (cp), not retyped."),
        "run.md is the front matter above, then a free Markdown index: each scenario with its id, name, priority, result, failed step or reason, and evidence files. The front matter and the index say the same things.".to_owned(),
        "The evidence files sit in the run folder.".to_owned(),
    ];
    let handling = lines(&[
        "Never edit scenarios.md to clear an error; it must stay the set the run drove. Never write results into the scenario set itself.",
        "Upload the folder with goodflows cr runs upload <run folder>. The server accepts only a run whose set is stored on a change request; a run with no set stays local.",
        "A run from a dirty tree is accepted, but it tested code no commit holds.",
        "Never stage or commit anything under .verify/; keep .verify/ in .gitignore.",
    ]);
    Schema {
        template,
        fields,
        body,
        handling,
    }
}

fn claims() -> Schema {
    let claims = markdown::CLAIMS_FORMAT;
    let template = format!(
        r#"---
format: {claims}
cr: null
head: "[full commit SHA of HEAD]"
door: two_way
door_reason: "[why a revert is or is not enough]"
reach: "[who or what is hit if this is wrong]"
safe_because:
  - fact: "[a fact that makes the change safe]"
    location: "[path:line]"
    proof: ran
    evidence:
      - ".verify/[app]/[run id]/run.md#[scenario id]"
left_open: []
questions:
  - id: "1"
    text: "[the question the reviewer must answer]"
    options:
      - "[an option]"
      - "[another option]"
    recommendation: "[an option]"
decisions:
  - id: "1"
    path: "[path from the repository root]"
    hunk: |-
      @@ -10,3 +10,4 @@ [the hunk exactly as git diff prints it]
       [context line]
      -[removed line]
      +[added line]
    note: "[why this hunk changed, or what the reviewer should check]"
---

# [type: what changed]

## What changed

[What a person or the system does differently after merge.]

## How it works

[One short paragraph, and the smallest picture that makes the change clear.]

## Evidence

[What was run, and what it showed.]

## What could break

Door: [one-way or two-way, and why]

Reach: [who or what is hit if this is wrong]

Safe because: [each fact, path:line, and how far it was proven]

Left open: [each remaining risk, or None found.]

## How to check

[The smallest steps a reviewer can run.]

## Questions for the reviewer

[Each question with its options and your recommendation, or None.]

## Decisions to judge

[Each decision hunk as a diff block with its note, or None.]
"#
    );
    let fields = vec![
        format!(
            "Write exactly these {} keys: {}. The server refuses a missing or unknown key.",
            markdown::CLAIMS_KEYS.len(),
            markdown::CLAIMS_KEYS.join(", ")
        ),
        format!("{QUOTING} The one exception is hunk, a |- block scalar, so diff lines keep their text."),
        format!("format: {claims}."),
        "cr: null until goodflows cr open creates the change request and writes its number here; then that positive integer. Keep a top-level cr: line.".to_owned(),
        "head: the full SHA git rev-parse HEAD prints. Claims describe one head; the server refuses claims for any head but the change request's current one.".to_owned(),
        format!(
            "door: {} (merge cannot be walked back: a destructive migration, sent mail or money, a published API, deleted data) or {}. door_reason and reach: nonblank strings.",
            markdown::DOORS[0],
            markdown::DOORS[1]
        ),
        format!(
            "safe_because: 1 to 100 facts, each with fact and location (nonblank; location is a real path:line), proof ({}, from weakest to strongest), and optional evidence and covers.",
            markdown::PROOF_LEVELS.join(", ")
        ),
        "evidence: up to 20 relative references of 1 to 1,024 bytes, such as a verify run's file, run.md#<scenario id>, or a test as path:line. A fact with proof: ran names at least one.".to_owned(),
        "covers: optional; exactly capability, requirement and scenario, copied from the goodflows change's spec delta, nonempty.".to_owned(),
        "left_open: up to 100 risks, each with risk, location and affected, nonblank; or [].".to_owned(),
        "questions: up to 50, or []; each with id, a quoted string unique among questions (a bare 1 is refused), text, options (two or more nonblank strings) and recommendation, one of the options copied exactly.".to_owned(),
        "decisions: up to 3 hunks a reviewer should judge, or []; each with id (a quoted string, unique), path (a path in the head's diff), hunk (from its @@ line, copied from git diff with no line edited) and note.".to_owned(),
    ];
    let body = vec![
        FRONT_MATTER.to_owned(),
        "The body's first \"# \" heading is the title goodflows cr open uses, a conventional commit subject such as \"feat: add login\". Without it, cr open needs --title.".to_owned(),
        "The server does not parse the rest of the body. It says the same things as the front matter for a reviewer, under the headings in the template; write \"None.\" under a heading with nothing in it.".to_owned(),
    ];
    let handling = lines(&[
        "Write it at the path goodflows cr path prints, .goodflows/change-requests/<branch>/cr.md. Keep it untracked (.goodflows/ in .git/info/exclude) and never commit it, since committing makes head stale.",
        "Commit first: the claims describe a clean working tree at head. After every new commit, set head again and recheck every claim.",
        "While cr is null, goodflows cr open --claims <file> creates the change request and fills in cr. Once cr holds a number, goodflows cr claims <file> updates it.",
        "goodflows cr validate fails on format, cr, head and the question and decision ids, and warns about missing or unknown keys, door, proof, ran facts without evidence and recommendations outside their options. Fix every warning too. The server checks the rest on upload, such as head and decision paths against the change request, so a file that validates can still be refused.",
    ]);
    Schema {
        template,
        fields,
        body,
        handling,
    }
}

fn feature_file() -> Schema {
    let template = r#"---
id: "[feature id: lowercase letters, digits, - and _]"
title: "[Feature name]"
paths:
  - "[glob of the code the feature covers, such as lib/login/**]"
---

# [Feature name]

[What the feature lets a user do, in one or two sentences.]

## Sub-features

- [sub-feature id]: [what it does]

## How to get to it (user POV)

- [Each entry point a user takes: a button, a URL, a command]

## Driving it with [harness]

Preconditions:

- [State the app must be in first]

- **[Step name].** [What the user does.] Run `[harness command]`. [The observable result that proves it.]

## Gotchas

- [What trips a driver up, and why]
"#
    .to_owned();
    let fields = lines(&[
        "The front matter is optional and has no format key; feature-file/1 is only this schema's name. Do not add one.",
        "id: optional, default the file name without .md. The server requires [a-z0-9][a-z0-9_-]{0,63}, and no two files with paths may share an id.",
        "title: optional string, default the body's first \"# \" heading, else the id. The server requires 1 to 200 characters.",
        "paths: the globs of the code the feature covers, as a list of strings; the server accepts 1 to 100 globs of 1 to 512 bytes. Each glob is relative, holds no control characters and none of [ ] { } \\, and uses ** only as a whole path segment, such as lib/**/*.ex. A file without paths is skipped by goodflows features sync.",
    ]);
    let body = lines(&[
        "Feature files live in <skills>/verify-<app>/features/, where <skills> is .agents/skills, .claude/skills or .cursor/skills. README.md there is the index, not a feature file; every other .md file is one.",
        "Each file answers, from the user's point of view, what the feature is, how to reach it, how to drive it with the verify skill's harness, and what observable end state proves it works, under the four H2s in the template.",
        "Use stable handles (ARIA roles and names, data attributes, prompt strings, route paths) over coordinates and tab order.",
    ]);
    let handling = lines(&[
        "goodflows features sync uploads the files with paths as the project's feature index, which maps changed files to features; at least one file needs paths.",
        "With several verify-*/features directories, pass --dir to goodflows features validate and sync.",
        "When a journey cannot pass because the seed data lacks a precondition, record the reason under Gotchas; never weaken the journey.",
    ]);
    Schema {
        template,
        fields,
        body,
        handling,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_resolve_to_the_newest_version_and_sources_are_checked() {
        let ok = |given: &str, source| resolve(given, source).map(|(f, _)| f.format).ok();
        assert_eq!(
            ok("qa-scenarios", Some(Source::Intent)),
            Some("qa-scenarios/2")
        );
        assert_eq!(ok("change-claims", None), Some("change-claims/2"));
        assert_eq!(ok("feature-file/1", None), Some(FEATURE_FORMAT));
        let error = |given: &str, source| format!("{:#}", resolve(given, source).err().unwrap());
        let missing = error("qa-scenarios/2", None);
        assert!(
            missing.contains("--source intent or --source diff"),
            "{missing}"
        );
        assert!(error("qa-scenarios/1", Some(Source::Diff)).contains("only qa-scenarios/2"));
        assert!(error("nope", None).contains("verify-run-notes/1"));
        assert!(error("change-claims", Some(Source::Diff)).contains("no variants"));
    }

    #[test]
    fn each_variant_names_only_its_own_keys() {
        let keys = |source| {
            let s = scenario_set(source);
            s.fields.join("\n") + &s.template
        };
        let intent = keys(Source::Intent);
        let diff = keys(Source::Diff);
        for key in ["change:", "specs:", "covers:", "coverage:"] {
            assert!(intent.contains(key), "{key}");
        }
        for key in ["branch", "base", "revision:", "regressions:"] {
            assert!(diff.contains(key), "{key}");
        }
        assert!(!intent.contains("regressions:"));
        assert!(!diff.contains("coverage:"));
    }
}
