use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

const GIT_TIMEOUT: Duration = Duration::from_secs(2);

/// Source context: commit, branch and dirty only, each null when unavailable.
/// Remote URLs and other repository metadata are never captured.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub commit: Option<String>,
    pub branch: Option<String>,
    pub dirty: Option<bool>,
}

async fn git(workdir: &Path, token_env: Option<&str>, args: &[&str]) -> Option<String> {
    let mut command = Command::new("git");
    command.args(args).current_dir(workdir);
    if let Some(token_env) = token_env {
        command.env_remove(token_env);
    }
    let output = command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

/// `token_env`, the publisher's credential variable when reporting is
/// configured, is removed from every git process.
pub async fn capture(workdir: &Path, token_env: Option<&str>) -> Source {
    let gather = async {
        let (commit, branch, status) = tokio::join!(
            git(workdir, token_env, &["rev-parse", "HEAD"]),
            git(
                workdir,
                token_env,
                &["symbolic-ref", "--short", "-q", "HEAD"]
            ),
            git(
                workdir,
                token_env,
                &["status", "--porcelain", "--untracked-files=no"]
            ),
        );
        Source {
            commit: commit.and_then(|c| valid_commit(c.trim())),
            branch: branch.and_then(|b| valid_branch(b.trim_end_matches(['\n', '\r']))),
            dirty: status.map(|s| !s.trim().is_empty()),
        }
    };
    tokio::time::timeout(GIT_TIMEOUT, gather)
        .await
        .unwrap_or_default()
}

/// Where an invocation's source context comes from.
#[derive(Clone, Copy)]
pub enum Origin<'a> {
    /// A local checkout, captured with git when it is needed.
    Checkout(&'a Path),
    /// A remote Sprite invocation (design D11): the controller's snapshot
    /// provenance, `None` when HEAD could not be read. The source tree there
    /// has no `.git`, so nothing is captured natively.
    Snapshot(Option<&'a Source>),
}

impl Origin<'_> {
    pub fn remote(&self) -> bool {
        matches!(self, Self::Snapshot(_))
    }

    /// The source context, or `None` when a snapshot has none.
    pub async fn source(&self, token_env: Option<&str>) -> Option<Source> {
        match self {
            Self::Checkout(workdir) => Some(capture(workdir, token_env).await),
            Self::Snapshot(source) => source.cloned(),
        }
    }
}

pub(crate) fn valid_commit(commit: &str) -> Option<String> {
    (matches!(commit.len(), 40 | 64) && commit.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| commit.to_owned())
}

pub(crate) fn valid_branch(branch: &str) -> Option<String> {
    (!branch.is_empty() && branch.len() <= 256 && !branch.chars().any(char::is_control))
        .then(|| branch.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commit_and_branch_rules() {
        assert!(valid_commit(&"a".repeat(40)).is_some());
        assert!(valid_commit(&"0".repeat(64)).is_some());
        assert!(valid_commit(&"a".repeat(41)).is_none());
        assert!(valid_commit(&"g".repeat(40)).is_none());
        assert!(valid_branch("main").is_some());
        assert!(valid_branch("").is_none());
        assert!(valid_branch(&"b".repeat(256)).is_some());
        assert!(valid_branch(&"b".repeat(257)).is_none());
        assert!(valid_branch("a\u{0}b").is_none());
    }

    #[tokio::test]
    async fn non_git_directory_yields_null_fields() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(capture(dir.path(), Some("X")).await, Source::default());
    }
}
