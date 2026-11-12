//! `goodflows mcp`: an MCP server on stdin and stdout. Each tool runs this
//! binary with JSON output in the server's working directory, so config
//! discovery, flags and exit codes are the CLI's own, and returns only what
//! an agent needs: the summary and bounded failure details, never raw job
//! output. The filtering follows the Amp plugin (`.amp/plugins/goodflows.ts`),
//! with the same limits and fields.

use anyhow::Result;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{CallToolResult, ContentBlock, Implementation, ServerCapabilities, ServerConfig};
use rmcp::service::RequestContext;
use rmcp::{schemars, tool, tool_handler, tool_router, RoleServer, ServerHandler, ServiceExt};
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::future::Future;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::sync::watch;

const DIAGNOSTIC_LIMIT: usize = 60_000;
const LINE_LIMIT: usize = 1_048_576;
/// Called-run failures retained, and the characters kept of each field, so
/// many large child summaries cannot grow memory beyond what a result sends.
const CALLED_LIMIT: usize = 200;
const FIELD_LIMIT: usize = 4096;
const ERROR_LIMIT: usize = 8192;

/// Serve until the client closes stdin or a signal arrives. Tool calls still
/// running then stop their goodflows process, which cancels its jobs, and
/// the server exits once every one has.
pub async fn serve(
    config: Option<PathBuf>,
    mut cancel: watch::Receiver<Option<i32>>,
) -> Result<u8> {
    let running = Arc::new(watch::Sender::new(0));
    let server = Server {
        config,
        running: running.clone(),
        tool_router: Server::tool_router(),
    };
    let service = server.serve(rmcp::transport::stdio()).await?;
    let token = service.cancellation_token();
    let signal = tokio::select! {
        quit = service.waiting() => {
            quit?;
            None
        }
        _ = cancel.changed() => {
            token.cancel();
            *cancel.borrow()
        }
    };
    running.subscribe().wait_for(|count| *count == 0).await?;
    match signal {
        // Exit at once: a blocking stdin read would hold up runtime shutdown.
        Some(signal) => std::process::exit(128 + signal),
        None => Ok(0),
    }
}

#[derive(Clone)]
struct Server {
    /// `goodflows mcp --config`, for calls that pass no config.
    config: Option<PathBuf>,
    /// goodflows processes the tools are running.
    running: Arc<watch::Sender<usize>>,
    tool_router: ToolRouter<Self>,
}

/// Counts one running goodflows process while it lives.
struct Running(Arc<watch::Sender<usize>>);

impl Running {
    fn start(count: &Arc<watch::Sender<usize>>) -> Self {
        count.send_modify(|n| *n += 1);
        Self(count.clone())
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct RunParams {
    /// Workflow to run (default: the workflow named `default`, or the config's top-level jobs)
    workflow: Option<String>,
    /// Config file (default: the server's --config, else goodflows.toml or .goodflows.toml in its working directory)
    config: Option<String>,
    /// Apply this environment profile (--env)
    env: Option<String>,
    /// Run only these jobs and the jobs they depend on (--jobs)
    #[serde(default)]
    jobs: Vec<String>,
    /// Rerun only the jobs the last run named as failed, in its workflow and environment (--failed)
    #[serde(default)]
    failed: bool,
    /// Maximum concurrent jobs; 0 uses the logical CPU count (--parallelism)
    parallelism: Option<u64>,
    /// true for --fail-fast, false for --no-fail-fast (default: the config's setting)
    fail_fast: Option<bool>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ValidateParams {
    /// Workflow to check (default: every workflow)
    workflow: Option<String>,
    /// Config file (default: the server's --config, else goodflows.toml or .goodflows.toml in its working directory)
    config: Option<String>,
    /// Apply this environment profile (--env)
    env: Option<String>,
    /// Check the selection these jobs make with their dependencies (--jobs; needs a workflow when the config has named workflows)
    #[serde(default)]
    jobs: Vec<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ListParams {
    /// Workflow whose jobs to list (default: the workflow named `default`, or the config's top-level jobs)
    workflow: Option<String>,
    /// Config file (default: the server's --config, else goodflows.toml or .goodflows.toml in its working directory)
    config: Option<String>,
    /// Apply this environment profile (--env)
    env: Option<String>,
    /// List the workflows instead of one workflow's jobs (--workflows)
    #[serde(default)]
    workflows: bool,
}

#[derive(Deserialize, schemars::JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct LastParams {
    /// Config file (default: the server's --config, else goodflows.toml or .goodflows.toml in its working directory)
    config: Option<String>,
}

#[tool_router]
impl Server {
    #[tool(
        description = "Run the project's goodflows checks and wait for them. Returns whether they passed, the exit code, the run summary counts and, for each job that did not pass (including jobs of called workflows), its command, error and bounded stdout and stderr tails. Never returns full job output."
    )]
    async fn run(
        &self,
        Parameters(p): Parameters<RunParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let mut args = self.command(&["run", "--json=summary"], p.config);
        option(&mut args, "--env", p.env);
        jobs(&mut args, &p.jobs);
        if p.failed {
            args.push("--failed".into());
        }
        option(
            &mut args,
            "--parallelism",
            p.parallelism.map(|n| n.to_string()),
        );
        match p.fail_fast {
            Some(true) => args.push("--fail-fast".into()),
            Some(false) => args.push("--no-fail-fast".into()),
            None => {}
        }
        workflow(&mut args, p.workflow);
        let execution = self.execute(args, context.ct.cancelled()).await;
        checks(execution, true)
    }

    #[tool(
        description = "Show the last local goodflows run's results again without running anything: whether it passed, its summary counts and its failures with bounded output tails."
    )]
    async fn last(
        &self,
        Parameters(p): Parameters<LastParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let args = self.command(&["last", "--json"], p.config);
        checks(self.execute(args, context.ct.cancelled()).await, false)
    }

    #[tool(
        description = "Validate the goodflows config and a job selection without running commands. Returns the config_valid report, or the problems found."
    )]
    async fn validate(
        &self,
        Parameters(p): Parameters<ValidateParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let mut args = self.command(&["validate", "--json"], p.config);
        option(&mut args, "--env", p.env);
        jobs(&mut args, &p.jobs);
        workflow(&mut args, p.workflow);
        report(self.execute(args, context.ct.cancelled()).await)
    }

    #[tool(
        description = "List a workflow's jobs, their dependencies and the settings a run would use, or with workflows=true the available workflows and the default one."
    )]
    async fn list(
        &self,
        Parameters(p): Parameters<ListParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let mut args = self.command(&["list", "--json"], p.config);
        option(&mut args, "--env", p.env);
        if p.workflows {
            args.push("--workflows".into());
        }
        workflow(&mut args, p.workflow);
        report(self.execute(args, context.ct.cancelled()).await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("goodflows", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Runs this project's goodflows checks. Use `run` to check your work and fix what it reports until `passed` is true; `last` shows the previous run again.",
            )
    }
}

fn option(args: &mut Vec<OsString>, flag: &str, value: Option<String>) {
    if let Some(value) = value {
        args.extend([flag.into(), value.into()]);
    }
}

fn jobs(args: &mut Vec<OsString>, jobs: &[String]) {
    if !jobs.is_empty() {
        args.extend(["--jobs".into(), jobs.join(",").into()]);
    }
}

/// The workflow goes last, after `--`, so a name cannot read as a flag.
fn workflow(args: &mut Vec<OsString>, workflow: Option<String>) {
    if let Some(workflow) = workflow {
        args.extend(["--".into(), workflow.into()]);
    }
}

impl Server {
    fn command(&self, words: &[&str], config: Option<String>) -> Vec<OsString> {
        let mut args: Vec<OsString> = words.iter().map(OsString::from).collect();
        if let Some(config) = config
            .map(OsString::from)
            .or_else(|| self.config.clone().map(Into::into))
        {
            args.extend(["--config".into(), config]);
        }
        args
    }

    /// Run goodflows with `args` and keep only what a result reports. When
    /// `cancelled` completes first, goodflows gets SIGTERM, which cancels its
    /// jobs, and its own result is still read to the end.
    async fn execute(
        &self,
        args: Vec<OsString>,
        cancelled: impl Future<Output = ()>,
    ) -> std::result::Result<Execution, String> {
        let program = std::env::current_exe().map_err(|e| format!("cannot find goodflows: {e}"))?;
        let mut child = tokio::process::Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot start goodflows: {e}"))?;
        let _running = Running::start(&self.running);
        let pid = child.id();
        let (stdout, stderr) = (child.stdout.take(), child.stderr.take());
        let mut execution = Execution::default();
        let mut stderr_tail = Vec::new();
        let status = {
            let work = async {
                let (read, _) = tokio::join!(
                    execution.read(stdout.expect("piped stdout")),
                    keep_tail(stderr.expect("piped stderr"), &mut stderr_tail)
                );
                read?;
                child.wait().await
            };
            tokio::pin!(work);
            tokio::pin!(cancelled);
            tokio::select! {
                biased;
                status = &mut work => status,
                () = &mut cancelled => {
                    if let Some(pid) = pid {
                        // SAFETY: kill only signals; the child is not yet reaped.
                        unsafe { libc::kill(pid as i32, libc::SIGTERM) };
                    }
                    work.await
                }
            }
        }
        .map_err(|e| format!("goodflows: {e}"))?;
        execution.exit_code = status.code();
        let stderr = String::from_utf8_lossy(&stderr_tail);
        if !stderr.is_empty() {
            execution.note(&stderr);
        }
        if let Some(signal) = std::os::unix::process::ExitStatusExt::signal(&status) {
            execution.note(&format!("GoodFlows terminated by signal {signal}."));
        }
        Ok(execution)
    }
}

/// Keep the last [`ERROR_LIMIT`] bytes of `reader`.
async fn keep_tail(mut reader: impl AsyncRead + Unpin, tail: &mut Vec<u8>) {
    let mut buffer = [0; 8192];
    while let Ok(n @ 1..) = reader.read(&mut buffer).await {
        tail.extend_from_slice(&buffer[..n]);
        let excess = tail.len().saturating_sub(ERROR_LIMIT);
        tail.drain(..excess);
    }
}

/// What a result reports from one goodflows process: at most one root
/// `summary` or `jobs`, `workflows` or `config_valid` event, bounded failures
/// of called workflow runs, and a small error excerpt. Everything else, such
/// as job output, is dropped as it is read.
#[derive(Default)]
struct Execution {
    exit_code: Option<i32>,
    report: Option<Value>,
    /// Bounded copies of the non-passing jobs of called workflow runs, whose
    /// summaries carry `parent_run_id`, with their run.
    called: Vec<(Value, Value)>,
    called_omitted: usize,
    error: String,
    /// goodflows reported an error, or wrote output that is not its JSON.
    failed: bool,
}

impl Execution {
    fn note(&mut self, message: &str) {
        self.error = bounded(&format!("{}\n{message}", self.error), ERROR_LIMIT);
    }

    fn fail(&mut self, message: &str) {
        self.failed = true;
        self.note(message);
    }

    /// Read JSON Lines events, dropping any line over [`LINE_LIMIT`].
    async fn read(&mut self, mut reader: impl AsyncRead + Unpin) -> std::io::Result<()> {
        let mut buffer = vec![0; 65536];
        let mut pending = Vec::new();
        let mut dropping = false;
        loop {
            let n = reader.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            for part in buffer[..n].split_inclusive(|b| *b == b'\n') {
                if !dropping {
                    if pending.len() + part.len() > LINE_LIMIT {
                        self.fail("GoodFlows JSON event exceeded the 1 MiB limit.");
                        pending.clear();
                        dropping = true;
                    } else {
                        pending.extend_from_slice(part);
                    }
                }
                if part.ends_with(b"\n") {
                    if !dropping {
                        self.consume(&pending);
                    }
                    pending.clear();
                    dropping = false;
                }
            }
        }
        if !dropping {
            self.consume(&pending);
        }
        Ok(())
    }

    fn consume(&mut self, line: &[u8]) {
        if line.trim_ascii().is_empty() {
            return;
        }
        let Ok(event) = serde_json::from_slice::<Value>(line) else {
            let line = String::from_utf8_lossy(line);
            return self.fail(&format!(
                "Invalid GoodFlows JSON output: {}",
                bounded(&line, 1024)
            ));
        };
        let kind = event["type"].as_str().unwrap_or_default();
        if event["schema_version"] != 1 || kind.is_empty() {
            return self.fail("Unsupported or invalid GoodFlows JSON event.");
        }
        match kind {
            // A called workflow run's own summary; the root summary has no
            // parent and comes last.
            "summary" if event.get("parent_run_id").is_some() => {
                let run: Map<_, _> = ["workflow", "run_id", "caller_job", "parent_run_id"]
                    .into_iter()
                    .map(|key| (key.to_owned(), scalar(&event[key], head)))
                    .collect();
                for job in event["results"].as_array().into_iter().flatten() {
                    if !job.is_object() || job["status"] == "passed" {
                        continue;
                    }
                    if self.called.len() < CALLED_LIMIT {
                        self.called.push((Value::Object(run.clone()), project(job)));
                    } else {
                        self.called_omitted += 1;
                    }
                }
            }
            "summary" | "workflows" | "jobs" | "config_valid" => {
                if self.report.is_some() {
                    self.fail(&format!("Duplicate {kind} event."));
                }
                self.report = Some(event);
            }
            "error" => {
                let mut message = event["message"]
                    .as_str()
                    .unwrap_or("GoodFlows reported an error.")
                    .to_owned();
                for problem in event["problems"].as_array().into_iter().flatten() {
                    message.push_str(&format!("\n- {}", string(problem)));
                }
                if let Some(hint) = event["hint"].as_str() {
                    message.push_str(&format!("\nhint: {hint}"));
                }
                self.fail(&message);
            }
            _ => {}
        }
    }
}

fn head(text: &str) -> String {
    text.chars().take(FIELD_LIMIT).collect()
}

fn tail(text: &str) -> String {
    let skip = text.chars().count().saturating_sub(FIELD_LIMIT);
    text.chars().skip(skip).collect()
}

fn bounded(text: &str, limit: usize) -> String {
    match text.char_indices().nth(limit) {
        None => text.to_owned(),
        Some((end, _)) => format!("{}\n[diagnostics truncated]", &text[..end]),
    }
}

/// A bounded string, a number, or null: objects, arrays and other values
/// become null.
fn scalar(value: &Value, keep: fn(&str) -> String) -> Value {
    match value {
        Value::String(text) => Value::String(keep(text)),
        Value::Number(_) => value.clone(),
        _ => Value::Null,
    }
}

/// A value as text, as JavaScript's `String()` writes it; null is empty.
fn string(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// Only the fields a result reads, each bounded; trimmed tails stay marked
/// truncated.
fn project(job: &Value) -> Value {
    let trimmed = |key: &str| {
        job[key]
            .as_str()
            .is_some_and(|t| t.chars().count() > FIELD_LIMIT)
    };
    json!({
        "name": scalar(&job["name"], head), "status": scalar(&job["status"], head),
        "exit_code": scalar(&job["exit_code"], head), "command": scalar(&job["command"], head),
        "error": scalar(&job["error"], head),
        "stdout_tail": scalar(&job["stdout_tail"], tail), "stderr_tail": scalar(&job["stderr_tail"], tail),
        "stdout_truncated": job["stdout_truncated"] == true || trimmed("stdout_tail"),
        "stderr_truncated": job["stderr_truncated"] == true || trimmed("stderr_tail"),
    })
}

/// The longest suffix of `text` whose JSON string, quotes included, fits in
/// `budget` characters: escaping can expand control characters.
fn json_tail(text: &str, budget: usize) -> &str {
    let mut size = 2;
    let mut start = text.len();
    for (index, c) in text.char_indices().rev() {
        size += match c {
            '"' | '\\' | '\n' | '\r' | '\t' | '\u{8}' | '\u{c}' => 2,
            c if (c as u32) < 0x20 => 6,
            c => c.len_utf16(),
        };
        if size > budget {
            break;
        }
        start = index;
    }
    &text[start..]
}

/// The result of `run` or `last`: a pass/fail line, then the summary counts
/// with each failure's metadata, then bounded log excerpts. Metadata is
/// reserved before logs so an early noisy job cannot hide later failures.
fn checks(execution: std::result::Result<Execution, String>, exit_code: bool) -> CallToolResult {
    let execution = match execution {
        Ok(execution) => execution,
        Err(error) => return incomplete(None, &error),
    };
    let summary = match &execution.report {
        Some(summary)
            if summary["type"] == "summary"
                && summary["success"].is_boolean()
                && summary["results"].as_array().is_some_and(|results| {
                    results
                        .iter()
                        .all(|job| job["name"].is_string() && job["status"].is_string())
                }) =>
        {
            summary
        }
        _ if !execution.error.trim().is_empty() => {
            return incomplete(execution.exit_code, &execution.error)
        }
        _ => return incomplete(execution.exit_code, "Missing or invalid GoodFlows summary."),
    };
    let results = summary["results"].as_array().expect("checked above");
    let passed = execution.exit_code == Some(0)
        && !execution.failed
        && summary["success"] == true
        && results
            .iter()
            .all(|job| job["status"] == "passed" || job["status"] == "skipped");
    // Non-passing jobs of the root run, then of each called workflow run,
    // labelled with that run so the agent can see which called job failed.
    let failures: Vec<(Option<&Value>, &Value)> = results
        .iter()
        .filter(|job| job["status"] != "passed")
        .map(|job| (None, job))
        .chain(execution.called.iter().map(|(run, job)| (Some(run), job)))
        .collect();
    let labelled = |run: Option<&Value>, mut entry: Map<String, Value>| {
        if let Some(run) = run {
            entry.insert("called_run".into(), run.clone());
        }
        Value::Object(entry)
    };
    let metadata: Vec<_> = failures
        .iter()
        .map(|(run, job)| {
            let error = match &job["error"] {
                Value::Null => Value::Null,
                error => json!(bounded(&string(error), 1024)),
            };
            let entry = json!({
                "name": job["name"], "status": job["status"], "exit_code": job["exit_code"],
                "command": bounded(&string(&job["command"]), 2048), "error": error,
            });
            labelled(*run, entry.as_object().expect("an object").clone())
        })
        .collect();
    let budget = 4096.min(20_000 / (failures.len() * 2).max(1));
    let logs: Vec<_> = failures
        .iter()
        .map(|(run, job)| {
            let text = |key: &str| job[key].as_str().unwrap_or_default();
            let (stdout, stderr) = (
                json_tail(text("stdout_tail"), budget),
                json_tail(text("stderr_tail"), budget),
            );
            let truncated = job["stdout_truncated"] == true
                || job["stderr_truncated"] == true
                || stdout.len() < text("stdout_tail").len()
                || stderr.len() < text("stderr_tail").len();
            let entry = json!({
                "name": job["name"], "stdout_tail": stdout, "stderr_tail": stderr,
                "excerpts_truncated": truncated,
            });
            labelled(*run, entry.as_object().expect("an object").clone())
        })
        .collect();
    let mut diagnostics = summary.as_object().expect("a summary object").clone();
    diagnostics.remove("results");
    diagnostics.insert("passed".into(), json!(passed));
    if exit_code {
        diagnostics.insert("exit_code".into(), json!(execution.exit_code));
    }
    diagnostics.insert("error".into(), json!(execution.error));
    if execution.called_omitted > 0 {
        diagnostics.insert(
            "called_failures_omitted".into(),
            json!(execution.called_omitted),
        );
    }
    diagnostics.insert("results".into(), json!(metadata));
    let status = match (passed, exit_code) {
        (true, true) => "GoodFlows checks passed.".to_owned(),
        (false, true) => format!(
            "GoodFlows checks failed (exit code {}).",
            code(execution.exit_code)
        ),
        (true, false) => "The last GoodFlows run passed.".to_owned(),
        (false, false) => "The last GoodFlows run failed.".to_owned(),
    };
    let mut content = vec![
        ContentBlock::text(status),
        ContentBlock::text(bounded(&Value::Object(diagnostics).to_string(), 30_000)),
    ];
    if !logs.is_empty() {
        content.push(ContentBlock::text(format!(
            "Failure log excerpts (JSON):\n{}",
            bounded(&json!(logs).to_string(), 28_000)
        )));
    }
    CallToolResult::success(content)
}

/// The result of `list` or `validate`: goodflows's own report event.
fn report(execution: std::result::Result<Execution, String>) -> CallToolResult {
    match execution {
        Ok(Execution {
            exit_code: Some(0),
            report: Some(report),
            failed: false,
            ..
        }) => CallToolResult::success(vec![ContentBlock::text(bounded(
            &report.to_string(),
            DIAGNOSTIC_LIMIT,
        ))]),
        Ok(execution) => incomplete(execution.exit_code, &execution.error),
        Err(error) => incomplete(None, &error),
    }
}

/// goodflows could not do what the tool asked, such as for a config error
/// (exit code 2).
fn incomplete(exit_code: Option<i32>, error: &str) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(format!(
        "GoodFlows could not complete the command (exit code {}).\n{}",
        code(exit_code),
        bounded(error.trim(), ERROR_LIMIT)
    ))])
}

fn code(exit_code: Option<i32>) -> String {
    exit_code.map_or_else(|| "none".into(), |code| code.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn execution(events: &[Value], exit_code: i32) -> Execution {
        let mut execution = Execution {
            exit_code: Some(exit_code),
            ..Default::default()
        };
        for event in events {
            execution.consume(event.to_string().as_bytes());
        }
        execution
    }

    fn texts(result: &CallToolResult) -> Vec<String> {
        result
            .content
            .iter()
            .map(|block| block.as_text().expect("text content").text.clone())
            .collect()
    }

    fn summary(results: Value, success: bool) -> Value {
        json!({"schema_version":1, "type":"summary", "workflow":"ci", "run_id":"root",
               "success":success, "failed":1, "results":results})
    }

    #[test]
    fn job_output_is_dropped_and_called_failures_are_kept_and_bounded() {
        let big = format!("EARLY-MARKER{}", "x".repeat(10_000));
        let events = [
            json!({"schema_version":1, "type":"run_started", "workflow":"ci"}),
            json!({"schema_version":1, "type":"job_output", "data":"RAW-JOB-OUTPUT"}),
            json!({"schema_version":1, "type":"summary", "workflow":"suite", "run_id":"child",
                   "parent_run_id":"root", "caller_job":"call-suite", "success":false,
                   "results":[
                       {"name":"ok", "status":"passed", "stdout_tail":"PASSED-OUTPUT"},
                       {"name":"work", "status":"failed", "exit_code":3, "command":"make",
                        "error":"exited with 3", "stdout_tail":big, "stderr_tail":"boom\n",
                        "output_head":"HEAD-ONLY", "stdout_truncated":false}]}),
            summary(
                json!([{"name":"call-suite", "status":"failed", "error":"workflow 'suite' failed",
                        "command":null, "exit_code":null}]),
                false,
            ),
        ];
        let result = checks(Ok(execution(&events, 1)), true);
        assert_ne!(result.is_error, Some(true));
        let texts = texts(&result);
        let all = texts.join("\n");
        for absent in [
            "RAW-JOB-OUTPUT",
            "PASSED-OUTPUT",
            "EARLY-MARKER",
            "HEAD-ONLY",
        ] {
            assert!(!all.contains(absent), "{absent} in {all}");
        }
        assert_eq!(texts[0], "GoodFlows checks failed (exit code 1).");
        let diagnostics: Value = serde_json::from_str(&texts[1]).unwrap();
        assert_eq!(
            (&diagnostics["passed"], &diagnostics["exit_code"]),
            (&json!(false), &json!(1))
        );
        let results = diagnostics["results"].as_array().unwrap();
        assert_eq!(results.len(), 2, "{results:?}");
        assert_eq!(results[0]["name"], "call-suite");
        assert!(results[0].get("called_run").is_none());
        assert_eq!(results[1]["name"], "work");
        assert_eq!(results[1]["exit_code"], 3);
        assert_eq!(
            results[1]["called_run"],
            json!({"workflow":"suite", "run_id":"child", "caller_job":"call-suite", "parent_run_id":"root"})
        );
        let logs: Value = serde_json::from_str(
            texts[2]
                .strip_prefix("Failure log excerpts (JSON):\n")
                .unwrap(),
        )
        .unwrap();
        let work = &logs[1];
        assert_eq!(work["stderr_tail"], "boom\n");
        // Two failures: each stream keeps at most 4096 serialized characters.
        let stdout = work["stdout_tail"].as_str().unwrap();
        assert!(
            stdout.len() <= 4094 && stdout.ends_with('x'),
            "{}",
            stdout.len()
        );
        assert_eq!(work["excerpts_truncated"], true);
    }

    #[test]
    fn called_failures_beyond_the_limit_are_counted_not_kept() {
        let jobs: Vec<_> = (0..CALLED_LIMIT + 50)
            .map(|i| json!({"name":format!("j{i}"), "status":"failed"}))
            .collect();
        let child =
            json!({"schema_version":1, "type":"summary", "parent_run_id":"root", "results":jobs});
        let execution = execution(&[child], 1);
        assert_eq!(
            (execution.called.len(), execution.called_omitted),
            (CALLED_LIMIT, 50)
        );
        let result = checks(Ok(execution), true);
        // The summary is missing, so the result says so instead of reporting.
        assert_eq!(result.is_error, Some(true));
    }

    #[test]
    fn exit_status_decides_a_pass_even_when_the_summary_says_success() {
        let passing = summary(
            json!([{"name":"a", "status":"passed"}, {"name":"b", "status":"skipped"}]),
            true,
        );
        let result = checks(Ok(execution(std::slice::from_ref(&passing), 0)), true);
        assert_eq!(texts(&result)[0], "GoodFlows checks passed.");
        // Like the plugin, a skipped job is listed, though the run passed.
        let diagnostics: Value = serde_json::from_str(&texts(&result)[1]).unwrap();
        assert_eq!(diagnostics["passed"], true);
        assert_eq!(diagnostics["results"][0]["name"], "b");
        let result = checks(Ok(execution(&[passing], 130)), true);
        assert_eq!(
            texts(&result)[0],
            "GoodFlows checks failed (exit code 130)."
        );
        let diagnostics: Value = serde_json::from_str(&texts(&result)[1]).unwrap();
        assert_eq!(diagnostics["passed"], false);
    }

    #[test]
    fn errors_and_invalid_output_become_tool_errors() {
        let error = json!({"schema_version":1, "type":"error", "code":"config_invalid",
                           "message":"invalid config", "problems":["unknown job 'x'"]});
        let result = report(Ok(execution(&[error], 2)));
        assert_eq!(result.is_error, Some(true));
        let text = &texts(&result)[0];
        assert!(
            text.contains("exit code 2") && text.contains("- unknown job 'x'"),
            "{text}"
        );
        let mut garbage = Execution::default();
        garbage.consume(b"not json");
        garbage.consume(br#"{"type":"jobs"}"#);
        assert!(garbage.failed && garbage.error.contains("Unsupported or invalid"));
    }

    #[test]
    fn json_tail_budgets_the_escaped_size() {
        for (text, budget) in [
            ("\u{1}".repeat(100), 62),
            ("a\"b\\c\n".repeat(50), 41),
            ("é".repeat(30), 12),
        ] {
            let kept = json_tail(&text, budget);
            assert!(text.ends_with(kept));
            let size = |s: &str| serde_json::to_string(s).unwrap().encode_utf16().count();
            assert!(size(kept) <= budget, "{kept:?}");
            let longer = &text[text.len()
                - kept.len()
                - text[..text.len() - kept.len()]
                    .chars()
                    .last()
                    .unwrap()
                    .len_utf8()..];
            assert!(size(longer) > budget, "{longer:?}");
        }
    }
}
