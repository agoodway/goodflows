//! Goodflows changes: `goodflows change validate` and `change archive`.
//!
//! A change is a `goodflows/changes/<id>/` directory holding `change.md`,
//! whose `goodflows-change/1` front matter has the goals, the decisions and
//! what is left to the builder; requirement deltas in
//! `specs/<capability>/spec.md`; and `qa-scenarios.md`, the intent scenario
//! set. Archiving merges the deltas into `goodflows/specs/` and moves the
//! change to `goodflows/changes/archive/YYYY-MM-DD-<id>/`.

use super::git::Git;
use super::markdown::{self, Invalid};
use crate::output::Output;
use anyhow::{bail, Result};
use clap::Subcommand;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

const FORMAT: &str = "goodflows-change/1";
const CHANGES: &str = "goodflows/changes";
const SPECS: &str = "goodflows/specs";
const KEYS: [&str; 12] = [
    "format",
    "id",
    "status",
    "size",
    "door",
    "capabilities",
    "depends_on",
    "goals",
    "non_goals",
    "anti_goals",
    "decisions",
    "left_to_builder",
];

#[derive(Subcommand)]
pub enum ChangeCommand {
    /// Check goodflows changes: change.md, the spec deltas, the intent scenario set, and how they cite each other
    #[command(
        after_help = "Checks each goodflows/changes/<id>/ directory: the goodflows-change/1 front matter of
change.md, every specs/<capability>/spec.md delta against goodflows/specs/, and qa-scenarios.md.
Every requirement must cite a goal or decision on a Refs: line and be covered by a Must scenario;
every decision the user made must be cited. Warnings flag likely over-specification. Exits 1 when
any change has errors. Needs no API key or goodflows.toml. goodflows agents describes the format.

Examples:
  goodflows change validate                   # every change except goodflows/changes/archive/
  goodflows change validate add-login         # one change
  goodflows change validate --json            # every change's errors and warnings as JSON"
    )]
    Validate {
        /// Change ids under goodflows/changes/ (default: every change there except archive/)
        #[arg(value_name = "ID", value_parser = super::change_id)]
        ids: Vec<String>,
    },
    /// Merge an approved change's spec deltas into goodflows/specs/ and move it to goodflows/changes/archive/
    #[command(
        after_help = "Refuses, changing nothing, unless the change validates and its status is approved. Applies
RENAMED, REMOVED, MODIFIED and then ADDED requirements to goodflows/specs/<capability>/spec.md,
creating a capability spec that does not exist, and drops the Refs: lines, whose ids mean
something only inside the change. Then sets status: archived and moves the change to
goodflows/changes/archive/<UTC date>-<id>/.

Examples:
  goodflows change validate add-login         # check it first; archive refuses on errors
  goodflows change archive add-login          # merge and move add-login
  goodflows change archive add-login --json   # print the merged specs as JSON"
    )]
    Archive {
        /// The change id under goodflows/changes/
        #[arg(value_name = "ID", value_parser = super::change_id)]
        id: String,
    },
}

pub async fn change(command: &ChangeCommand, output: &Output) -> Result<u8> {
    let top = Git::new("GOODFLOWS_API_KEY").toplevel().await?;
    match command {
        ChangeCommand::Validate { ids } => {
            let ids = if ids.is_empty() {
                discover(&top)?
            } else {
                ids.clone()
            };
            let files: Vec<Value> = ids
                .iter()
                .map(|id| {
                    let (report, _) = check(&top, id);
                    json!({
                        "file": format!("{CHANGES}/{id}"),
                        "kind": "change",
                        "ok": report.errors.is_empty(),
                        "errors": report.errors,
                        "warnings": report.warnings,
                    })
                })
                .collect();
            let ok = files.iter().all(|file| file["ok"] == true);
            output
                .emit(json!({"type": "change_validation", "ok": ok, "files": files}))
                .await?;
            Ok(if ok { 0 } else { 1 })
        }
        ChangeCommand::Archive { id } => {
            let secs = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_secs();
            let (year, month, day) = crate::retention::civil_from_days((secs / 86_400) as i64);
            let event = archive(&top, id, &format!("{year:04}-{month:02}-{day:02}"))?;
            output.emit(event).await?;
            Ok(0)
        }
    }
}

/// Every change directory except `archive/`, sorted.
fn discover(top: &Path) -> Result<Vec<String>> {
    let ids: Vec<String> = dirs(&top.join(CHANGES))
        .into_iter()
        .filter(|name| name != "archive")
        .collect();
    if ids.is_empty() {
        bail!("found no changes under {CHANGES}/; pass the change ids to check");
    }
    Ok(ids)
}

/// The subdirectory names of `path`, sorted.
fn dirs(path: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(path)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|entry| entry.path().is_dir())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// The change id of an archive directory, `YYYY-MM-DD-<id>`.
fn archived_id(name: &str) -> Option<String> {
    let date = name.get(..11)?.as_bytes();
    let shape = date.iter().enumerate().all(|(i, b)| match i {
        4 | 7 | 10 => *b == b'-',
        _ => b.is_ascii_digit(),
    });
    (shape && name.len() > 11).then(|| name[11..].to_owned())
}

/// The `depends_on` of a `change.md`, in order and without repeats,
/// leaving out each change archived in `archive`, the names of the
/// `goodflows/changes/archive/` directories at the same commit. `None` when
/// the text has no readable front matter.
pub fn dependencies_at(change_md: &str, archive: &[String]) -> Option<Vec<String>> {
    let fields = front_matter(change_md, "", &mut Vec::new())?;
    let archived: BTreeSet<String> = archive.iter().filter_map(|n| archived_id(n)).collect();
    let mut ids: Vec<String> = Vec::new();
    for id in fields["depends_on"].as_array().into_iter().flatten() {
        if let Some(id) = id.as_str() {
            if !archived.contains(id) && !ids.iter().any(|known| known == id) {
                ids.push(id.to_owned());
            }
        }
    }
    Some(ids)
}

/// Every active change with its `depends_on` and spec deltas, as read from
/// disk, and the ids of archived changes. A change's own problems are left
/// to its own check.
struct Graph {
    active: BTreeMap<String, (Vec<String>, Vec<Delta>)>,
    archived: BTreeSet<String>,
}

impl Graph {
    fn read(top: &Path) -> Self {
        let root = top.join(CHANGES);
        let mut active = BTreeMap::new();
        for id in dirs(&root).into_iter().filter(|id| id != "archive") {
            let dir = root.join(&id);
            let depends_on = std::fs::read_to_string(dir.join("change.md"))
                .ok()
                .and_then(|text| front_matter(&text, "", &mut Vec::new()))
                .map(|fields| {
                    fields["depends_on"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let deltas = deltas(&dir, &format!("{CHANGES}/{id}"), &mut Vec::new());
            active.insert(id, (depends_on, deltas));
        }
        let archived = dirs(&root.join("archive"))
            .iter()
            .filter_map(|name| archived_id(name))
            .collect();
        Graph { active, archived }
    }

    fn dependencies(&self, id: &str) -> &[String] {
        self.active.get(id).map_or(&[], |(deps, _)| deps.as_slice())
    }

    /// The active changes `id` depends on, directly or through others:
    /// each after its own dependencies, otherwise in id order.
    fn order(&self, id: &str) -> Vec<String> {
        fn visit(graph: &Graph, id: &str, seen: &mut BTreeSet<String>, out: &mut Vec<String>) {
            let mut deps: Vec<&String> = graph.dependencies(id).iter().collect();
            deps.sort();
            for dep in deps {
                if graph.active.contains_key(dep) && seen.insert(dep.clone()) {
                    visit(graph, dep, seen, out);
                    out.push(dep.clone());
                }
            }
        }
        let mut out = Vec::new();
        visit(self, id, &mut BTreeSet::from([id.to_owned()]), &mut out);
        out
    }

    /// A path of active changes from `id` back to `id`, if there is one.
    fn cycle(&self, id: &str) -> Option<Vec<String>> {
        fn walk(
            graph: &Graph,
            target: &str,
            path: &mut Vec<String>,
            seen: &mut BTreeSet<String>,
        ) -> bool {
            let at = path.last().cloned().unwrap_or_default();
            let mut deps: Vec<&String> = graph.dependencies(&at).iter().collect();
            deps.sort();
            for dep in deps {
                if dep == target {
                    path.push(dep.clone());
                    return true;
                }
                if graph.active.contains_key(dep) && seen.insert(dep.clone()) {
                    path.push(dep.clone());
                    if walk(graph, target, path, seen) {
                        return true;
                    }
                    path.pop();
                }
            }
            false
        }
        let mut path = vec![id.to_owned()];
        walk(self, id, &mut path, &mut BTreeSet::new()).then_some(path)
    }

    /// The first active change, other than `id` and those in `skip`, whose
    /// delta adds requirement `name` to `capability` or renames one to it.
    fn adder(&self, id: &str, skip: &[String], capability: &str, name: &str) -> Option<String> {
        self.active
            .iter()
            .filter(|(other, _)| *other != id && !skip.contains(other))
            .find(|(_, (_, deltas))| {
                deltas
                    .iter()
                    .filter(|d| d.capability == capability)
                    .any(|d| {
                        d.requirements
                            .iter()
                            .any(|r| r.op == Op::Added && r.name == name)
                            || d.renames.iter().any(|(_, to)| to == name)
                    })
            })
            .map(|(other, _)| other.clone())
    }
}

/// `goodflows/specs/<capability>/spec.md` with the deltas of the active
/// dependencies in `order` applied, and whether the spec file is new. A
/// dependency delta that does not apply is that change's own error and is
/// skipped here.
fn base_spec(
    top: &Path,
    graph: &Graph,
    order: &[String],
    capability: &str,
) -> Result<(Spec, bool), String> {
    let shown = format!("{SPECS}/{capability}/spec.md");
    let (mut spec, created) = match std::fs::read_to_string(top.join(&shown)) {
        Ok(text) => (
            Spec::parse(&text)
                .ok_or_else(|| format!("{shown}: has no \"## Requirements\" heading"))?,
            false,
        ),
        Err(_) => (Spec::new(capability), true),
    };
    for dep in order {
        let deltas = graph.active.get(dep).map_or(&[][..], |(_, d)| d.as_slice());
        for delta in deltas.iter().filter(|d| d.capability == capability) {
            if let Ok((merged, _)) = spec.clone().merge(delta, &shown, &|_| None) {
                spec = merged;
            }
        }
    }
    Ok((spec, created))
}

#[derive(Default)]
struct Report {
    errors: Vec<String>,
    warnings: Vec<String>,
}

/// What archiving needs from a change that validated.
struct Checked {
    status: String,
    text: String,
    merged: Vec<Merged>,
}

/// A capability spec after the change's delta is applied.
struct Merged {
    capability: String,
    created: bool,
    spec: Spec,
    counts: [usize; 4],
}

#[derive(Clone, Copy, PartialEq)]
enum Op {
    Added,
    Modified,
    Removed,
    Renamed,
}

struct Requirement {
    op: Op,
    name: String,
    refs: Option<Vec<String>>,
    scenarios: Vec<String>,
    /// The block from its heading to the next heading, verbatim.
    text: String,
}

struct Delta {
    capability: String,
    file: String,
    requirements: Vec<Requirement>,
    renames: Vec<(String, String)>,
}

/// One change's problems, and what archiving needs when it has none.
fn check(top: &Path, id: &str) -> (Report, Option<Checked>) {
    let mut report = Report::default();
    let base = format!("{CHANGES}/{id}");
    let dir = top.join(CHANGES).join(id);
    if !dir.is_dir() {
        report
            .errors
            .push(format!("{base}: no such change directory"));
        return (report, None);
    }
    let file = format!("{base}/change.md");
    let text = match std::fs::read_to_string(dir.join("change.md")) {
        Ok(text) => text,
        Err(_) => {
            report.errors.push(format!(
                "{file}: missing or not UTF-8; every change needs a change.md"
            ));
            String::new()
        }
    };
    let meta = front_matter(&text, &file, &mut report.errors)
        .map(|fields| meta(&fields, id, &file, &mut report));

    let deltas = deltas(&dir, &base, &mut report.errors);
    let graph = Graph::read(top);
    let order = graph.order(id);
    if let Some(meta) = &meta {
        for dep in &meta.depends_on {
            if !graph.active.contains_key(dep) && !graph.archived.contains(dep) {
                report.errors.push(format!(
                    "{file}: depends_on names {dep}, which is neither an active change nor archived in {CHANGES}/archive/"
                ));
            }
        }
    }
    if let Some(path) = graph.cycle(id) {
        report.errors.push(format!(
            "{file}: depends_on makes a cycle: {}",
            path.join(" → ")
        ));
    }
    if let Some(meta) = &meta {
        let found: BTreeSet<&str> = deltas.iter().map(|d| d.capability.as_str()).collect();
        for capability in &meta.capabilities {
            if !found.contains(capability.as_str()) {
                report.errors.push(format!(
                    "{file}: capability {capability} has no delta at {base}/specs/{capability}/spec.md"
                ));
            }
        }
        for delta in &deltas {
            if !meta.capabilities.contains(&delta.capability) {
                report.errors.push(format!(
                    "{}: capability {} is not listed in change.md's capabilities",
                    delta.file, delta.capability
                ));
            }
        }
    }

    let mut merged = Vec::new();
    for delta in &deltas {
        let shown = format!("{SPECS}/{}/spec.md", delta.capability);
        let (spec, created) = match base_spec(top, &graph, &order, &delta.capability) {
            Ok(base) => base,
            Err(problem) => {
                report.errors.push(problem);
                continue;
            }
        };
        let added_by = |name: &str| graph.adder(id, &order, &delta.capability, name);
        match spec.merge(delta, &shown, &added_by) {
            Ok((spec, counts)) => merged.push(Merged {
                capability: delta.capability.clone(),
                created,
                spec,
                counts,
            }),
            Err(problems) => report.errors.extend(problems),
        }
    }

    // Refs: every requirement cites goals or decisions the change defines.
    let mut cited = BTreeSet::new();
    for delta in &deltas {
        for requirement in &delta.requirements {
            let at = format!("{}: requirement \"{}\"", delta.file, requirement.name);
            match &requirement.refs {
                None => report.errors.push(format!(
                    "{at} has no Refs: line naming the goals or decisions it comes from"
                )),
                Some(refs) => {
                    for r in refs {
                        if meta.as_ref().is_some_and(|m| !m.ids.contains_key(r)) {
                            report.errors.push(format!("{at} cites unknown id {r}"));
                        }
                        cited.insert(r.clone());
                    }
                }
            }
            if requirement.scenarios.len() > 3 {
                report.warnings.push(format!(
                    "{at} has {} scenarios; keep spec scenarios to the boundaries and put the rest in qa-scenarios.md",
                    requirement.scenarios.len()
                ));
            }
        }
    }
    if let Some(meta) = &meta {
        for (id, kind) in &meta.ids {
            if cited.contains(id) {
                continue;
            }
            match kind.as_str() {
                "user" => report.errors.push(format!(
                    "{file}: decision {id} was made by the user, but no requirement cites it"
                )),
                "agent" => report.warnings.push(format!(
                    "{file}: decision {id} is cited by no requirement; it may decide something the builder could"
                )),
                "goal" => report
                    .warnings
                    .push(format!("{file}: goal {id} is cited by no requirement")),
                _ => {}
            }
        }
        let count: usize = deltas.iter().map(|d| d.requirements.len()).sum();
        let budget = if meta.size == "small" { 8 } else { 20 };
        if count > budget {
            report.warnings.push(format!(
                "{base}: {count} requirements is more than {budget} for a {} change; check for over-specification or split the change",
                meta.size
            ));
        }
    }

    scenarios(
        &dir,
        &base,
        id,
        &deltas,
        &merged,
        (top, &graph, &order),
        &mut report.errors,
    );
    leftovers(&dir, &base, &mut report.errors);

    let checked = (report.errors.is_empty()).then(|| Checked {
        status: meta.map(|m| m.status).unwrap_or_default(),
        text,
        merged,
    });
    (report, checked)
}

struct Meta {
    status: String,
    depends_on: Vec<String>,
    size: String,
    capabilities: Vec<String>,
    /// Goal and decision ids: `goal`, `non_goal`, `anti_goal`, or the
    /// decision's `by`.
    ids: BTreeMap<String, String>,
}

fn front_matter(text: &str, file: &str, errors: &mut Vec<String>) -> Option<Value> {
    if text.is_empty() {
        return None;
    }
    let Some(front) = markdown::split(text) else {
        errors.push(format!(
            "{file}: must start with YAML front matter between --- lines"
        ));
        return None;
    };
    let parsed =
        markdown::refuse_anchors(front.yaml, file).and_then(|()| markdown::parse(front.yaml, file));
    match parsed {
        Ok(fields) => Some(fields),
        Err(error) => {
            errors.extend(markdown::problems(error));
            None
        }
    }
}

fn meta(fields: &Value, dir_id: &str, file: &str, report: &mut Report) -> Meta {
    let errors = &mut report.errors;
    for key in fields.as_object().into_iter().flat_map(|o| o.keys()) {
        if !KEYS.contains(&key.as_str()) {
            errors.push(format!("{file}: unknown field {key}"));
        }
    }
    for key in KEYS {
        if fields.get(key).is_none() {
            errors.push(format!("{file}: {key} is required"));
        }
    }
    if fields["format"] != FORMAT {
        errors.push(format!("{file}: format must be {FORMAT}"));
    }
    if fields["id"] != dir_id {
        errors.push(format!(
            "{file}: id must be {dir_id}, the change's directory name"
        ));
    }
    let mut pick = |key: &str, allowed: &[&str]| -> String {
        match fields[key].as_str() {
            Some(value) if allowed.contains(&value) => value.to_owned(),
            _ => {
                if fields.get(key).is_some() {
                    errors.push(format!("{file}: {key} must be {}", allowed.join(" or ")));
                }
                String::new()
            }
        }
    };
    let status = pick("status", &["proposed", "approved", "archived"]);
    let size = pick("size", &["small", "project"]);
    pick("door", &["one_way", "two_way"]);

    let mut capabilities = Vec::new();
    for value in list(fields, "capabilities", file, errors) {
        match value.as_str().filter(|c| capability_name(c)) {
            Some(c) if capabilities.iter().any(|known| known == c) => {
                errors.push(format!("{file}: capability {c} is listed twice"));
            }
            Some(c) => capabilities.push(c.to_owned()),
            None => errors.push(format!(
                "{file}: capabilities must be names such as change-requests or area/name (a-z, 0-9 and -)"
            )),
        }
    }
    let mut depends_on = Vec::new();
    for value in list(fields, "depends_on", file, errors) {
        match value.as_str().map(super::change_id) {
            Some(Ok(id)) => depends_on.push(id),
            _ => errors.push(format!("{file}: depends_on must list change ids")),
        }
    }

    let mut ids = BTreeMap::new();
    for (key, kind) in [
        ("goals", "goal"),
        ("non_goals", "non_goal"),
        ("anti_goals", "anti_goal"),
    ] {
        for (index, item) in list(fields, key, file, errors).iter().enumerate() {
            let at = format!("{file}: {key}[{index}]");
            object(item, &["id", "text"], &[], &at, errors);
            if let Some(id) = item_id(item, &at, errors) {
                add_id(&mut ids, id, kind, file, errors);
            }
        }
    }
    for (index, item) in list(fields, "decisions", file, errors).iter().enumerate() {
        let at = format!("{file}: decisions[{index}]");
        object(
            item,
            &["id", "question", "answer", "by", "why"],
            &["rejected"],
            &at,
            errors,
        );
        let by = match item["by"].as_str() {
            Some(by @ ("user" | "agent")) => by,
            _ => {
                errors.push(format!("{at}: by must be user or agent"));
                "agent"
            }
        };
        if item.get("rejected").is_some_and(|r| {
            !r.as_array().is_some_and(|r| {
                r.iter()
                    .all(|o| o.as_str().is_some_and(|o| !o.trim().is_empty()))
            })
        }) {
            errors.push(format!("{at}: rejected must list the options not chosen"));
        }
        if let Some(id) = item_id(item, &at, errors) {
            add_id(&mut ids, id, by, file, errors);
        }
    }
    let left = list(fields, "left_to_builder", file, errors);
    if left
        .iter()
        .any(|item| item.as_str().is_none_or(|s| s.trim().is_empty()))
    {
        errors.push(format!(
            "{file}: left_to_builder must list nonempty strings"
        ));
    }
    if left.is_empty() && fields.get("left_to_builder").is_some() {
        report.warnings.push(format!(
            "{file}: left_to_builder is empty; name what the builder decides, so silence never means forgotten"
        ));
    }
    Meta {
        status,
        depends_on,
        size,
        capabilities,
        ids,
    }
}

fn list<'a>(fields: &'a Value, key: &str, file: &str, errors: &mut Vec<String>) -> &'a [Value] {
    match &fields[key] {
        Value::Array(items) => items,
        Value::Null => &[],
        _ => {
            errors.push(format!("{file}: {key} must be a list"));
            &[]
        }
    }
}

/// An object with the `required` keys as nonempty strings and nothing but
/// those and the `optional` keys.
fn object(item: &Value, required: &[&str], optional: &[&str], at: &str, errors: &mut Vec<String>) {
    let Some(map) = item.as_object() else {
        errors.push(format!("{at}: must be a mapping"));
        return;
    };
    for key in map.keys() {
        if !required.contains(&key.as_str()) && !optional.contains(&key.as_str()) {
            errors.push(format!("{at}: unknown field {key}"));
        }
    }
    for key in required {
        if *key != "by"
            && map
                .get(*key)
                .and_then(Value::as_str)
                .is_none_or(|s| s.trim().is_empty())
        {
            errors.push(format!("{at}: {key} must be a nonempty string"));
        }
    }
}

fn item_id<'a>(item: &'a Value, at: &str, errors: &mut Vec<String>) -> Option<&'a str> {
    let id = item["id"].as_str()?;
    let ok = id.len() <= 32
        && id.starts_with(|c: char| c.is_ascii_alphanumeric())
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        errors.push(format!(
            "{at}: id must be up to 32 letters, digits, ., _ or -, such as G1 or D2"
        ));
        return None;
    }
    Some(id)
}

fn add_id(
    ids: &mut BTreeMap<String, String>,
    id: &str,
    kind: &str,
    file: &str,
    errors: &mut Vec<String>,
) {
    if ids.insert(id.to_owned(), kind.to_owned()).is_some() {
        errors.push(format!(
            "{file}: id {id} is used twice; goal and decision ids share one namespace"
        ));
    }
}

fn capability_name(name: &str) -> bool {
    name.split('/').all(|segment| {
        segment.starts_with(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit())
            && segment
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    })
}

/// Every file under `dir`, sorted.
fn files(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for path in entries {
        if path.is_dir() {
            found.extend(files(&path));
        } else {
            found.push(path);
        }
    }
    found
}

/// The change's `specs/<capability>/spec.md` deltas.
fn deltas(dir: &Path, base: &str, errors: &mut Vec<String>) -> Vec<Delta> {
    let specs = dir.join("specs");
    let mut deltas = Vec::new();
    for path in files(&specs) {
        let Ok(relative) = path.strip_prefix(&specs) else {
            continue;
        };
        let relative = relative.to_string_lossy().replace('\\', "/");
        let file = format!("{base}/specs/{relative}");
        let Some(capability) = relative.strip_suffix("/spec.md") else {
            errors.push(format!(
                "{file}: specs/ holds only <capability>/spec.md deltas"
            ));
            continue;
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => deltas.push(parse_delta(&text, capability, &file, errors)),
            Err(_) => errors.push(format!("{file}: not UTF-8")),
        }
    }
    deltas
}

/// The file's lines without their endings, with byte offsets and whether
/// they are part of a fenced code block.
fn lines(text: &str) -> Vec<(usize, &str, bool)> {
    let mut out = Vec::new();
    let mut offset = 0;
    let mut fence: Option<&str> = None;
    for raw in text.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\n', '\r']);
        let trimmed = line.trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        let code = fence.is_some() || marker.is_some();
        match (fence, marker) {
            (Some(open), Some(m)) if m == open => fence = None,
            (None, Some(m)) => fence = Some(m),
            _ => {}
        }
        out.push((offset, line, code));
        offset += raw.len();
    }
    out
}

fn heading(line: &str) -> Option<(usize, &str)> {
    let level = line.bytes().take_while(|b| *b == b'#').count();
    ((1..=6).contains(&level) && line[level..].starts_with(' '))
        .then(|| (level, line[level..].trim()))
}

fn requirement_name(title: &str) -> Option<&str> {
    title
        .strip_prefix("Requirement:")
        .map(str::trim)
        .filter(|n| !n.is_empty())
}

fn parse_delta(text: &str, capability: &str, file: &str, errors: &mut Vec<String>) -> Delta {
    let mut delta = Delta {
        capability: capability.to_owned(),
        file: file.to_owned(),
        requirements: Vec::new(),
        renames: Vec::new(),
    };
    let mut op = None;
    // The open requirement, its start offset, and whether a line after its
    // heading has been seen (only the first may be Refs:).
    let mut open: Option<(Requirement, usize, bool)> = None;
    let mut from: Option<String> = None;
    let close =
        |open: &mut Option<(Requirement, usize, bool)>, end: usize, into: &mut Vec<Requirement>| {
            if let Some((mut requirement, start, _)) = open.take() {
                requirement.text = text[start..end].to_owned();
                into.push(requirement);
            }
        };
    for (offset, line, code) in lines(text) {
        if let Some((level, title)) = heading(line).filter(|(level, _)| !code && *level <= 3) {
            close(&mut open, offset, &mut delta.requirements);
            match level {
                2 => {
                    op = match title {
                        "ADDED Requirements" => Some(Op::Added),
                        "MODIFIED Requirements" => Some(Op::Modified),
                        "REMOVED Requirements" => Some(Op::Removed),
                        "RENAMED Requirements" => Some(Op::Renamed),
                        _ => {
                            errors.push(format!("{file}: unknown section \"## {title}\"; a delta has ADDED, MODIFIED, REMOVED and RENAMED Requirements sections"));
                            None
                        }
                    }
                }
                3 => match (op, requirement_name(title)) {
                    (Some(Op::Renamed), _) => errors.push(format!(
                        "{file}: RENAMED Requirements lists - FROM: and - TO: lines, not ### headings"
                    )),
                    (Some(op), Some(name)) => {
                        open = Some((
                            Requirement {
                                op,
                                name: name.to_owned(),
                                refs: None,
                                scenarios: Vec::new(),
                                text: String::new(),
                            },
                            offset,
                            false,
                        ))
                    }
                    (None, _) => errors.push(format!(
                        "{file}: \"### {title}\" is outside an ADDED, MODIFIED or REMOVED Requirements section"
                    )),
                    (Some(_), None) => errors.push(format!(
                        "{file}: \"### {title}\" must be \"### Requirement: <name>\""
                    )),
                },
                _ => {}
            }
            continue;
        }
        if let Some((requirement, _, seen)) = &mut open {
            if !code && !*seen && !line.trim().is_empty() {
                *seen = true;
                if let Some(refs) = line.trim().strip_prefix("Refs:") {
                    requirement.refs = Some(
                        refs.split(',')
                            .map(|r| r.trim().to_owned())
                            .filter(|r| !r.is_empty())
                            .collect(),
                    );
                }
            }
            if let Some((4, title)) = heading(line).filter(|_| !code) {
                if let Some(name) = title.strip_prefix("Scenario:") {
                    requirement.scenarios.push(name.trim().to_owned());
                }
            }
        } else if op == Some(Op::Renamed) && !code {
            let item = line.trim().trim_start_matches(['-', '*']).trim();
            let name = |rest: &str| {
                requirement_name(
                    rest.trim()
                        .trim_matches('`')
                        .trim()
                        .trim_start_matches("###")
                        .trim(),
                )
                .map(str::to_owned)
            };
            if let Some(rest) = item.strip_prefix("FROM:") {
                if from.is_some() {
                    errors.push(format!("{file}: a RENAMED FROM: line has no TO: line"));
                }
                from = name(rest);
                if from.is_none() {
                    errors.push(format!(
                        "{file}: RENAMED lines read - FROM: `### Requirement: <old name>`"
                    ));
                }
            } else if let Some(rest) = item.strip_prefix("TO:") {
                match (from.take(), name(rest)) {
                    (Some(old), Some(new)) => delta.renames.push((old, new)),
                    _ => errors.push(format!(
                        "{file}: each RENAMED - TO: `### Requirement: <new name>` line follows its - FROM: line"
                    )),
                }
            }
        }
    }
    close(&mut open, text.len(), &mut delta.requirements);
    if from.is_some() {
        errors.push(format!("{file}: a RENAMED FROM: line has no TO: line"));
    }
    if delta.requirements.is_empty() && delta.renames.is_empty() {
        errors.push(format!("{file}: the delta changes no requirement"));
    }
    delta
}

/// A durable capability spec: the text up to its first requirement, the
/// `### ` blocks under `## Requirements`, and the text after them.
#[derive(Clone)]
struct Spec {
    head: String,
    blocks: Vec<(Option<String>, String)>,
    tail: String,
}

impl Spec {
    fn new(capability: &str) -> Self {
        Spec {
            head: format!("# {capability} Specification\n\n## Requirements\n"),
            blocks: Vec::new(),
            tail: String::new(),
        }
    }

    fn parse(text: &str) -> Option<Self> {
        let lines = lines(text);
        let start = lines
            .iter()
            .position(|(_, line, code)| !code && heading(line) == Some((2, "Requirements")))?;
        let mut cuts = Vec::new();
        let mut tail = text.len();
        for (offset, line, code) in &lines[start + 1..] {
            match heading(line).filter(|_| !code) {
                Some((level, _)) if level <= 2 => {
                    tail = *offset;
                    break;
                }
                Some((3, title)) => {
                    cuts.push((*offset, requirement_name(title).map(str::to_owned)))
                }
                _ => {}
            }
        }
        let head_end = cuts.first().map_or(tail, |(offset, _)| *offset);
        let blocks = cuts
            .iter()
            .enumerate()
            .map(|(i, (offset, name))| {
                let end = cuts.get(i + 1).map_or(tail, |(next, _)| *next);
                (name.clone(), text[*offset..end].to_owned())
            })
            .collect();
        Some(Spec {
            head: text[..head_end].to_owned(),
            blocks,
            tail: text[tail..].to_owned(),
        })
    }

    fn render(&self) -> String {
        fn gap(out: &mut String) {
            if !out.is_empty() && !out.ends_with("\n\n") {
                out.push_str(if out.ends_with('\n') { "\n" } else { "\n\n" });
            }
        }
        let mut out = self.head.clone();
        for (_, text) in &self.blocks {
            gap(&mut out);
            out.push_str(text);
        }
        if !self.tail.is_empty() {
            gap(&mut out);
            out.push_str(&self.tail);
        }
        out
    }

    fn find(&self, name: &str) -> Option<usize> {
        self.blocks
            .iter()
            .position(|(n, _)| n.as_deref() == Some(name))
    }

    /// The spec with `delta` applied in OpenSpec's order (RENAMED, REMOVED,
    /// MODIFIED, ADDED), and how many of each it applied; or every target
    /// that is missing or already taken. `added_by` names the active change
    /// that adds a missing target, for the hint.
    fn merge(
        mut self,
        delta: &Delta,
        shown: &str,
        added_by: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(Spec, [usize; 4]), Vec<String>> {
        let mut problems = Vec::new();
        let file = &delta.file;
        let missing = |problems: &mut Vec<String>, op: &str, name: &str| {
            let hint = added_by(name).map_or(String::new(), |change| {
                format!(
                    "; active change {change} adds this requirement; add {change} to depends_on"
                )
            });
            problems.push(format!(
                "{file}: {op} requirement \"{name}\" is not in {shown}{hint}"
            ))
        };
        for (from, to) in &delta.renames {
            match self.find(from) {
                Some(_) if self.find(to).is_some() => problems.push(format!(
                    "{}: RENAMED requirement \"{from}\" to \"{to}\", which {shown} already has",
                    delta.file
                )),
                Some(i) => {
                    let text = &self.blocks[i].1;
                    let rest = text.find('\n').map_or("\n", |n| &text[n..]);
                    self.blocks[i] = (Some(to.clone()), format!("### Requirement: {to}{rest}"));
                }
                None => missing(&mut problems, "RENAMED", from),
            }
        }
        let of = |op: Op| delta.requirements.iter().filter(move |r| r.op == op);
        for requirement in of(Op::Removed) {
            match self.find(&requirement.name) {
                Some(i) => drop(self.blocks.remove(i)),
                None => missing(&mut problems, "REMOVED", &requirement.name),
            }
        }
        for requirement in of(Op::Modified) {
            match self.find(&requirement.name) {
                Some(i) => self.blocks[i].1 = merged_text(requirement),
                None => missing(&mut problems, "MODIFIED", &requirement.name),
            }
        }
        for requirement in of(Op::Added) {
            if self.find(&requirement.name).is_some() {
                problems.push(format!(
                    "{}: ADDED requirement \"{}\" is already in {shown} (or added twice); use MODIFIED",
                    delta.file, requirement.name
                ));
            } else {
                self.blocks
                    .push((Some(requirement.name.clone()), merged_text(requirement)));
            }
        }
        if !problems.is_empty() {
            return Err(problems);
        }
        let counts = [
            of(Op::Added).count(),
            of(Op::Modified).count(),
            of(Op::Removed).count(),
            delta.renames.len(),
        ];
        Ok((self, counts))
    }

    /// The `#### Scenario:` names of a requirement.
    fn scenarios(&self, name: &str) -> Vec<String> {
        let Some(i) = self.find(name) else {
            return Vec::new();
        };
        lines(&self.blocks[i].1)
            .into_iter()
            .filter(|(_, _, code)| !code)
            .filter_map(|(_, line, _)| match heading(line) {
                Some((4, title)) => title.strip_prefix("Scenario:").map(|s| s.trim().to_owned()),
                _ => None,
            })
            .collect()
    }
}

/// A delta requirement as the durable spec keeps it: without its `Refs:`
/// line, whose ids mean something only inside the change.
fn merged_text(requirement: &Requirement) -> String {
    let mut out = String::new();
    let mut dropped = false;
    for (_, line, code) in lines(&requirement.text) {
        if !dropped && !code && line.trim().starts_with("Refs:") {
            dropped = true;
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    format!("{}\n", out.trim_end())
}

/// Checks `qa-scenarios.md`: a valid intent set for this change whose Must
/// scenarios cover every added and modified requirement.
fn scenarios(
    dir: &Path,
    base: &str,
    id: &str,
    deltas: &[Delta],
    merged: &[Merged],
    (top, graph, order): (&Path, &Graph, &[String]),
    errors: &mut Vec<String>,
) {
    let file = format!("{base}/qa-scenarios.md");
    let Ok(text) = std::fs::read_to_string(dir.join("qa-scenarios.md")) else {
        errors.push(format!(
            "{file}: missing; every change needs an intent scenario set"
        ));
        return;
    };
    match markdown::scenario_set(&text, &file) {
        Ok(set) => {
            if set.source != "intent" {
                errors.push(format!("{file}: source must be intent"));
            }
            if let Err(error) = markdown::sections(&text, &set.ids, &file) {
                errors.extend(markdown::problems(error));
            }
        }
        Err(error) => errors.extend(markdown::problems(error)),
    }
    let Some(fields) = front_matter(&text, &file, &mut Vec::new()) else {
        return;
    };
    if fields["change"] != id {
        errors.push(format!(
            "{file}: change must be {id}, the id the change request links"
        ));
    }
    let mut other: BTreeMap<String, Option<Spec>> = BTreeMap::new();
    let mut covered = BTreeSet::new();
    for (index, scenario) in fields["scenarios"]
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
    {
        let at = format!("{file}: scenarios[{index}]");
        for cover in scenario["covers"].as_array().into_iter().flatten() {
            let (Some(capability), Some(requirement)) =
                (cover["capability"].as_str(), cover["requirement"].as_str())
            else {
                errors.push(format!(
                    "{at}: each covers entry names a capability and a requirement"
                ));
                continue;
            };
            let spec = match merged.iter().find(|m| m.capability == capability) {
                Some(m) => Some(&m.spec),
                None => other
                    .entry(capability.to_owned())
                    .or_insert_with(|| {
                        base_spec(top, graph, order, capability)
                            .ok()
                            .map(|(spec, _)| spec)
                    })
                    .as_ref(),
            };
            if spec.is_none_or(|s| s.find(requirement).is_none()) {
                errors.push(format!(
                    "{at}: covers {capability}: \"{requirement}\", which neither this change nor {SPECS} has"
                ));
                continue;
            }
            if let Some(name) = cover["scenario"].as_str() {
                let delta = deltas.iter().find(|d| d.capability == capability);
                let names = delta
                    .and_then(|d| d.requirements.iter().find(|r| r.name == requirement))
                    .map_or_else(
                        || spec.map_or_else(Vec::new, |s| s.scenarios(requirement)),
                        |r| r.scenarios.clone(),
                    );
                if !names.iter().any(|n| n == name) {
                    errors.push(format!(
                        "{at}: covers scenario \"{name}\", which requirement \"{requirement}\" does not have; use implied: true"
                    ));
                }
            } else if cover["implied"] != true {
                errors.push(format!(
                    "{at}: each covers entry has a scenario name or implied: true"
                ));
            }
            if scenario["priority"] == "must" {
                covered.insert((capability.to_owned(), requirement.to_owned()));
            }
        }
    }
    for delta in deltas {
        for requirement in &delta.requirements {
            if requirement.op != Op::Removed
                && !covered.contains(&(delta.capability.clone(), requirement.name.clone()))
            {
                errors.push(format!(
                    "{}: requirement \"{}\" is covered by no Must scenario in qa-scenarios.md",
                    delta.file, requirement.name
                ));
            }
        }
    }
}

/// `TBD` and `TODO` left anywhere in the change.
fn leftovers(dir: &Path, base: &str, errors: &mut Vec<String>) {
    for path in files(dir) {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let shown = path
            .strip_prefix(dir)
            .map_or(String::new(), |p| p.to_string_lossy().into_owned());
        for (number, line) in text.lines().enumerate() {
            let words = line.split(|c: char| !c.is_ascii_alphanumeric());
            if words.into_iter().any(|w| w == "TBD" || w == "TODO") {
                errors.push(format!(
                    "{base}/{shown}:{}: TBD or TODO; decide it, or name it in left_to_builder",
                    number + 1
                ));
            }
        }
    }
}

/// Merges the change into `goodflows/specs/` and moves it to the archive.
fn archive(top: &Path, id: &str, date: &str) -> Result<Value> {
    let (report, checked) = check(top, id);
    let Some(checked) = checked else {
        let mut problems = vec![format!(
            "change {id} has errors; nothing was archived (goodflows change validate {id} lists them)"
        )];
        problems.extend(report.errors);
        return Err(Invalid(problems).into());
    };
    if checked.status != "approved" {
        bail!(
            "change {id} is {}; only an approved change is archived",
            checked.status
        );
    }
    let graph = Graph::read(top);
    let active: Vec<&str> = graph
        .dependencies(id)
        .iter()
        .filter(|dep| graph.active.contains_key(*dep))
        .map(String::as_str)
        .collect();
    if !active.is_empty() {
        bail!(
            "change {id} depends on {}, which is still active; archive it first",
            active.join(", ")
        );
    }
    let destination = format!("{CHANGES}/archive/{date}-{id}");
    if top.join(&destination).exists() {
        bail!("{destination} already exists");
    }
    let file = format!("{CHANGES}/{id}/change.md");
    let text = markdown::set_line(&checked.text, "status", "archived", &file)?;
    let mut specs = Vec::new();
    for merged in &checked.merged {
        let path = format!("{SPECS}/{}/spec.md", merged.capability);
        let absolute = top.join(&path);
        if let Some(parent) = absolute.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&absolute, merged.spec.render())?;
        let [added, modified, removed, renamed] = merged.counts;
        specs.push(json!({
            "capability": merged.capability,
            "path": path,
            "created": merged.created,
            "added": added,
            "modified": modified,
            "removed": removed,
            "renamed": renamed,
        }));
    }
    let dir = top.join(CHANGES).join(id);
    std::fs::write(dir.join("change.md"), text)?;
    std::fs::create_dir_all(top.join(CHANGES).join("archive"))?;
    std::fs::rename(&dir, top.join(&destination))?;
    Ok(json!({
        "type": "change_archive",
        "id": id,
        "archived_to": destination,
        "specs": specs,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    const CHANGE: &str = "---
format: goodflows-change/1
id: add-login
status: approved
size: small
door: two_way
capabilities: [auth]
depends_on: []
goals: [{id: G1, text: People sign in with email}]
non_goals: []
anti_goals: []
decisions:
  - {id: D1, question: How long do sessions last?, answer: 30 days, by: user, why: matches the mobile app}
  - {id: D2, question: Where do failures go?, answer: back to the form, by: agent, why: simplest}
left_to_builder: [The form's layout]
---

## Why

Sign in.
";

    const DELTA: &str = "## ADDED Requirements

### Requirement: Sessions last 30 days
Refs: D1, G1
A session SHALL expire 30 days after sign in.

#### Scenario: Day 31
- **WHEN** a session is 31 days old
- **THEN** the user signs in again

## MODIFIED Requirements

### Requirement: Sign out
Refs: G1
Signing out SHALL end every session of the user.

## REMOVED Requirements

### Requirement: Remember me
Refs: D1

## RENAMED Requirements

- FROM: `### Requirement: Login`
- TO: `### Requirement: Sign in`
";

    const SPEC: &str = "# auth Specification

## Purpose

Auth.

## Requirements

### Requirement: Login
People log in.

### Requirement: Sign out
Signing out ends the session.

### Requirement: Remember me
A checkbox.

## Notes

Kept.
";

    const SET: &str = "---
format: qa-scenarios/2
source: intent
change: add-login
specs: uncommitted
scenarios:
  - id: 1
    name: Old sessions expire
    priority: must
    covers:
      - {capability: auth, requirement: Sessions last 30 days, scenario: Day 31}
  - id: 2
    name: Sign out ends sessions
    priority: must
    covers:
      - {capability: auth, requirement: Sign out, implied: true}
---

# QA

## Scenarios

### 1. Old sessions expire (Must)

steps

### 2. Sign out ends sessions (Must)

steps
";

    fn project(files: &[(&str, &str)]) -> tempfile::TempDir {
        let top = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let path = top.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        top
    }

    fn valid() -> Vec<(&'static str, String)> {
        vec![
            ("goodflows/changes/add-login/change.md", CHANGE.to_owned()),
            (
                "goodflows/changes/add-login/specs/auth/spec.md",
                DELTA.to_owned(),
            ),
            (
                "goodflows/changes/add-login/qa-scenarios.md",
                SET.to_owned(),
            ),
            ("goodflows/specs/auth/spec.md", SPEC.to_owned()),
        ]
    }

    fn report(files: Vec<(&str, String)>) -> Report {
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        check(project(&files).path(), "add-login").0
    }

    fn with(path: &str, from: &str, to: &str) -> Vec<(&'static str, String)> {
        let mut files = valid();
        let file = files.iter_mut().find(|(p, _)| p.ends_with(path)).unwrap();
        assert!(file.1.contains(from), "{from}");
        file.1 = file.1.replace(from, to);
        files
    }

    fn has(problems: &[String], text: &str) -> bool {
        problems.iter().any(|p| p.contains(text))
    }

    #[test]
    fn a_valid_change_warns_only_about_the_uncited_agent_decision() {
        let report = report(valid());
        assert_eq!(report.errors, Vec::<String>::new());
        assert_eq!(
            report.warnings,
            vec!["goodflows/changes/add-login/change.md: decision D2 is cited by no requirement; it may decide something the builder could"]
        );
    }

    #[test]
    fn traceability_problems_are_errors() {
        let r = report(with("auth/spec.md", "Refs: D1, G1\n", "Refs: G9\n"));
        assert!(has(
            &r.errors,
            "\"Sessions last 30 days\" cites unknown id G9"
        ));
        let r = report(with("auth/spec.md", "Refs: D1\n", ""));
        assert!(has(
            &r.errors,
            "requirement \"Remember me\" has no Refs: line"
        ));
        // D1 is the user's; once no Refs: line names it, it is an error.
        let files = with("auth/spec.md", "Refs: D1, G1\n", "Refs: G1\n")
            .into_iter()
            .map(|(p, t)| (p, t.replace("Refs: D1\n", "Refs: G1\n")))
            .collect();
        let r = report(files);
        assert!(has(
            &r.errors,
            "decision D1 was made by the user, but no requirement cites it"
        ));
    }

    #[test]
    fn every_added_and_modified_requirement_needs_a_must() {
        let r = report(with(
            "qa-scenarios.md",
            "    priority: must\n    covers:\n      - {capability: auth, requirement: Sign out",
            "    priority: should\n    covers:\n      - {capability: auth, requirement: Sign out",
        ));
        assert!(has(
            &r.errors,
            "requirement \"Sign out\" is covered by no Must scenario"
        ));
        assert!(!has(&r.errors, "\"Sessions last 30 days\" is covered"));
        assert!(
            !has(&r.errors, "\"Remember me\" is covered"),
            "removed needs no scenario"
        );
        let r = report(with(
            "qa-scenarios.md",
            "scenario: Day 31",
            "scenario: Day 32",
        ));
        assert!(has(
            &r.errors,
            "covers scenario \"Day 32\", which requirement \"Sessions last 30 days\" does not have"
        ));
        let r = report(with(
            "qa-scenarios.md",
            "requirement: Sign out,",
            "requirement: Sign off,",
        ));
        assert!(has(
            &r.errors,
            "covers auth: \"Sign off\", which neither this change nor"
        ));
    }

    #[test]
    fn delta_targets_are_checked_against_the_durable_spec() {
        let r = report(with(
            "goodflows/specs/auth/spec.md",
            "### Requirement: Remember me\nA checkbox.\n\n",
            "",
        ));
        assert!(has(
            &r.errors,
            "REMOVED requirement \"Remember me\" is not in goodflows/specs/auth/spec.md"
        ));
        let r = report(with(
            "add-login/specs/auth/spec.md",
            "### Requirement: Sessions last 30 days",
            "### Requirement: Sign out",
        ));
        assert!(has(
            &r.errors,
            "ADDED requirement \"Sign out\" is already in"
        ));
        let r = report(with(
            "add-login/specs/auth/spec.md",
            "TO: `### Requirement: Sign in`",
            "TO: `### Requirement: Sign out`",
        ));
        assert!(has(
            &r.errors,
            "RENAMED requirement \"Login\" to \"Sign out\""
        ));
    }

    #[test]
    fn front_matter_problems_are_all_listed() {
        let r = report(with(
            "change.md",
            "id: add-login\nstatus: approved\nsize: small",
            "id: add-logout\nstatus: done\nsize: small\nowner: me",
        ));
        for problem in [
            "id must be add-login",
            "status must be proposed or approved or archived",
            "unknown field owner",
        ] {
            assert!(has(&r.errors, problem), "{problem}: {:?}", r.errors);
        }
        let r = report(with(
            "change.md",
            "capabilities: [auth]",
            "capabilities: [auth, billing]",
        ));
        assert!(has(&r.errors, "capability billing has no delta"));
        let r = report(with(
            "change.md",
            "Sign in.",
            "Sign in. TODO pick a provider",
        ));
        assert!(has(&r.errors, "change.md:20: TBD or TODO"));
    }

    #[test]
    fn archive_merges_in_order_and_moves_the_change() {
        let files: Vec<(&str, String)> = valid();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let top = project(&files);
        let event = archive(top.path(), "add-login", "2026-11-25").unwrap();
        assert_eq!(
            event["archived_to"],
            "goodflows/changes/archive/2026-11-25-add-login"
        );
        assert_eq!(
            event["specs"][0],
            json!({"capability": "auth", "path": "goodflows/specs/auth/spec.md", "created": false,
                   "added": 1, "modified": 1, "removed": 1, "renamed": 1})
        );
        let spec =
            std::fs::read_to_string(top.path().join("goodflows/specs/auth/spec.md")).unwrap();
        assert_eq!(
            spec,
            "# auth Specification\n\n## Purpose\n\nAuth.\n\n## Requirements\n\n### Requirement: Sign in\nPeople log in.\n\n### Requirement: Sign out\nSigning out SHALL end every session of the user.\n\n### Requirement: Sessions last 30 days\nA session SHALL expire 30 days after sign in.\n\n#### Scenario: Day 31\n- **WHEN** a session is 31 days old\n- **THEN** the user signs in again\n\n## Notes\n\nKept.\n"
        );
        let moved = top
            .path()
            .join("goodflows/changes/archive/2026-11-25-add-login");
        assert!(std::fs::read_to_string(moved.join("change.md"))
            .unwrap()
            .contains("\nstatus: archived\n"));
        assert!(!top.path().join("goodflows/changes/add-login").exists());
    }

    #[test]
    fn archive_refuses_changing_nothing() {
        let files = with("change.md", "status: approved", "status: proposed");
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let top = project(&files);
        let error = archive(top.path(), "add-login", "2026-11-25").unwrap_err();
        assert!(error.to_string().contains("is proposed"), "{error}");
        let files = with(
            "goodflows/specs/auth/spec.md",
            "### Requirement: Login",
            "### Requirement: Log in",
        );
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let top = project(&files);
        let error = archive(top.path(), "add-login", "2026-11-25").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("RENAMED requirement \"Login\" is not in"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(top.path().join("goodflows/specs/auth/spec.md")).unwrap(),
            SPEC.replace("Requirement: Login", "Requirement: Log in")
        );
        assert!(top
            .path()
            .join("goodflows/changes/add-login/change.md")
            .is_file());
    }

    #[test]
    fn a_new_capability_spec_is_created() {
        let delta = DELTA.split("## MODIFIED").next().unwrap();
        let mut files = with("add-login/specs/auth/spec.md", DELTA, delta);
        files.retain(|(p, _)| !p.starts_with("goodflows/specs"));
        let files = files.into_iter().map(|(p, t)| (p, t.replace("      - {capability: auth, requirement: Sign out, implied: true}", "      - {capability: auth, requirement: Sessions last 30 days, implied: true}"))).collect::<Vec<_>>();
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let top = project(&files);
        let event = archive(top.path(), "add-login", "2026-11-25").unwrap();
        assert_eq!(event["specs"][0]["created"], true);
        let spec =
            std::fs::read_to_string(top.path().join("goodflows/specs/auth/spec.md")).unwrap();
        assert!(spec.starts_with("# auth Specification\n\n## Requirements\n\n### Requirement: Sessions last 30 days\nA session SHALL"), "{spec}");
    }

    const PASSKEYS: &str = "---
format: goodflows-change/1
id: add-passkeys
status: proposed
size: small
door: two_way
capabilities: [auth]
depends_on: []
goals: [{id: G1, text: Passkeys}]
non_goals: []
anti_goals: []
decisions: []
left_to_builder: [x]
---
";

    /// add-login modifies Passkeys, which active change add-passkeys adds.
    fn with_passkeys(depends_on: &str) -> Vec<(&'static str, String)> {
        let mut files = with(
            "add-login/specs/auth/spec.md",
            "## REMOVED Requirements",
            "### Requirement: Passkeys\nRefs: G1\nPasskeys SHALL also work on Android.\n\n## REMOVED Requirements",
        );
        for (path, text) in &mut files {
            if path.ends_with("add-login/change.md") {
                *text = text.replace("depends_on: []", depends_on);
            }
            if path.ends_with("add-login/qa-scenarios.md") {
                *text = text.replace(
                    "      - {capability: auth, requirement: Sign out, implied: true}",
                    "      - {capability: auth, requirement: Sign out, implied: true}\n      - {capability: auth, requirement: Passkeys, implied: true}",
                );
            }
        }
        files.push((
            "goodflows/changes/add-passkeys/change.md",
            PASSKEYS.to_owned(),
        ));
        files.push((
            "goodflows/changes/add-passkeys/specs/auth/spec.md",
            "## ADDED Requirements\n\n### Requirement: Passkeys\nRefs: G1\nPasskeys SHALL work.\n"
                .to_owned(),
        ));
        files
    }

    #[test]
    fn dependencies_must_be_active_or_archived() {
        let files = with("change.md", "depends_on: []", "depends_on: [nope]");
        assert!(has(
            &report(files.clone()).errors,
            "depends_on names nope, which is neither"
        ));
        let mut files = files;
        files.push((
            "goodflows/changes/archive/2026-11-01-nope/change.md",
            "x".to_owned(),
        ));
        assert_eq!(report(files).errors, Vec::<String>::new());
    }

    #[test]
    fn cycles_name_their_path() {
        let r = report(with(
            "change.md",
            "depends_on: []",
            "depends_on: [add-login]",
        ));
        assert!(
            has(&r.errors, "depends_on makes a cycle: add-login → add-login"),
            "{:?}",
            r.errors
        );
        let mut files = with("change.md", "depends_on: []", "depends_on: [other]");
        files.push((
            "goodflows/changes/other/change.md",
            PASSKEYS
                .replace("id: add-passkeys", "id: other")
                .replace("depends_on: []", "depends_on: [add-login]"),
        ));
        let r = report(files);
        assert!(
            has(&r.errors, "cycle: add-login → other → add-login"),
            "{:?}",
            r.errors
        );
    }

    #[test]
    fn targets_resolve_with_dependency_deltas_applied() {
        let r = report(with_passkeys("depends_on: []"));
        assert!(
            has(&r.errors, "MODIFIED requirement \"Passkeys\" is not in goodflows/specs/auth/spec.md; active change add-passkeys adds this requirement; add add-passkeys to depends_on"),
            "{:?}", r.errors
        );
        let r = report(with_passkeys("depends_on: [add-passkeys]"));
        assert_eq!(r.errors, Vec::<String>::new());
    }

    #[test]
    fn archive_waits_for_active_dependencies() {
        let files = with_passkeys("depends_on: [add-passkeys]");
        let files: Vec<(&str, &str)> = files.iter().map(|(p, t)| (*p, t.as_str())).collect();
        let top = project(&files);
        let error = archive(top.path(), "add-login", "2026-11-25").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("depends on add-passkeys, which is still active"),
            "{error}"
        );
        assert_eq!(
            std::fs::read_to_string(top.path().join("goodflows/specs/auth/spec.md")).unwrap(),
            SPEC
        );
    }
}
