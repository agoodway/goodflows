//! The installed Sprite CLI as controller transport.
//!
//! Every call names the organization and Sprite explicitly (`-o`/`-s` or a
//! positional name), uses non-TTY exec with an explicit stdin mode and
//! `--no-port-forward`, and never runs `sprite use` or changes the active
//! context. Credentials stay in the CLI's own environment or keyring and are
//! never passed in argv.

use anyhow::{bail, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::{AsyncRead, AsyncWriteExt};
use tokio::process::Command;

/// Find an executable on `PATH`.
pub fn find_on_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| {
            std::fs::metadata(candidate).is_ok_and(|m| {
                m.is_file()
                    && std::os::unix::fs::PermissionsExt::mode(&m.permissions()) & 0o111 != 0
            })
        })
}

/// Remove known secret values from transport diagnostics and bound their size.
pub fn sanitize(text: &[u8], secrets: &[String]) -> String {
    let mut text = String::from_utf8_lossy(text).into_owned();
    for secret in secrets.iter().filter(|s| s.len() >= 4) {
        text = text.replace(secret.as_str(), "[redacted]");
    }
    let text = text.trim();
    let mut bounded: String = text.chars().take(2000).collect();
    if bounded.len() < text.len() {
        bounded.push_str(" …");
    }
    bounded
        .chars()
        .filter(|c| *c == '\n' || !c.is_control())
        .collect()
}

/// Diagnostic bytes kept from a streaming call's stderr; the rest is read
/// and discarded.
pub const STREAM_STDERR_KEEP: usize = 64 * 1024;

/// A streaming agent call: the child (stdout piped for the caller) and the
/// background task draining its stderr.
pub struct Streaming {
    pub child: tokio::process::Child,
    stderr: Option<tokio::task::JoinHandle<Vec<u8>>>,
}

impl Streaming {
    /// The kept stderr bytes, once the stream has ended.
    pub async fn stderr(&mut self) -> Vec<u8> {
        match self.stderr.take() {
            Some(task) => task.await.unwrap_or_default(),
            None => Vec::new(),
        }
    }
}

fn drain_stderr(
    mut stderr: impl AsyncRead + Unpin + Send + 'static,
) -> tokio::task::JoinHandle<Vec<u8>> {
    tokio::spawn(async move {
        use tokio::io::AsyncReadExt;
        let mut kept = Vec::new();
        let mut buf = vec![0u8; 8192];
        while let Ok(n) = stderr.read(&mut buf).await {
            if n == 0 {
                break;
            }
            let room = STREAM_STDERR_KEEP.saturating_sub(kept.len());
            kept.extend_from_slice(&buf[..n.min(room)]);
        }
        kept
    })
}

pub struct Captured {
    pub success: bool,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone)]
pub struct Sprite {
    pub cli: PathBuf,
    pub org: String,
    pub name: String,
    /// Values that must never appear in diagnostics.
    pub secrets: Vec<String>,
}

/// The remote runner invocation: `$HOME/.goodflows/runners/<id>/goodflows
/// __sprite-agent <op> <args>`, with every value passed as a positional
/// argument rather than interpolated into shell text.
///
/// The runner id `current` names the runner the supervisor was last verified
/// with, so recovery commands need not provision anything.
const AGENT: &str = r#"d=$1; shift
if [ "$d" = current ]; then d=$(cat "$HOME/.goodflows/runners/current") || { echo "no GoodFlows runner is installed on this Sprite" >&2; exit 97; }; fi
exec "$HOME/.goodflows/runners/$d/goodflows" __sprite-agent "$@""#;

/// Longest one ordinary (non-streaming) Sprite CLI call may take: listing,
/// create, destroy, and short agent operations such as status, ownership
/// checks or cancel. Expiry kills the call and reports the platform as
/// unreachable (exit 3).
pub const CALL_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Longest a call that moves bulk data may take: uploads (runner install,
/// the source submission, which the Sprite itself bounds at
/// `UPLOAD_TIMEOUT_SECS`, plus admission and handoff) and saved-log reads.
pub const BULK_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(super::UPLOAD_TIMEOUT_SECS + 300);

/// The bound for one call (debug builds: `GOODFLOWS_TEST_CALL_TIMEOUT_SECS`
/// shortens every bound for tests).
pub fn call_bound(bound: std::time::Duration) -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return std::time::Duration::from_secs(secs);
    }
    bound
}

/// What [`AGENT`] reports when the Sprite has no recorded runner: no agent
/// ran, so the operation definitely did nothing.
pub const NO_RUNNER: &str = "no GoodFlows runner is installed on this Sprite";

/// The Sprite platform could not be reached (an unresolved transport
/// failure, exit 3), as opposed to a configuration problem (exit 1).
#[derive(Debug)]
pub struct Unreachable(pub String);

impl std::fmt::Display for Unreachable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Unreachable {}

/// The platform did not prove that the credential acts in the requested
/// organization: a configuration/authorization error (exit 1). The real CLI
/// selects the organization from its token (`SPRITE_TOKEN` or the stored
/// token), and a listing under `-o <org>` can report a different one, so the
/// listing's own `org.name` and every listed Sprite's `organization` are the
/// only evidence of where a later create, exec or destroy would act.
#[derive(Debug)]
pub struct OrgMismatch(pub String);

impl std::fmt::Display for OrgMismatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for OrgMismatch {}

/// Resolve the organization once: `SPRITE_ORG`, else the CLI's current
/// default organization. The result is recorded in the local receipt.
pub async fn resolve_org(cli: &Path) -> Result<String> {
    if let Some(org) = std::env::var("SPRITE_ORG").ok().filter(|o| !o.is_empty()) {
        return Ok(org);
    }
    let bound = call_bound(CALL_TIMEOUT);
    let output = tokio::time::timeout(
        bound,
        Command::new(cli)
            .args(["org", "list"])
            .stdin(Stdio::null())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| {
        Unreachable(format!(
            "`sprite org list` did not finish within {}s",
            bound.as_secs()
        ))
    })?
    .context("run `sprite org list`")?;
    if !output.status.success() {
        return Err(Unreachable(format!(
            "`sprite org list` failed: {}",
            sanitize(&output.stderr, &[])
        ))
        .into());
    }
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(name) = line.trim().strip_suffix("(current)") {
            let name = name.trim();
            if !name.is_empty() && !name.contains(char::is_whitespace) {
                return Ok(name.to_owned());
            }
        }
    }
    bail!("could not determine the Sprite organization; set SPRITE_ORG or run `sprite org set-default <org>`")
}

impl Sprite {
    fn command(&self) -> Command {
        let mut command = Command::new(&self.cli);
        command.kill_on_drop(true);
        command
    }

    fn exec_args(&self, stdin: bool) -> Vec<String> {
        vec![
            "exec".into(),
            "-o".into(),
            self.org.clone(),
            "-s".into(),
            self.name.clone(),
            "--no-port-forward".into(),
            if stdin { "--stdin" } else { "--no-stdin" }.into(),
            "--".into(),
        ]
    }

    pub fn diagnostic(&self, captured: &Captured) -> String {
        sanitize(&captured.stderr, &self.secrets)
    }

    /// Run one CLI call to completion within its bound ([`BULK_TIMEOUT`]
    /// when it sends stdin, otherwise [`CALL_TIMEOUT`]). Expiry kills the
    /// call and fails with [`Unreachable`].
    async fn capture(
        &self,
        args: &[String],
        stdin: Option<&mut (dyn AsyncRead + Unpin + Send)>,
    ) -> Result<Captured> {
        let bound = if stdin.is_some() {
            BULK_TIMEOUT
        } else {
            CALL_TIMEOUT
        };
        self.capture_within(args, stdin, bound).await
    }

    async fn capture_within(
        &self,
        args: &[String],
        stdin: Option<&mut (dyn AsyncRead + Unpin + Send)>,
        bound: std::time::Duration,
    ) -> Result<Captured> {
        let bound = call_bound(bound);
        match tokio::time::timeout(bound, self.capture_unbounded(args, stdin)).await {
            Ok(result) => result,
            // Dropping the call's future kills the CLI process (kill_on_drop).
            Err(_) => Err(Unreachable(format!(
                "the Sprite CLI call `{}` for {} did not finish within {}s",
                args.first().map(String::as_str).unwrap_or_default(),
                self.name,
                bound.as_secs()
            ))
            .into()),
        }
    }

    async fn capture_unbounded(
        &self,
        args: &[String],
        stdin: Option<&mut (dyn AsyncRead + Unpin + Send)>,
    ) -> Result<Captured> {
        let mut command = self.command();
        command
            .args(args)
            .stdin(if stdin.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start the Sprite CLI ({})", self.cli.display()))?;
        let writer = async {
            if let (Some(input), Some(mut pipe)) = (stdin, child.stdin.take()) {
                // A closed pipe is reported by the command's own status.
                let _ = tokio::io::copy(input, &mut pipe).await;
                let _ = pipe.shutdown().await;
            }
        };
        let (_, output) = tokio::join!(writer, async {
            let mut stdout = child.stdout.take();
            let mut stderr = child.stderr.take();
            let mut out = Vec::new();
            let mut err = Vec::new();
            let read_out = async {
                if let Some(s) = &mut stdout {
                    let _ = tokio::io::AsyncReadExt::read_to_end(s, &mut out).await;
                }
            };
            let read_err = async {
                if let Some(s) = &mut stderr {
                    let _ = tokio::io::AsyncReadExt::read_to_end(s, &mut err).await;
                }
            };
            tokio::join!(read_out, read_err);
            (out, err)
        });
        let status = child.wait().await?;
        Ok(Captured {
            success: status.success(),
            stdout: output.0,
            stderr: output.1,
        })
    }

    /// Run a fixed command on the Sprite with no stdin.
    pub async fn exec(&self, argv: &[&str]) -> Result<Captured> {
        let mut args = self.exec_args(false);
        args.extend(argv.iter().map(|a| a.to_string()));
        self.capture(&args, None).await
    }

    /// Run a fixed command on the Sprite, streaming `input` over WebSocket stdin.
    pub async fn exec_stdin(
        &self,
        argv: &[&str],
        input: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<Captured> {
        let mut args = self.exec_args(true);
        args.extend(argv.iter().map(|a| a.to_string()));
        self.capture(&args, Some(input)).await
    }

    /// Invoke the provisioned runner's agent.
    pub async fn agent(&self, runner: &str, op: &[&str]) -> Result<Captured> {
        let mut argv = vec!["sh", "-c", AGENT, "goodflows-agent", runner];
        argv.extend_from_slice(op);
        self.exec(&argv).await
    }

    /// Invoke an agent operation whose output can be large (saved logs),
    /// bounded like an upload ([`BULK_TIMEOUT`]).
    pub async fn agent_bulk(&self, runner: &str, op: &[&str]) -> Result<Captured> {
        let mut args = self.exec_args(false);
        for a in ["sh", "-c", AGENT, "goodflows-agent", runner] {
            args.push(a.into());
        }
        args.extend(op.iter().map(|a| a.to_string()));
        self.capture_within(&args, None, BULK_TIMEOUT).await
    }

    pub async fn agent_stdin(
        &self,
        runner: &str,
        op: &[&str],
        input: &mut (dyn AsyncRead + Unpin + Send),
    ) -> Result<Captured> {
        let mut argv = vec!["sh", "-c", AGENT, "goodflows-agent", runner];
        argv.extend_from_slice(op);
        self.exec_stdin(&argv, input).await
    }

    /// Spawn a long-lived agent call whose stdout the caller streams. Its
    /// stderr is drained in the background (a noisy remote can never stall
    /// the stream), keeping the first [`STREAM_STDERR_KEEP`] bytes for
    /// diagnostics.
    pub fn agent_stream(&self, runner: &str, op: &[&str]) -> Result<Streaming> {
        let mut args = self.exec_args(false);
        for a in ["sh", "-c", AGENT, "goodflows-agent", runner] {
            args.push(a.into());
        }
        args.extend(op.iter().map(|a| a.to_string()));
        let mut command = self.command();
        command
            .args(&args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command
            .spawn()
            .with_context(|| format!("failed to start the Sprite CLI ({})", self.cli.display()))?;
        let stderr = child.stderr.take().map(drain_stderr);
        Ok(Streaming { child, stderr })
    }

    /// The Sprite's platform identifier if it exists, `Some(None)` when it
    /// exists without a reported identifier, `None` when absent.
    ///
    /// Fails with [`OrgMismatch`] unless the listing reports exactly the
    /// requested organization, both at the top level and for every listed
    /// Sprite. Every remote path looks the Sprite up before its first
    /// mutation, so this is the single verification point.
    pub async fn lookup(&self) -> Result<Option<Option<String>>> {
        let args: Vec<String> = ["list", "-o", &self.org, "--prefix", &self.name, "--json"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let captured = self.capture(&args, None).await?;
        if !captured.success {
            bail!(
                "could not list Sprites in organization {}: {}",
                self.org,
                self.diagnostic(&captured)
            );
        }
        let value: serde_json::Value = serde_json::from_slice(&captured.stdout)
            .context("`sprite list --json` returned unexpected output")?;
        let refuse = |reported: &str| {
            OrgMismatch(format!(
                "organization mismatch: requested {} but {reported}; the Sprite CLI acts in its credential's organization (SPRITE_TOKEN or the stored token), so GoodFlows refuses to create, use or destroy {}. Use a credential for {}, or set SPRITE_ORG to the credential's organization",
                self.org, self.name, self.org
            ))
        };
        match value
            .get("org")
            .and_then(|o| o.get("name"))
            .and_then(|n| n.as_str())
            .filter(|n| !n.is_empty())
        {
            Some(reported) if reported == self.org => {}
            Some(reported) => {
                return Err(refuse(&format!("the platform reports organization {reported}")).into())
            }
            None => {
                return Err(
                    refuse("`sprite list --json` did not report the platform organization").into(),
                )
            }
        }
        let sprites = value
            .get("sprites")
            .and_then(|s| s.as_array())
            .context("`sprite list --json` returned no Sprite list")?;
        for listed in sprites {
            let name = listed
                .get("name")
                .and_then(|n| n.as_str())
                .unwrap_or("(unnamed)");
            match listed.get("organization").and_then(|o| o.as_str()) {
                Some(org) if org == self.org => {}
                Some(org) => {
                    return Err(refuse(&format!(
                        "the platform lists Sprite {name} in organization {org}"
                    ))
                    .into())
                }
                None => {
                    return Err(refuse(&format!(
                        "`sprite list --json` did not report the organization of Sprite {name}"
                    ))
                    .into())
                }
            }
        }
        let matches: Vec<_> = sprites
            .iter()
            .filter(|s| s.get("name").and_then(|n| n.as_str()) == Some(self.name.as_str()))
            .collect();
        if matches.len() > 1 {
            bail!(
                "`sprite list` reported {} Sprites named {}; refusing to guess which one is owned",
                matches.len(),
                self.name
            );
        }
        Ok(matches
            .first()
            .map(|s| s.get("id").and_then(|i| i.as_str()).map(str::to_owned)))
    }

    /// `true` only when this call's own create succeeded. A conflict, an
    /// error or a lost acknowledgement all return `false`; callers must then
    /// verify ownership and never initialize the resource.
    pub async fn create(&self) -> Result<bool> {
        let args: Vec<String> = ["create", "-o", &self.org, "--skip-console", &self.name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        match self.capture(&args, None).await {
            Ok(captured) => Ok(captured.success),
            // A create that timed out may still have happened: like a lost
            // acknowledgement, never initialize it.
            Err(error) if error.is::<Unreachable>() => Ok(false),
            Err(error) => Err(error),
        }
    }

    pub async fn destroy(&self) -> Result<Captured> {
        let args: Vec<String> = ["destroy", "-o", &self.org, "--force", &self.name]
            .iter()
            .map(|s| s.to_string())
            .collect();
        self.capture(&args, None).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_noisy_stream_stderr_is_drained_and_bounded() {
        // Far more stderr than a pipe buffer holds: without draining, the
        // writer would block before closing stdout.
        let sprite = Sprite {
            cli: PathBuf::from("sh"),
            org: String::new(),
            name: String::new(),
            secrets: Vec::new(),
        };
        let mut command = sprite.command();
        command
            .args(["-c", "head -c 1048576 /dev/zero >&2; echo done"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        let stderr = child.stderr.take().map(drain_stderr);
        let mut stream = Streaming { child, stderr };
        let mut out = String::new();
        let read = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            tokio::io::AsyncReadExt::read_to_string(
                stream.child.stdout.as_mut().unwrap(),
                &mut out,
            ),
        )
        .await;
        assert!(read.is_ok(), "stdout stalled behind undrained stderr");
        assert_eq!(out, "done\n");
        assert!(stream.child.wait().await.unwrap().success());
        assert_eq!(stream.stderr().await.len(), STREAM_STDERR_KEEP);
    }

    #[test]
    fn the_agent_wrapper_reports_a_missing_runner_with_the_known_text() {
        assert!(AGENT.contains(&format!("echo \"{NO_RUNNER}\" >&2; exit 97")));
    }

    #[test]
    fn sanitize_redacts_and_bounds() {
        let text = sanitize(
            b"failed with token abcd1234 here\x1b[31m",
            &["abcd1234".into()],
        );
        assert_eq!(text, "failed with token [redacted] here[31m");
        assert!(sanitize(&vec![b'x'; 5000], &[]).len() < 2100);
    }
}
