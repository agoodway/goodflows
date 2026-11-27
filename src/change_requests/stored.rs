//! Stored changes: `goodflows change push`, `list`, `show`, `pull`,
//! `versions` and `delete`, which mirror a `goodflows/changes/<id>/`
//! directory on the GoodFlows server and bring a stored version back.
//!
//! The repository owns a change; the server keeps every pushed version.
//! Push sends the directory's files as they are on disk with the commit,
//! branch and dirty flag they came from. Pull writes a whole version or
//! nothing, and never writes outside the change directory or through a
//! symlink.

use super::api::{data, explain_conflict, quoted, Api, Body, HttpError};
use super::changes::{
    archived_id, capability_name, check, front_matter, ChangeCommand, StoredStatus, CHANGES,
};
use super::git::Git;
use crate::config::Reporting;
use crate::output::{visible, Output};
use anyhow::{anyhow, bail, Context, Result};
use reqwest::Method;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::Path;
use tokio::sync::watch;

const ARCHIVE: &str = "goodflows/changes/archive";

pub async fn command(
    reporting: Option<&Reporting>,
    command: &ChangeCommand,
    output: &Output,
    cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let name = match command {
        ChangeCommand::Push { .. } => "change push",
        ChangeCommand::List { .. } => "change list",
        ChangeCommand::Show { .. } => "change show",
        ChangeCommand::Pull { .. } => "change pull",
        ChangeCommand::Versions { .. } => "change versions",
        ChangeCommand::Delete { .. } => "change delete",
        _ => unreachable!("offline change commands run in changes::change"),
    };
    let api = Api::new(reporting, name, cancel)?;
    let git = Git::new(api.token_env());
    let event = match command {
        ChangeCommand::Push { id } => push(&api, &git, id).await?,
        ChangeCommand::List { status } => list(&api, *status).await?,
        ChangeCommand::Show { id, version, file } => {
            show(&api, id, *version, file.as_deref()).await?
        }
        ChangeCommand::Pull { id, version, force } => {
            pull(&api, &git, id, *version, *force).await?
        }
        ChangeCommand::Versions { id } => versions(&api, id).await?,
        ChangeCommand::Delete { id, .. } => delete(&api, id).await?,
        _ => unreachable!(),
    };
    output.emit(event).await?;
    Ok(0)
}

/// Whether `path`, relative to a change directory, is a file a change may
/// hold: `change.md`, `qa-scenarios.md`, `verification.md` or
/// `specs/<capability>/spec.md` under `change validate`'s capability rule.
/// The same rule for push and pull.
fn change_file(path: &str) -> bool {
    match path {
        "change.md" | "qa-scenarios.md" | "verification.md" => true,
        _ => path
            .strip_prefix("specs/")
            .and_then(|rest| rest.strip_suffix("/spec.md"))
            .is_some_and(capability_name),
    }
}

const CHANGE_FILES: &str =
    "a change holds only change.md, specs/<capability>/spec.md, qa-scenarios.md and verification.md";

fn listed<'a>(items: impl IntoIterator<Item = &'a String>) -> String {
    items
        .into_iter()
        .map(|item| visible(item).into_owned())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Fails when `relative`, under the checkout's top level, is a symlink.
fn no_symlink(top: &Path, relative: &str) -> Result<()> {
    let is_link = top
        .join(relative)
        .symlink_metadata()
        .is_ok_and(|m| m.file_type().is_symlink());
    if is_link {
        bail!("{relative} is a symlink; stored change commands do not follow symlinks, so nothing was sent or written. Replace it with the real directory or file and run the command again");
    }
    Ok(())
}

/// The archive directories of `id`, `goodflows/changes/archive/<date>-<id>`.
fn archived_dirs(top: &Path, id: &str) -> Result<Vec<String>> {
    for relative in ["goodflows", CHANGES, ARCHIVE] {
        no_symlink(top, relative)?;
    }
    let mut found = Vec::new();
    for entry in std::fs::read_dir(top.join(ARCHIVE)).into_iter().flatten() {
        let entry = entry.context("could not read goodflows/changes/archive")?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if archived_id(&name).as_deref() != Some(id) {
            continue;
        }
        let relative = format!("{ARCHIVE}/{name}");
        no_symlink(top, &relative)?;
        if entry.path().is_dir() {
            found.push(relative);
        }
    }
    found.sort();
    Ok(found)
}

/// The directory push reads for `id`, relative to the top level, and
/// whether it is archived.
fn locate(top: &Path, id: &str) -> Result<(String, bool)> {
    let archived = archived_dirs(top, id)?;
    let active = format!("{CHANGES}/{id}");
    no_symlink(top, &active)?;
    if top.join(&active).symlink_metadata().is_ok() {
        if !archived.is_empty() {
            bail!(
                "{active} is a new change, but {} already archived change {id}; each stored id is one change, so pick a new id for the new change",
                listed(&archived)
            );
        }
        if !top.join(&active).is_dir() {
            bail!("{active} is not a directory, so nothing was sent; move the file away and put change {id} in a directory there");
        }
        return Ok((active, false));
    }
    match archived.as_slice() {
        [] => bail!("there is no change {id}: neither {active}/ nor {ARCHIVE}/<date>-{id}/ exists; check the id, or run goodflows change pull {id} to fetch a stored change"),
        [one] => Ok((one.clone(), true)),
        several => bail!(
            "several archived directories are change {id}: {}; keep only one to push it",
            listed(several)
        ),
    }
}

/// Every file under `dir`, by path relative to it, with its bytes. Fails
/// naming every symlink.
fn read_tree(dir: &Path, shown: &str) -> Result<BTreeMap<String, Vec<u8>>> {
    fn walk(
        dir: &Path,
        prefix: &str,
        files: &mut BTreeMap<String, Vec<u8>>,
        links: &mut Vec<String>,
    ) -> Result<()> {
        let entries =
            std::fs::read_dir(dir).with_context(|| format!("could not read {}", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("could not read {}", dir.display()))?;
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            let kind = entry
                .file_type()
                .with_context(|| format!("could not read {}", entry.path().display()))?;
            if kind.is_symlink() {
                links.push(name);
            } else if kind.is_dir() {
                walk(&entry.path(), &format!("{name}/"), files, links)?;
            } else {
                let bytes = std::fs::read(entry.path())
                    .with_context(|| format!("could not read {}", entry.path().display()))?;
                files.insert(name, bytes);
            }
        }
        Ok(())
    }
    let (mut files, mut links) = (BTreeMap::new(), Vec::new());
    walk(dir, "", &mut files, &mut links)?;
    if !links.is_empty() {
        links.sort();
        bail!(
            "{shown} holds symlinks: {}; stored change commands do not follow symlinks, so nothing was sent or written. Replace each with the file itself and run the command again",
            listed(&links)
        );
    }
    Ok(files)
}

/// The change files of `dir` as text, refusing any other file.
fn read_change(
    top: &Path,
    dir: &str,
    id: &str,
    archived: bool,
) -> Result<BTreeMap<String, String>> {
    let tree = read_tree(&top.join(dir), dir)?;
    let others: Vec<&String> = tree.keys().filter(|path| !change_file(path)).collect();
    if !others.is_empty() {
        bail!(
            "{dir} holds files a change cannot have: {}; {CHANGE_FILES}, so move them out and push again",
            listed(others)
        );
    }
    if !archived {
        let (report, _) = check(top, id);
        if !report.errors.is_empty() {
            bail!(
                "change {id} has errors, so nothing was sent; fix them and push again (goodflows change validate {id} lists them):\n  {}",
                report.errors.join("\n  ")
            );
        }
    }
    let mut files = BTreeMap::new();
    for (path, bytes) in tree {
        let text = String::from_utf8(bytes).map_err(|_| {
            anyhow!(
                "{dir}/{} is not UTF-8 text, so it cannot be stored; nothing was sent. Save it as UTF-8 and push again",
                visible(&path)
            )
        })?;
        files.insert(path, text);
    }
    if archived {
        let shown = format!("{dir}/change.md");
        let text = files
            .get("change.md")
            .ok_or_else(|| {
                anyhow!("{dir} has no change.md; every change needs one, so nothing was sent. Restore it from git and push again")
            })?;
        let fields = front_matter(text, &shown, &mut Vec::new()).ok_or_else(|| {
            anyhow!("{shown} has no readable front matter; fix it and push again")
        })?;
        if fields["id"] != id {
            bail!(
                "{shown} is change {}, not {id}; fix its id and push again",
                quoted(&fields["id"])
            );
        }
    }
    Ok(files)
}

/// HEAD's commit (null before the first commit), the current branch (null
/// when detached), and whether `files` differ from what HEAD tracks under
/// `dir` in any way.
async fn source(
    git: &Git,
    top: &Path,
    dir: &str,
    files: &BTreeMap<String, String>,
) -> Result<Value> {
    let commit = git
        .try_output(&["rev-parse", "--verify", "-q", "HEAD^{commit}"])
        .await
        .map(|sha| sha.trim().to_owned());
    let branch = git.branch().await;
    if branch.as_deref().is_some_and(|b| !super::valid_text(b)) {
        bail!("the branch name must be 1-256 bytes without control characters; push from another branch or a detached HEAD");
    }
    let dirty = match &commit {
        None => true,
        Some(commit) => differs_from(git, top, commit, dir, files).await?,
    };
    Ok(json!({"commit": commit, "branch": branch, "dirty": dirty}))
}

/// Whether the files of `dir` are anything but exactly the regular files
/// `commit` tracks under it, with the same content.
async fn differs_from(
    git: &Git,
    top: &Path,
    commit: &str,
    dir: &str,
    files: &BTreeMap<String, String>,
) -> Result<bool> {
    let top = top.to_string_lossy();
    let tree = git
        .output(&["-C", &top, "ls-tree", "-r", "-z", commit, "--", dir])
        .await?;
    let mut tracked = BTreeMap::new();
    for line in tree.split('\0').filter(|l| !l.is_empty()) {
        let Some((meta, path)) = line.split_once('\t') else {
            return Ok(true);
        };
        let mut parts = meta.split(' ');
        let (mode, sha) = (parts.next(), parts.nth(1));
        let relative = path.strip_prefix(dir).and_then(|p| p.strip_prefix('/'));
        match (mode, sha, relative) {
            (Some("100644" | "100755"), Some(sha), Some(relative)) => {
                tracked.insert(relative.to_owned(), sha.to_owned());
            }
            _ => return Ok(true),
        }
    }
    if !tracked.keys().eq(files.keys()) {
        return Ok(true);
    }
    if files.is_empty() {
        return Ok(false);
    }
    let paths: Vec<String> = files.keys().map(|p| format!("{dir}/{p}")).collect();
    let mut args = vec!["-C", &top, "hash-object", "--no-filters", "--"];
    args.extend(paths.iter().map(String::as_str));
    let hashes = git.output(&args).await?;
    Ok(!hashes.lines().eq(tracked.values().map(String::as_str)))
}

/// A 404 from the `/changes` collection, explained when the project's
/// change requests answer: then the server has no stored changes routes.
async fn without_routes(api: &Api, error: anyhow::Error) -> anyhow::Error {
    if HttpError::of(&error, 404).is_none() {
        return error;
    }
    match api.get(&["change-requests"], &[("limit", "1".to_owned())]).await {
        Ok(_) => anyhow!(
            "{} failed: the GoodFlows server does not store changes yet; update the server, or keep the change in the repository for now",
            api.command()
        ),
        Err(_) => error,
    }
}

async fn push(api: &Api, git: &Git, id: &str) -> Result<Value> {
    let top = git.toplevel().await?;
    let (dir, archived) = locate(&top, id)?;
    let files = read_change(&top, &dir, id, archived)?;
    let source = source(git, &top, &dir, &files).await?;
    let body = json!({"files": files, "source": source});
    let sent = api
        .send(Method::POST, &["changes"], &[], Body::Json(body))
        .await
        .map_err(|error| {
            explain_conflict(error, |errors| {
                errors["detail"]
                    .as_str()
                    .is_some_and(|d| d.contains("is archived"))
                    .then(|| {
                        format!(
                            "change push failed: stored change {id} is archived; pick a new id for new work"
                        )
                    })
            })
        });
    let (status, response) = match sent {
        Ok(sent) => sent,
        Err(error) => return Err(without_routes(api, error).await),
    };
    let mut stored = data(response, api.command())?;
    let version = stored["version"].as_u64().ok_or_else(|| {
        anyhow!("change push failed: the server's response has no version number; run goodflows change versions {id} to see what the server stored")
    })?;
    let replaced = stored["replaced_version"].take();
    if let Some(fields) = stored.as_object_mut() {
        fields.remove("duplicate");
        fields.remove("replaced_version");
    }
    Ok(json!({
        "type": "change_push",
        "id": id,
        "dir": dir,
        "files": files.keys().collect::<Vec<_>>(),
        "version": version,
        "status": if status == 201 { "created" } else { "unchanged" },
        "replaced_version": replaced,
        "change": stored,
        "source": source,
    }))
}

/// Every row of a paged list, following `next_cursor` until it is null.
async fn every_page(
    api: &Api,
    segments: &[&str],
    mut query: Vec<(&str, String)>,
) -> Result<Vec<Value>> {
    query.push(("limit", "200".to_owned()));
    let mut rows = Vec::new();
    loop {
        let page = api.get(segments, &query).await?;
        let items = page["data"].as_array().ok_or_else(|| {
            anyhow!(
                "{} failed: the server's response is not a list; check that GOODFLOWS_URL points at a GoodFlows server and try again",
                api.command()
            )
        })?;
        rows.extend(items.iter().cloned());
        match page["next_cursor"].as_str() {
            Some(next) => {
                query.retain(|(key, _)| *key != "cursor");
                query.push(("cursor", next.to_owned()));
            }
            None => return Ok(rows),
        }
    }
}

async fn list(api: &Api, status: Option<StoredStatus>) -> Result<Value> {
    let status = status.map(super::name);
    let query = status.iter().map(|s| ("status", s.clone())).collect();
    let changes = match every_page(api, &["changes"], query).await {
        Ok(changes) => changes,
        Err(error) => return Err(without_routes(api, error).await),
    };
    Ok(json!({"type": "change_list", "status": status, "changes": changes}))
}

/// The latest version of `id`, or version `n`, with its files.
async fn version(api: &Api, id: &str, n: Option<u64>) -> Result<Value> {
    let body = match n {
        None => api.get(&["changes", id], &[]).await?,
        Some(n) => {
            api.get(&["changes", id, "versions", &n.to_string()], &[])
                .await?
        }
    };
    data(body, api.command())
}

async fn show(api: &Api, id: &str, n: Option<u64>, file: Option<&str>) -> Result<Value> {
    let mut stored = version(api, id, n).await?;
    let Some(path) = file else {
        if n.is_some() {
            // A version read carries no links; the change read does.
            let latest = version(api, id, None).await?;
            stored["change_requests"] = latest["change_requests"].clone();
        }
        stored["type"] = json!("change_show");
        stored["id"] = json!(id);
        return Ok(stored);
    };
    let entries = stored["files"].as_array().cloned().unwrap_or_default();
    let found = entries.iter().find(|f| f["path"] == path);
    match found.and_then(|f| f["text"].as_str()) {
        Some(text) => Ok(json!({
            "type": "change_show",
            "id": id,
            "version": stored["version"],
            "file": path,
            "text": text,
        })),
        None => {
            let paths: Vec<String> = entries
                .iter()
                .filter_map(|f| f["path"].as_str().map(str::to_owned))
                .collect();
            bail!(
                "version {} of stored change {id} has no file {}; its files are {}",
                quoted(&stored["version"]),
                visible(path),
                listed(&paths)
            )
        }
    }
}

async fn versions(api: &Api, id: &str) -> Result<Value> {
    let mut versions = every_page(api, &["changes", id, "versions"], Vec::new()).await?;
    versions.sort_by_key(|v| std::cmp::Reverse(v["version"].as_u64()));
    Ok(json!({"type": "change_versions", "id": id, "versions": versions}))
}

async fn delete(api: &Api, id: &str) -> Result<Value> {
    api.send(Method::DELETE, &["changes", id], &[], Body::None)
        .await
        .map_err(|error| {
            explain_conflict(error, |errors| {
                let crs = errors["change_requests"].as_array()?;
                let numbers: Vec<String> = crs
                    .iter()
                    .map(|cr| format!("#{} ({})", quoted(&cr["number"]), quoted(&cr["state"])))
                    .collect();
                Some(format!(
                    "change delete failed: change requests {} link stored change {id}, so it was not deleted; a linked change cannot be deleted, so keep it, or delete those change requests on the server first",
                    numbers.join(", ")
                ))
            })
        })?;
    Ok(json!({"type": "change_delete", "id": id, "deleted": true}))
}

/// A version's `[{path, text}]` files, refusing the whole version when
/// any path is not a change file or appears twice, or any file has no text.
fn version_files(files: &Value, id: &str, number: &Value) -> Result<BTreeMap<String, String>> {
    let files = files.as_array().ok_or_else(|| {
        anyhow!("change pull failed: the server's version of stored change {id} has no files, so nothing was written; check it with goodflows change show {id}")
    })?;
    let mut bad = Vec::new();
    let mut empty = Vec::new();
    let mut twice = Vec::new();
    let mut out = BTreeMap::new();
    for file in files {
        let Some(path) = file["path"].as_str().map(str::to_owned) else {
            bad.push(quoted(&file["path"]));
            continue;
        };
        if !change_file(&path) {
            bad.push(path);
        } else if out.contains_key(&path) || twice.contains(&path) {
            twice.push(path);
        } else if let Some(text) = file["text"].as_str() {
            out.insert(path, text.to_owned());
        } else {
            empty.push(path);
        }
    }
    if !twice.is_empty() {
        bail!(
            "version {} of stored change {id} lists files more than once: {}; nothing was written. Push the change again from a checkout that has it, or pull another version with --version",
            quoted(number),
            listed(&twice)
        );
    }
    if !bad.is_empty() {
        bail!(
            "version {} of stored change {id} has paths that are not change files: {}; {CHANGE_FILES}, so nothing was written. Push the change again from a checkout that has it, or pull another version with --version",
            quoted(number),
            listed(&bad)
        );
    }
    if !empty.is_empty() {
        bail!(
            "version {} of stored change {id} has files without text: {}; nothing was written. Pull with a private key (sk_), which can read file contents",
            quoted(number),
            listed(&empty)
        );
    }
    Ok(out)
}

async fn pull(api: &Api, git: &Git, id: &str, n: Option<u64>, force: bool) -> Result<Value> {
    let top = git.toplevel().await?;
    let archived = archived_dirs(&top, id)?;
    if !archived.is_empty() {
        bail!(
            "{} is the archived change {id}; pulling it would make it active again, so nothing was written. Read it there, or pick a new id for new work",
            listed(&archived)
        );
    }
    let dir = format!("{CHANGES}/{id}");
    no_symlink(&top, &dir)?;
    let target = top.join(&dir);
    let local = match target.symlink_metadata() {
        Err(_) => BTreeMap::new(),
        Ok(meta) if meta.is_dir() => read_tree(&target, &dir)?,
        Ok(_) => bail!(
            "{dir} is not a directory; nothing was written. Move the file away and pull again"
        ),
    };
    let stored = version(api, id, n).await?;
    let latest_status = if n.is_some() {
        &stored["latest_status"]
    } else {
        &stored["status"]
    };
    match latest_status.as_str() {
        Some("archived") => bail!(
            "stored change {id} is archived; pull refuses every version of an archived change, so nothing was written. Read it with goodflows change show {id}, or pick a new id for new work"
        ),
        Some(_) => {}
        None => bail!(
            "change pull failed: the server's response does not say whether stored change {id} is archived, so nothing was written; update the GoodFlows server, or read the change with goodflows change show {id}"
        ),
    }
    let number = &stored["version"];
    let files = version_files(&stored["files"], id, number)?;
    let differ: Vec<&String> = local
        .iter()
        .filter(|(path, bytes)| files.get(*path).map(String::as_bytes) != Some(bytes.as_slice()))
        .map(|(path, _)| path)
        .collect();
    if !differ.is_empty() && !force {
        bail!(
            "{dir} has files that differ from version {} or that it does not have: {}; nothing was written. Commit or move them, or pass --force to replace them",
            quoted(number),
            listed(differ)
        );
    }
    let deleted: Vec<&String> = local.keys().filter(|p| !files.contains_key(*p)).collect();
    let same = differ.is_empty() && local.len() == files.len();
    if !same {
        std::fs::create_dir_all(top.join(CHANGES))
            .with_context(|| format!("could not create {CHANGES}"))?;
        let staging = top.join(format!("goodflows/.pull-{id}-{}", std::process::id()));
        let cancel = api.cancel();
        let interrupted = || cancel.borrow().is_some();
        install(&target, &staging, &files, &interrupted, &|path, bytes| {
            std::fs::write(path, bytes)
        })?;
    }
    Ok(json!({
        "type": "change_pull",
        "id": id,
        "version": number,
        "dir": dir,
        "files": files.keys().collect::<Vec<_>>(),
        "deleted": deleted,
        "written": !same,
    }))
}

/// Replaces `target` with a directory of exactly `files`, or leaves it as
/// it was. The new directory is written in `staging`, outside
/// `goodflows/changes/`, and swapped in with two renames; a failure or an
/// interruption at any step puts the old directory back.
fn install(
    target: &Path,
    staging: &Path,
    files: &BTreeMap<String, String>,
    interrupted: &dyn Fn() -> bool,
    write: &dyn Fn(&Path, &[u8]) -> std::io::Result<()>,
) -> Result<()> {
    let shown = staging.display();
    let new = staging.join("new");
    let old = staging.join("old");
    let had = target.symlink_metadata().is_ok();
    let stop = || anyhow!("change pull interrupted; nothing was written");
    let written = (|| -> Result<()> {
        if staging.symlink_metadata().is_ok() {
            std::fs::remove_dir_all(staging)?;
        }
        std::fs::create_dir(staging)?;
        std::fs::create_dir(&new)?;
        for (path, text) in files {
            if interrupted() {
                return Err(stop());
            }
            let file = new.join(path);
            if let Some(parent) = file.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write(&file, text.as_bytes())?;
        }
        if interrupted() {
            return Err(stop());
        }
        if had {
            std::fs::rename(target, &old)?;
        }
        Ok(())
    })();
    if let Err(error) = written {
        let _ = std::fs::remove_dir_all(staging);
        return Err(error.context("change pull failed; nothing was written"));
    }
    let restore = |error: anyhow::Error| -> anyhow::Error {
        if had {
            if let Err(undo) = std::fs::rename(&old, target) {
                return anyhow!(
                    "change pull failed ({error:#}) and could not put the local files back ({undo}); they are in {}",
                    old.display()
                );
            }
        }
        let _ = std::fs::remove_dir_all(staging);
        error.context("change pull failed; nothing was written")
    };
    if let Err(error) = std::fs::rename(&new, target) {
        return Err(restore(anyhow!(
            "could not move the new files into place: {error}"
        )));
    }
    if interrupted() {
        if let Err(error) = std::fs::rename(target, staging.join("undone")) {
            return Err(anyhow!(
                "change pull was interrupted after writing and could not undo it ({error}); the local files are in {shown}/old"
            ));
        }
        return Err(restore(stop()));
    }
    let _ = std::fs::remove_dir_all(staging);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn tree(dir: &Path) -> BTreeMap<String, Vec<u8>> {
        read_tree(dir, "dir").unwrap()
    }

    fn setup() -> (tempfile::TempDir, BTreeMap<String, String>) {
        let top = tempfile::tempdir().unwrap();
        let target = top.path().join("goodflows/changes/add-login");
        std::fs::create_dir_all(target.join("specs/auth")).unwrap();
        std::fs::write(target.join("change.md"), "old change\n").unwrap();
        std::fs::write(target.join("specs/auth/spec.md"), "old delta\n").unwrap();
        std::fs::write(target.join("verification.md"), "local only\n").unwrap();
        let files = BTreeMap::from([
            ("change.md".to_owned(), "new change\n".to_owned()),
            ("qa-scenarios.md".to_owned(), "new qa\n".to_owned()),
            ("specs/auth/spec.md".to_owned(), "new delta\n".to_owned()),
            ("specs/roles/spec.md".to_owned(), "new roles\n".to_owned()),
        ]);
        (top, files)
    }

    #[test]
    fn a_write_failure_after_the_first_file_leaves_the_directory_as_it_was() {
        let (top, files) = setup();
        let target = top.path().join("goodflows/changes/add-login");
        let staging = top.path().join("goodflows/.pull-add-login-1");
        let before = tree(&target);
        for fail_at in 1..files.len() {
            let calls = Cell::new(0);
            let error = install(&target, &staging, &files, &|| false, &|path, bytes| {
                calls.set(calls.get() + 1);
                if calls.get() > fail_at {
                    return Err(std::io::Error::other("disk full"));
                }
                std::fs::write(path, bytes)
            })
            .unwrap_err();
            assert!(format!("{error:#}").contains("disk full"), "{error:#}");
            assert_eq!(tree(&target), before, "failing write {}", fail_at + 1);
            assert!(!staging.exists());
        }
    }

    #[test]
    fn an_interruption_at_any_step_leaves_the_directory_as_it_was() {
        let (top, files) = setup();
        let target = top.path().join("goodflows/changes/add-login");
        let staging = top.path().join("goodflows/.pull-add-login-1");
        let before = tree(&target);
        // One check before each file, one before the swap, one after it.
        for at in 0..files.len() + 2 {
            let checks = Cell::new(0);
            let interrupted = || {
                checks.set(checks.get() + 1);
                checks.get() > at
            };
            let write = |path: &Path, bytes: &[u8]| std::fs::write(path, bytes);
            let error = install(&target, &staging, &files, &interrupted, &write).unwrap_err();
            assert!(format!("{error:#}").contains("interrupted"), "{error:#}");
            assert_eq!(tree(&target), before, "interrupted at check {at}");
            assert!(!staging.exists());
        }
    }

    #[test]
    fn install_writes_exactly_the_version() {
        let (top, files) = setup();
        let target = top.path().join("goodflows/changes/add-login");
        let staging = top.path().join("goodflows/.pull-add-login-1");
        install(&target, &staging, &files, &|| false, &|path, bytes| {
            std::fs::write(path, bytes)
        })
        .unwrap();
        let expected: BTreeMap<String, Vec<u8>> = files
            .iter()
            .map(|(p, t)| (p.clone(), t.clone().into_bytes()))
            .collect();
        assert_eq!(tree(&target), expected);
        assert!(!staging.exists());
        let fresh = top.path().join("goodflows/changes/add-new");
        install(&fresh, &staging, &files, &|| false, &|p, b| {
            std::fs::write(p, b)
        })
        .unwrap();
        assert_eq!(tree(&fresh), expected);
    }

    #[test]
    fn push_and_pull_accept_the_same_change_files() {
        for path in [
            "change.md",
            "qa-scenarios.md",
            "verification.md",
            "specs/auth/spec.md",
            "specs/area/name/spec.md",
        ] {
            assert!(change_file(path), "{path}");
        }
        for path in [
            "notes.txt",
            "/change.md",
            "specs/spec.md",
            "specs//spec.md",
            "specs/./spec.md",
            "specs/../spec.md",
            "specs/auth/../../x/spec.md",
            "specs/Auth/spec.md",
            "specs/auth/other.md",
            "./change.md",
            "specs/auth/spec.md/",
        ] {
            assert!(!change_file(path), "{path}");
        }
    }
}
