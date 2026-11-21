//! Git for change requests: the current branch and head, the default claims
//! file path, and the `git-facts/1` document for HEAD.
//!
//! Every git process runs without the API key variable in its environment,
//! since hooks and credential helpers run inside it.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::process::Command;

/// `git-facts/1` limits; the server rejects more.
pub const MAX_COMMITS: usize = 1000;
pub const MAX_FILES: usize = 10_000;

pub struct Git {
    token_env: String,
}

impl Git {
    pub fn new(token_env: &str) -> Self {
        Self {
            token_env: token_env.to_owned(),
        }
    }

    fn command(&self, args: &[&str]) -> Command {
        let mut command = Command::new("git");
        command
            .args(args)
            .env_remove(&self.token_env)
            .env("GIT_OPTIONAL_LOCKS", "0")
            .stdin(Stdio::null())
            .kill_on_drop(true);
        command
    }

    /// The stdout of a git command that must succeed.
    pub async fn output(&self, args: &[&str]) -> Result<String> {
        let output = self
            .command(args)
            .output()
            .await
            .context("could not run git")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git {} failed: {}",
                args.first().copied().unwrap_or_default(),
                crate::output::visible(stderr.trim())
            );
        }
        String::from_utf8(output.stdout).map_err(|_| anyhow!("git printed non-UTF-8 output"))
    }

    /// The stdout of a git command, or `None` when it fails.
    async fn try_output(&self, args: &[&str]) -> Option<String> {
        let output = self
            .command(args)
            .stderr(Stdio::null())
            .output()
            .await
            .ok()?;
        output
            .status
            .success()
            .then(|| String::from_utf8(output.stdout).ok())
            .flatten()
    }

    /// Run git with the terminal's stdout and stderr, as `cr push` does.
    pub async fn passthrough(&self, args: &[String]) -> Result<()> {
        // git's own stdout goes to stderr, so `--json` output stays JSON.
        let status = Command::new("git")
            .args(args)
            .env_remove(&self.token_env)
            .stdout(Stdio::from(std::io::stderr()))
            .kill_on_drop(true)
            .status()
            .await
            .context("could not run git")?;
        if !status.success() {
            bail!("git {} failed", args.first().map_or("", String::as_str));
        }
        Ok(())
    }

    pub async fn head(&self) -> Result<String> {
        let head = self
            .output(&["rev-parse", "--verify", "HEAD^{commit}"])
            .await
            .context("this command needs a git checkout with at least one commit")?;
        Ok(head.trim().to_owned())
    }

    /// A commit named any way git understands, as a full SHA.
    pub async fn resolve(&self, revision: &str) -> Result<String> {
        let spec = format!("{revision}^{{commit}}");
        let sha = self
            .output(&["rev-parse", "--verify", "--end-of-options", &spec])
            .await
            .with_context(|| format!("{revision} does not name a commit"))?;
        Ok(sha.trim().to_owned())
    }

    /// The checked-out branch, or `None` on a detached HEAD.
    pub async fn branch(&self) -> Option<String> {
        self.try_output(&["symbolic-ref", "--short", "-q", "HEAD"])
            .await
            .map(|b| b.trim_end_matches(['\n', '\r']).to_owned())
            .filter(|b| !b.is_empty())
    }

    pub async fn toplevel(&self) -> Result<PathBuf> {
        let top = self
            .output(&["rev-parse", "--show-toplevel"])
            .await
            .context("this command must run inside a git checkout")?;
        Ok(PathBuf::from(top.trim_end_matches(['\n', '\r'])))
    }

    /// The default claims file, relative to the checkout root:
    /// `.goodflows/change-requests/<branch>/cr.md`, each `/` in the branch a
    /// directory, or `.goodflows/change-requests/.detached/<short SHA>/cr.md`.
    pub async fn claims_path(&self) -> Result<ClaimsPath> {
        let mut path = PathBuf::from(".goodflows/change-requests");
        let branch = self.branch().await;
        match &branch {
            Some(branch) => path.extend(branch.split('/')),
            None => {
                let short = self.output(&["rev-parse", "--short", "HEAD"]).await?;
                path.push(".detached");
                path.push(short.trim());
            }
        }
        path.push("cr.md");
        Ok(ClaimsPath {
            relative: path,
            branch,
        })
    }

    /// Update `origin/<target>` with `git fetch` when there is an `origin`
    /// remote, so facts use the merge base the server will see.
    pub async fn fetch_target(&self, target: &str) -> Result<()> {
        if self
            .try_output(&["remote", "get-url", "origin"])
            .await
            .is_none()
        {
            return Ok(());
        }
        let output = self
            .command(&["fetch", "--quiet", "origin", target])
            // Fail instead of waiting at a credential prompt.
            .env("GIT_TERMINAL_PROMPT", "0")
            .output()
            .await
            .context("could not run git")?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            bail!(
                "git fetch origin {} failed: {}; pass --no-fetch to use the refs this checkout has",
                crate::output::visible(target),
                crate::output::visible(stderr.trim())
            );
        }
        Ok(())
    }

    /// The commit the target branch points at: `origin/<target>` when this
    /// checkout has it, else the local branch.
    pub async fn target_commit(&self, target: &str) -> Result<String> {
        for reference in [
            format!("refs/remotes/origin/{target}"),
            format!("refs/heads/{target}"),
        ] {
            let spec = format!("{reference}^{{commit}}");
            if let Some(sha) = self
                .try_output(&["rev-parse", "--verify", "-q", "--end-of-options", &spec])
                .await
            {
                return Ok(sha.trim().to_owned());
            }
        }
        bail!(
            "the target branch {} is not in this checkout as origin/{0} or {0}; fetch it first",
            crate::output::visible(target)
        )
    }

    /// The `git-facts/1` document for HEAD against `target`.
    pub async fn facts(&self, target: &str) -> Result<Value> {
        let head = self.head().await?;
        let target_commit = self.target_commit(target).await?;
        let merge_base = self
            .output(&["merge-base", &head, &target_commit])
            .await
            .with_context(|| {
                format!(
                    "HEAD has no merge base with {}",
                    crate::output::visible(target)
                )
            })?
            .trim()
            .to_owned();
        let range = format!("{merge_base}..{head}");
        let listed = self
            .output(&["rev-list", "--reverse", "--parents", &range, "--"])
            .await?;
        let lines: Vec<&str> = listed.lines().filter(|l| !l.is_empty()).collect();
        if lines.len() > MAX_COMMITS {
            bail!(
                "HEAD is {} commits ahead of its merge base with {}; git-facts/1 allows at most {MAX_COMMITS}",
                lines.len(),
                crate::output::visible(target)
            );
        }
        let mut commits = Vec::with_capacity(lines.len());
        for line in lines {
            let mut shas = line.split(' ');
            let sha = shas.next().unwrap_or_default().to_owned();
            let parents: Vec<String> = shas.map(str::to_owned).collect();
            let Some(first) = parents.first() else {
                bail!("commit {sha} after the merge base has no parent");
            };
            let changed_files = self.changed_files(first, &sha).await?;
            commits.push(json!({"sha": sha, "parents": parents, "changed_files": changed_files}));
        }
        let files = self.changed_files(&merge_base, &head).await?;
        Ok(json!({
            "format": "git-facts/1",
            "head": head,
            "target": {"branch": target, "merge_base": merge_base},
            "commits": commits,
            "diff": {"files": files},
        }))
    }

    /// The files changed between two commits, with rename detection.
    async fn changed_files(&self, from: &str, to: &str) -> Result<Vec<Value>> {
        let diff = |format: &'static str| {
            let (from, to) = (from.to_owned(), to.to_owned());
            async move {
                self.output(&[
                    "diff",
                    "--no-ext-diff",
                    "--no-textconv",
                    "-M",
                    "-z",
                    format,
                    &from,
                    &to,
                    "--",
                ])
                .await
            }
        };
        let (statuses, counts) = tokio::try_join!(diff("--name-status"), diff("--numstat"))?;
        let files = file_entries(&statuses, &counts)?;
        if files.len() > MAX_FILES {
            bail!(
                "{} files changed between {} and {}; git-facts/1 allows at most {MAX_FILES}",
                files.len(),
                &from[..from.len().min(12)],
                &to[..to.len().min(12)]
            );
        }
        Ok(files)
    }
}

pub struct ClaimsPath {
    pub relative: PathBuf,
    pub branch: Option<String>,
}

impl ClaimsPath {
    pub fn absolute(&self, toplevel: &Path) -> PathBuf {
        toplevel.join(&self.relative)
    }
}

/// Joins `git diff -z --name-status` and `--numstat` output, which list the
/// same files in the same order, into `git-facts/1` file entries.
fn file_entries(statuses: &str, counts: &str) -> Result<Vec<Value>> {
    const UNREADABLE: &str = "could not read git diff output";
    let mut tokens = statuses.split('\0').filter(|t| !t.is_empty());
    let mut changed = Vec::new();
    while let Some(code) = tokens.next() {
        let status = match code.as_bytes().first() {
            Some(b'A') => "added",
            Some(b'M') => "modified",
            Some(b'D') => "deleted",
            Some(b'R') => "renamed",
            Some(b'C') => "copied",
            Some(b'T') => "type_changed",
            _ => bail!("git diff reported an unsupported file status {code:?}"),
        };
        let old_path = if matches!(status, "renamed" | "copied") {
            Some(tokens.next().context(UNREADABLE)?)
        } else {
            None
        };
        let path = tokens.next().context(UNREADABLE)?;
        changed.push((status, old_path, path));
    }
    let mut tokens = counts.split('\0');
    let mut files = Vec::with_capacity(changed.len());
    for (status, old_path, path) in changed {
        let head = tokens.next().context(UNREADABLE)?;
        let mut fields = head.splitn(3, '\t');
        let count = |field: Option<&str>| -> Result<u64> {
            match field.context(UNREADABLE)? {
                // A binary file.
                "-" => Ok(0),
                n => n.parse().context(UNREADABLE),
            }
        };
        let additions = count(fields.next())?;
        let deletions = count(fields.next())?;
        let named = fields.next().context(UNREADABLE)?;
        // A rename or copy leaves the path empty and lists both paths next.
        let counted = if named.is_empty() {
            let _old = tokens.next().context(UNREADABLE)?;
            tokens.next().context(UNREADABLE)?
        } else {
            named
        };
        if counted != path {
            bail!(UNREADABLE);
        }
        let mut entry = json!({
            "path": path,
            "status": status,
            "additions": additions,
            "deletions": deletions,
        });
        if let Some(old_path) = old_path {
            entry["old_path"] = json!(old_path);
        }
        files.push(entry);
    }
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_entries_join_statuses_and_counts() {
        let statuses = "M\0src/a.rs\0R087\0old/b.rs\0new/b.rs\0A\0img.png\0D\0gone.txt\0T\0link\0";
        let counts = "3\t1\tsrc/a.rs\0".to_owned()
            + "2\t2\t\0old/b.rs\0new/b.rs\0-\t-\timg.png\0"
            + "0\t4\tgone.txt\0"
            + "1\t1\tlink\0";
        let files = file_entries(statuses, &counts).unwrap();
        assert_eq!(
            Value::Array(files),
            json!([
                {"path": "src/a.rs", "status": "modified", "additions": 3, "deletions": 1},
                {"path": "new/b.rs", "status": "renamed", "additions": 2, "deletions": 2, "old_path": "old/b.rs"},
                {"path": "img.png", "status": "added", "additions": 0, "deletions": 0},
                {"path": "gone.txt", "status": "deleted", "additions": 0, "deletions": 4},
                {"path": "link", "status": "type_changed", "additions": 1, "deletions": 1},
            ])
        );
        assert!(file_entries("M\0a\0", "1\t1\tb\0").is_err());
        assert!(file_entries("", "").unwrap().is_empty());
    }
}
