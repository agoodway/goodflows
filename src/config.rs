use crate::expression::{Expression, Reference};
use anyhow::{anyhow, bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

/// A usage or config error: the caller has to change the command or the
/// config, because running it again fails the same way. `main` exits 2 for
/// it and reports `code`, `hint` and `problems` on the JSON `error` event.
#[derive(Debug)]
pub struct ConfigError {
    /// Stable machine-readable code, such as `unknown_job`.
    pub code: &'static str,
    message: String,
    pub hint: Option<String>,
    /// Every problem validation found, in order. Empty for other codes.
    pub problems: Vec<String>,
}

impl ConfigError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            problems: Vec::new(),
        }
    }

    fn with_hint(mut self, hint: Option<String>) -> Self {
        self.hint = hint;
        self
    }

    /// One `config_invalid` error naming every problem, or `Ok` when there
    /// are none.
    pub(crate) fn check(problems: Vec<anyhow::Error>) -> Result<()> {
        if problems.is_empty() {
            return Ok(());
        }
        Err(Self::invalid(
            problems.iter().map(|p| format!("{p:#}")).collect(),
        ))
    }

    /// A `config_invalid` error naming each of `problems`, which is not empty.
    fn invalid(problems: Vec<String>) -> anyhow::Error {
        let message = match problems.as_slice() {
            [problem] => problem.clone(),
            all => format!(
                "{} config problems:\n{}",
                all.len(),
                all.iter()
                    .map(|p| format!("  - {p}"))
                    .collect::<Vec<_>>()
                    .join("\n")
            ),
        };
        Self {
            problems,
            ..Self::new("config_invalid", message)
        }
        .into()
    }
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for ConfigError {}

/// Keep `result`'s error in `problems`, so validation goes on and reports
/// every problem at once.
fn note<T>(problems: &mut Vec<anyhow::Error>, result: Result<T>) -> Option<T> {
    result.map_err(|error| problems.push(error)).ok()
}

/// `did you mean 'x'?` for the `candidates` closest to the unknown `name`,
/// or `None` when no candidate is close.
fn did_you_mean<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<String> {
    let limit = (name.chars().count() / 3).max(1);
    let mut close: Vec<(usize, &str)> = candidates
        .into_iter()
        .map(|candidate| (edit_distance(name, candidate), candidate))
        .filter(|&(distance, _)| distance <= limit)
        .collect();
    close.sort();
    close.dedup();
    if close.is_empty() {
        return None;
    }
    let names: Vec<_> = close
        .iter()
        .take(3)
        .map(|(_, name)| format!("'{name}'"))
        .collect();
    Some(format!("did you mean {}?", names.join(" or ")))
}

/// Case-insensitive edit distance, counting a swap of two adjacent
/// characters as one edit.
fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.to_lowercase().chars().collect();
    let b: Vec<char> = b.to_lowercase().chars().collect();
    let mut d: Vec<Vec<usize>> = (0..=a.len())
        .map(|i| (0..=b.len()).map(|j| if i == 0 { j } else { i }).collect())
        .collect();
    for i in 1..=a.len() {
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            let mut best = (d[i - 1][j] + 1)
                .min(d[i][j - 1] + 1)
                .min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                best = best.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = best;
        }
    }
    d[a.len()][b.len()]
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub settings: Settings,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub jobs: Vec<Job>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub workflows: BTreeMap<String, Workflow>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub environments: BTreeMap<String, Environment>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reporting: Option<Reporting>,
    /// Explicit Sprite preparation hooks and environment allowlist. Used only
    /// by `run --sprite`; local runs ignore it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite: Option<SpriteConfig>,
}

/// Directory exposed to remote hooks and jobs for script-managed persistent
/// state. Reserved: never configurable, forwarded, or inherited by local runs.
pub const SPRITE_STATE_ENV: &str = "GOODFLOWS_SPRITE_STATE_DIR";

/// `[sprite]`: top-level only. `setup` and `resume` are required for remote
/// runs (use `true` for a deliberate no-op); partial configurations remain
/// valid for inspection.
#[derive(Debug, Deserialize, Serialize, Clone, Default)]
#[serde(deny_unknown_fields)]
pub struct SpriteConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<String>,
    /// Names of caller variables forwarded to remote hooks and jobs.
    #[serde(default)]
    pub pass_env: Vec<String>,
}

impl SpriteConfig {
    fn validate(&self) -> Result<()> {
        for (field, command) in [("setup", &self.setup), ("resume", &self.resume)] {
            if let Some(command) = command {
                if command.trim().is_empty() {
                    bail!("{field} must be a nonempty shell command (use `true` for a no-op)");
                }
                if command.contains('\0') {
                    bail!("{field} contains invalid control characters");
                }
            }
        }
        let mut seen = HashSet::new();
        for name in &self.pass_env {
            if !valid_env_name(name) {
                bail!(
                    "pass_env entry '{}' must be an environment variable name matching [A-Za-z_][A-Za-z0-9_]*",
                    name.escape_debug()
                );
            }
            if !seen.insert(name) {
                bail!("pass_env lists '{name}' more than once");
            }
        }
        Ok(())
    }
}

/// `settings.sprite_prefix`: 1-20 lowercase ASCII letters, digits or hyphens,
/// starting with a letter or digit.
pub fn valid_sprite_prefix(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 20
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'-')
}

/// The report family a job requests in `reports`. The wire version it is
/// collected as is [`crate::reporting::contract::REPORT_FORMAT`], so a later
/// contract version needs no configuration change.
pub const REPORT_FAMILY: &str = "test-timings";
pub const RESERVED_ENV: [&str; 2] = ["GOODFLOWS_RUN_ID", "GOODFLOWS_REPORT_DIR"];
/// Partition identity injected into every partitioned instance. Deliberately
/// separate from `RESERVED_ENV`, which reporting scrubs from every command.
pub const PARTITION_ENV: [&str; 2] = ["GOODFLOWS_PARTITION", "GOODFLOWS_PARTITION_COUNT"];

/// Selected `--env` profile name. Set by the runner; absent when no profile is selected.
pub const ENVIRONMENT_ENV: &str = "GOODFLOWS_ENVIRONMENT";
/// Per-command output file path. Set by the runner for each command that may
/// publish outputs; never inherited from the caller.
pub const OUTPUT_ENV: &str = "GOODFLOWS_OUTPUT";
pub const MAX_PARTITIONS: u32 = 64;
const DEFAULT_SPOOL_DIR: &str = ".goodflows/reports";
/// History directory beside the config file when `history_dir` is unset.
pub const DEFAULT_HISTORY_DIR: &str = "goodflows-history";
pub const DEFAULT_LOG_RUNS: u64 = 10;
pub const DEFAULT_HISTORY_RUNS: u64 = 1000;
/// A default, not a maximum: settings and flags may raise it.
pub const DEFAULT_HISTORY_BYTES: u64 = 10_485_760;

/// Opt-in publishing of job reports. Only environment variable *names* are
/// configured; the endpoint and key values are read by the publisher at runtime.
#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Reporting {
    pub project: String,
    #[serde(default = "default_url_env")]
    pub url_env: String,
    #[serde(default = "default_token_env")]
    pub token_env: String,
    /// Resolved to an absolute path by `Config::load`.
    pub spool_dir: Option<PathBuf>,
    /// Upload each invocation's run record. Controls upload only: local
    /// history files keep their own settings and flags.
    #[serde(
        default,
        deserialize_with = "history_flag",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub history: bool,
    // Present only to reject credentials written inline with a clear message.
    #[serde(default, skip_serializing)]
    token: Option<toml::Value>,
    #[serde(default, skip_serializing)]
    api_key: Option<toml::Value>,
    #[serde(default, skip_serializing)]
    url: Option<toml::Value>,
}

/// A boolean whose error names the key, which serde's type error does not.
fn history_flag<'de, D: serde::Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    bool::deserialize(deserializer)
        .map_err(|_| serde::de::Error::custom("reporting.history must be a boolean"))
}

fn default_url_env() -> String {
    "GOODFLOWS_URL".into()
}

fn default_token_env() -> String {
    "GOODFLOWS_API_KEY".into()
}

impl Reporting {
    pub fn spool_dir(&self) -> &Path {
        self.spool_dir
            .as_deref()
            .unwrap_or(Path::new(DEFAULT_SPOOL_DIR))
    }

    fn validate(&self) -> Result<()> {
        if self.token.is_some() || self.api_key.is_some() || self.url.is_some() {
            bail!("reporting credentials must not be written inline; name environment variables with url_env and token_env");
        }
        if !valid_project_slug(&self.project) {
            bail!("reporting project must be 1-64 bytes matching [a-z0-9][a-z0-9_-]*");
        }
        validate_credential_env_names(&self.url_env, &self.token_env)?;
        if self
            .spool_dir
            .as_ref()
            .is_some_and(|dir| dir.as_os_str().is_empty())
        {
            bail!("reporting spool_dir must not be empty");
        }
        Ok(())
    }
}

/// Rules for the credential variable names, shared by config validation and
/// by replay, which reads the names back from a saved manifest.
pub fn validate_credential_env_names(url_env: &str, token_env: &str) -> Result<()> {
    for (field, name) in [("url_env", url_env), ("token_env", token_env)] {
        if !valid_env_name(name) {
            bail!("reporting {field} must be an environment variable name matching [A-Za-z_][A-Za-z0-9_]*");
        }
        if RESERVED_ENV.contains(&name)
            || PARTITION_ENV.contains(&name)
            || name == ENVIRONMENT_ENV
            || name == OUTPUT_ENV
        {
            bail!("reporting {field} must not name a reserved goodflows variable");
        }
    }
    if url_env == token_env {
        bail!("reporting url_env and token_env must name different variables");
    }
    Ok(())
}

/// The server's workflow/job/environment name rule: 1-64 UTF-8 bytes, no control characters.
pub fn valid_report_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= crate::reporting::contract::MAX_NAME_BYTES
        && !name.chars().any(char::is_control)
}

pub fn valid_project_slug(slug: &str) -> bool {
    let bytes = slug.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 64
        && (bytes[0].is_ascii_lowercase() || bytes[0].is_ascii_digit())
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}

pub fn valid_env_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
        && bytes.all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub parallelism: Option<usize>,
    #[serde(default = "default_true")]
    pub fail_fast: bool,
    pub workdir: Option<PathBuf>,
    /// Command environment defaults, layered between the caller's environment and job env.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// Write one run history file per run. Off by default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub history: bool,
    /// Resolved against the config file's directory by `Config::load`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_bytes: Option<u64>,
    /// Project-wide Sprite name prefix; only valid in the top-level `[settings]`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sprite_prefix: Option<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallelism: None,
            fail_fast: true,
            workdir: None,
            env: BTreeMap::new(),
            history: false,
            history_dir: None,
            log_runs: None,
            history_runs: None,
            history_bytes: None,
            sprite_prefix: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EffectiveSettings {
    pub parallelism: usize,
    pub fail_fast: bool,
    pub workdir: PathBuf,
    /// Merged configured env defaults (shared < workflow < selected profile).
    /// Never includes the caller's environment or job/step overrides.
    pub env: BTreeMap<String, String>,
    pub history: bool,
    pub history_dir: PathBuf,
    pub log_runs: u64,
    pub history_runs: u64,
    pub history_bytes: u64,
}

/// Run history and log retention flags. `history_dir` is already resolved
/// against the invocation directory.
#[derive(Debug, Default, Clone)]
pub struct HistoryOverrides {
    /// `Some(false)` for `--no-history`, `Some(true)` for `--history` or `--history-dir`.
    pub history: Option<bool>,
    pub history_dir: Option<PathBuf>,
    pub log_runs: Option<u64>,
    pub history_runs: Option<u64>,
    pub history_bytes: Option<u64>,
}

impl Settings {
    pub fn resolve(
        &self,
        parallelism: Option<usize>,
        fail_fast: Option<bool>,
        cli: &HistoryOverrides,
    ) -> Result<EffectiveSettings> {
        Ok(EffectiveSettings {
            parallelism: match parallelism.or(self.parallelism) {
                Some(n) if n > 0 => n,
                _ => std::thread::available_parallelism()
                    .map(usize::from)
                    .unwrap_or(1),
            },
            fail_fast: fail_fast.unwrap_or(self.fail_fast),
            workdir: self
                .workdir
                .clone()
                .context("config workdir has not been resolved")?,
            env: self.env.clone(),
            history: cli.history.unwrap_or(self.history),
            history_dir: cli
                .history_dir
                .clone()
                .or_else(|| self.history_dir.clone())
                .context("config history_dir has not been resolved")?,
            log_runs: cli.log_runs.or(self.log_runs).unwrap_or(DEFAULT_LOG_RUNS),
            history_runs: cli
                .history_runs
                .or(self.history_runs)
                .unwrap_or(DEFAULT_HISTORY_RUNS),
            history_bytes: cli
                .history_bytes
                .or(self.history_bytes)
                .unwrap_or(DEFAULT_HISTORY_BYTES),
        })
    }

    /// Reject a zero retention cap in shared settings.
    fn validate_history(&self) -> Result<()> {
        validate_caps(self.log_runs, self.history_runs, self.history_bytes)
    }
}

fn validate_caps(
    log_runs: Option<u64>,
    history_runs: Option<u64>,
    history_bytes: Option<u64>,
) -> Result<()> {
    for (name, value) in [
        ("log_runs", log_runs),
        ("history_runs", history_runs),
        ("history_bytes", history_bytes),
    ] {
        if value == Some(0) {
            bail!("{name} must be at least 1");
        }
    }
    Ok(())
}

/// Join a configured history directory to the config file's directory.
fn resolve_history_dir(base: &Path, dir: &Path) -> Result<PathBuf> {
    if dir.as_os_str().is_empty() {
        bail!("history_dir must not be empty");
    }
    Ok(base.join(dir))
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub name: String,
    pub command: Option<String>,
    /// Workflow in the same config that this job invokes, instead of a
    /// command or steps. Omitted for command and step jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow: Option<String>,
    pub condition: Option<Condition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: BTreeMap<String, String>,
    #[serde(default)]
    pub depends_on: Vec<String>,
    pub timeout_secs: Option<u64>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    #[serde(default = "default_true")]
    pub shell: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reports: Vec<String>,
    /// `partitions` as written. `Config::validate` range-checks it and stores the
    /// result in `partitions`, so every out-of-range count gets one message naming the job.
    /// `Config::workflow` sets its count to a selected profile's count on the resolved copy.
    #[serde(rename(deserialize = "partitions"), default, skip_serializing)]
    pub(crate) partitions_input: Option<PartitionsInput>,
    /// Validated partitioning, set by `Config::validate`; `None` until then and for
    /// unpartitioned jobs. Serialized under the `partitions` key.
    #[serde(skip_deserializing, skip_serializing_if = "Option::is_none")]
    pub partitions: Option<Partitions>,
}

impl Job {
    /// Identity of each scheduled instance in index order, or a single `None`
    /// for an unpartitioned job.
    pub fn partition_ids(&self) -> impl Iterator<Item = Option<Partition>> {
        let count = self.partitions.map(|p| p.count.get());
        (1..=count.unwrap_or(1)).map(move |index| count.map(|count| Partition { index, count }))
    }

    /// Number of scheduled instances this job expands into.
    pub fn instances(&self) -> usize {
        self.partition_ids().count()
    }
}

/// Identity of one instance of a partitioned job. Serialized only into events
/// and results; saved manifests use `reporting::ManifestPartition`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
pub struct Partition {
    pub index: u32,
    pub count: u32,
}

impl Partition {
    /// Add this instance's identity to an event; unpartitioned events keep their shape.
    pub(crate) fn tag(partition: Option<Self>, mut event: serde_json::Value) -> serde_json::Value {
        if let Some(partition) = partition {
            event["partition"] = serde_json::json!(partition);
        }
        event
    }
}

/// How a partitioned job's work is divided. Only `index` exists today:
/// goodflows supplies partition identity and the tool splits the work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Strategy {
    Index,
}

/// Validated `partitions` setting, always serialized in the normalized table form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Partitions {
    pub count: NonZeroU32,
    pub strategy: Strategy,
}

/// `partitions` as parsed. Accepts `partitions = N` or
/// `partitions = { count = N, strategy = "index" }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PartitionsInput {
    /// The configured count as written; `validate_partitions` enforces the range
    /// so every out-of-range value gets one message naming the job.
    pub count: i64,
    pub strategy: Strategy,
}

impl<'de> Deserialize<'de> for PartitionsInput {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        // A hand-written visitor keeps specific messages; `#[serde(untagged)]`
        // would replace them with "did not match any variant".
        struct Visitor;

        impl<'de> serde::de::Visitor<'de> for Visitor {
            type Value = PartitionsInput;

            fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
                f.write_str("a partition count integer or a { count, strategy } table")
            }

            fn visit_i64<E: serde::de::Error>(self, value: i64) -> Result<PartitionsInput, E> {
                Ok(PartitionsInput {
                    count: value,
                    strategy: Strategy::Index,
                })
            }

            fn visit_u64<E: serde::de::Error>(self, value: u64) -> Result<PartitionsInput, E> {
                // TOML integers are i64; anything larger is out of range anyway.
                self.visit_i64(i64::try_from(value).unwrap_or(i64::MAX))
            }

            fn visit_map<A: serde::de::MapAccess<'de>>(
                self,
                mut map: A,
            ) -> Result<PartitionsInput, A::Error> {
                use serde::de::Error;
                let mut value = None;
                let mut strategy = None;
                while let Some(key) = map.next_key::<String>()? {
                    match key.as_str() {
                        "count" if value.is_some() => {
                            return Err(A::Error::duplicate_field("count"))
                        }
                        "count" => {
                            let raw: i64 = map.next_value().map_err(|_| {
                                A::Error::custom("partitions count must be an integer")
                            })?;
                            value = Some(raw);
                        }
                        "strategy" if strategy.is_some() => {
                            return Err(A::Error::duplicate_field("strategy"))
                        }
                        "strategy" => {
                            let raw: String = map.next_value().map_err(|_| {
                                A::Error::custom("partitions strategy must be a string")
                            })?;
                            if raw != "index" {
                                return Err(A::Error::custom(format!(
                                    "partitions strategy '{}' is not supported (supported: index)",
                                    raw.escape_debug()
                                )));
                            }
                            strategy = Some(Strategy::Index);
                        }
                        other => {
                            return Err(A::Error::custom(format!(
                                "partitions key '{}' is not supported (expected count, strategy)",
                                other.escape_debug()
                            )))
                        }
                    }
                }
                Ok(PartitionsInput {
                    count: value.ok_or_else(|| A::Error::missing_field("count"))?,
                    strategy: strategy.unwrap_or(Strategy::Index),
                })
            }
        }

        deserializer.deserialize_any(Visitor)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(untagged, deny_unknown_fields)]
pub enum Condition {
    LegacyShell(String),
    Expression { expression: String },
    Shell { shell: String },
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Step {
    pub id: String,
    pub command: String,
    pub condition: Option<Condition>,
    #[serde(default)]
    pub env: HashMap<String, String>,
    pub shell: Option<bool>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Workflow {
    #[serde(default, skip_serializing_if = "SettingsOverrides::is_empty")]
    pub settings: SettingsOverrides,
    #[serde(default)]
    pub jobs: Vec<Job>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Environment {
    #[serde(default, skip_serializing_if = "SettingsOverrides::is_empty")]
    pub settings: SettingsOverrides,
    /// Partition count per job name, replacing the declared count of a
    /// partitioned job when this profile is selected. Kept as `i64` until
    /// `Config::validate` so every out-of-range count gets one message.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub partitions: BTreeMap<String, i64>,
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SettingsOverrides {
    pub parallelism: Option<usize>,
    pub fail_fast: Option<bool>,
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// An explicit `false` wins over a lower `true`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_dir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_runs: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub history_bytes: Option<u64>,
    // Present only to reject a workflow/profile prefix override with a clear message.
    #[serde(default, skip_serializing)]
    sprite_prefix: Option<toml::Value>,
}

impl SettingsOverrides {
    /// Reject a zero retention cap in workflow or profile settings.
    fn validate_history(&self) -> Result<()> {
        if self.sprite_prefix.is_some() {
            bail!("sprite_prefix is project-wide; set it only in the top-level [settings]");
        }
        validate_caps(self.log_runs, self.history_runs, self.history_bytes)
    }

    fn is_empty(&self) -> bool {
        self.parallelism.is_none()
            && self.fail_fast.is_none()
            && self.workdir.is_none()
            && self.env.is_empty()
            && self.history.is_none()
            && self.history_dir.is_none()
            && self.log_runs.is_none()
            && self.history_runs.is_none()
            && self.history_bytes.is_none()
    }

    fn apply(&self, shared: &Settings) -> Settings {
        // Per-key merge: inherited keys survive unless overridden; an empty
        // string is a real override.
        let mut env = shared.env.clone();
        env.extend(self.env.clone());
        Settings {
            parallelism: self.parallelism.or(shared.parallelism),
            fail_fast: self.fail_fast.unwrap_or(shared.fail_fast),
            workdir: self.workdir.clone().or_else(|| shared.workdir.clone()),
            env,
            history: self.history.unwrap_or(shared.history),
            history_dir: self
                .history_dir
                .clone()
                .or_else(|| shared.history_dir.clone()),
            log_runs: self.log_runs.or(shared.log_runs),
            history_runs: self.history_runs.or(shared.history_runs),
            history_bytes: self.history_bytes.or(shared.history_bytes),
            sprite_prefix: shared.sprite_prefix.clone(),
        }
    }
}

pub fn validate_workflow_name(name: &str) -> Result<()> {
    validate_name(name, "workflow")
}

fn validate_name(name: &str, kind: &str) -> Result<()> {
    if name.trim().is_empty() || name.len() > 64 || name.chars().any(char::is_control) {
        bail!("{kind} name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters");
    }
    Ok(())
}

/// Reversibly encode a name as one path component. Used for workflow log
/// directories and reporting spool directories.
pub fn path_component(name: &str) -> String {
    // Encode uppercase too so distinct names never alias on macOS's commonly
    // case-insensitive filesystem. Percent itself is always encoded.
    name.bytes()
        .map(|b| {
            if b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-') {
                char::from(b).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

fn resolve_workdir(base: &Path, path: Option<&Path>) -> Result<PathBuf> {
    let path = base.join(path.unwrap_or(Path::new(".")));
    let resolved = path
        .canonicalize()
        .with_context(|| format!("workdir does not exist: {}", path.display()))?;
    if !resolved.is_dir() {
        bail!("workdir is not a directory: {}", resolved.display());
    }
    Ok(resolved)
}

fn default_true() -> bool {
    true
}

impl Config {
    pub fn load(explicit: Option<&Path>) -> Result<(Self, PathBuf)> {
        let path = match explicit {
            Some(path) => path.to_path_buf(),
            None => ["goodflows.toml", ".goodflows.toml"]
                .into_iter()
                .map(PathBuf::from)
                .find(|p| p.exists())
                .ok_or_else(|| {
                    ConfigError::new(
                        "missing_config",
                        "no config found (looked for goodflows.toml and .goodflows.toml)",
                    )
                    .with_hint(Some(
                        "create one with `goodflows init`, or pass --config PATH".into(),
                    ))
                })?,
        };
        let unreadable = |path: &Path| {
            ConfigError::new(
                "missing_config",
                format!("failed to read {}", path.display()),
            )
        };
        let path = path.canonicalize().with_context(|| unreadable(&path))?;
        let content = std::fs::read_to_string(&path).with_context(|| unreadable(&path))?;
        let mut config: Self = toml::from_str(&content).with_context(|| {
            ConfigError::new(
                "config_parse",
                format!("failed to parse {}", path.display()),
            )
        })?;
        let base = path.parent().context("config has no parent directory")?;
        // Path problems join the validation problems, so one pass reports all.
        let mut problems = Vec::new();
        let shared_workdir = config.settings.workdir.clone();
        if !config.jobs.is_empty() {
            config.settings.workdir = note(
                &mut problems,
                resolve_workdir(base, shared_workdir.as_deref()),
            );
        }
        for (name, workflow) in &mut config.workflows {
            workflow.settings.workdir = note(
                &mut problems,
                resolve_workdir(
                    base,
                    workflow
                        .settings
                        .workdir
                        .as_deref()
                        .or(shared_workdir.as_deref()),
                )
                .with_context(|| format!("workflow '{name}'")),
            );
        }
        for (name, environment) in &mut config.environments {
            if let Some(workdir) = &environment.settings.workdir {
                environment.settings.workdir = note(
                    &mut problems,
                    resolve_workdir(base, Some(workdir))
                        .with_context(|| format!("environment '{name}'")),
                );
            }
        }
        // History directories resolve like workdir: from the config file's directory.
        config.settings.history_dir = note(
            &mut problems,
            resolve_history_dir(
                base,
                config
                    .settings
                    .history_dir
                    .as_deref()
                    .unwrap_or(Path::new(DEFAULT_HISTORY_DIR)),
            )
            .context("settings"),
        );
        for (name, workflow) in &mut config.workflows {
            if let Some(dir) = &workflow.settings.history_dir {
                workflow.settings.history_dir = note(
                    &mut problems,
                    resolve_history_dir(base, dir).with_context(|| format!("workflow '{name}'")),
                );
            }
        }
        for (name, environment) in &mut config.environments {
            if let Some(dir) = &environment.settings.history_dir {
                environment.settings.history_dir = note(
                    &mut problems,
                    resolve_history_dir(base, dir).with_context(|| format!("environment '{name}'")),
                );
            }
        }
        if let Some(reporting) = &mut config.reporting {
            // Joining an empty path would yield `base` and hide the empty value from validate().
            if reporting
                .spool_dir
                .as_ref()
                .is_none_or(|dir| !dir.as_os_str().is_empty())
            {
                reporting.spool_dir = Some(base.join(reporting.spool_dir()));
            }
        }
        problems.extend(config.check());
        ConfigError::check(problems)?;
        Ok((config, path))
    }

    /// Validate every job and store the validated partition settings.
    /// Reports every problem in one `config_invalid` error.
    pub fn validate(&mut self) -> Result<()> {
        ConfigError::check(self.check())
    }

    /// Validate every job, store the validated partition settings, and
    /// return every problem found.
    fn check(&mut self) -> Vec<anyhow::Error> {
        let mut problems = Vec::new();
        if self.jobs.is_empty() && self.workflows.is_empty() {
            problems.push(anyhow!("no jobs defined"));
        }
        if let Some(reporting) = &self.reporting {
            note(&mut problems, reporting.validate().context("reporting"));
        }
        if let Some(prefix) = &self.settings.sprite_prefix {
            if !valid_sprite_prefix(prefix) {
                problems.push(anyhow!("settings sprite_prefix must be 1-20 bytes of lowercase ASCII letters, digits or hyphens, starting with a letter or digit"));
            }
        }
        if let Some(sprite) = &self.sprite {
            note(&mut problems, sprite.validate().context("sprite"));
        }
        let reporting = self.reporting.as_ref();
        // Validate every authored settings map, including unselected workflows and
        // profiles and entries a later scope overrides.
        note(
            &mut problems,
            validate_settings_env(&self.settings.env, reporting).context("settings env"),
        );
        note(
            &mut problems,
            self.settings.validate_history().context("settings"),
        );
        for (name, workflow) in &self.workflows {
            note(
                &mut problems,
                validate_settings_env(&workflow.settings.env, reporting)
                    .with_context(|| format!("workflow '{name}' settings env")),
            );
            note(
                &mut problems,
                workflow
                    .settings
                    .validate_history()
                    .with_context(|| format!("workflow '{name}' settings")),
            );
        }
        for (name, environment) in &self.environments {
            note(
                &mut problems,
                validate_settings_env(&environment.settings.env, reporting)
                    .with_context(|| format!("environment '{name}' settings env")),
            );
            note(
                &mut problems,
                environment
                    .settings
                    .validate_history()
                    .with_context(|| format!("environment '{name}' settings")),
            );
        }
        if !self.jobs.is_empty() {
            if self.workflows.contains_key("default") {
                problems.push(anyhow!("root jobs conflict with named workflow 'default'"));
            }
            let (partitions, job_problems) = validate_jobs(&self.jobs, reporting);
            problems.extend(job_problems);
            store_partitions(&mut self.jobs, partitions);
        }
        for (name, workflow) in &mut self.workflows {
            note(&mut problems, validate_workflow_name(name));
            let (partitions, job_problems) = validate_jobs(&workflow.jobs, reporting);
            problems.extend(
                job_problems
                    .into_iter()
                    .map(|problem| problem.context(format!("workflow '{name}'"))),
            );
            store_partitions(&mut workflow.jobs, partitions);
        }
        problems.extend(self.validate_calls());
        for name in self.environments.keys() {
            note(&mut problems, validate_name(name, "environment"));
        }
        // Profile partition entries are checked for every profile, selected or
        // not, against jobs that declare `partitions` in any workflow.
        let all_jobs = || {
            self.jobs
                .iter()
                .chain(self.workflows.values().flat_map(|workflow| &workflow.jobs))
        };
        let defined: HashSet<&str> = all_jobs().map(|job| job.name.as_str()).collect();
        let partitioned: BTreeSet<&str> = all_jobs()
            .filter(|job| job.partitions.is_some())
            .map(|job| job.name.as_str())
            .collect();
        for (profile, environment) in &self.environments {
            for (job, &count) in &environment.partitions {
                note(
                    &mut problems,
                    partition_count(count)
                        .with_context(|| format!("environment '{profile}': job '{job}'")),
                );
                if !defined.contains(job.as_str()) {
                    let available = if partitioned.is_empty() {
                        "none".to_owned()
                    } else {
                        partitioned.iter().copied().collect::<Vec<_>>().join(", ")
                    };
                    problems.push(anyhow!(
                        "environment '{profile}': unknown job '{job}' (partitioned jobs: {available})"
                    ));
                } else if !partitioned.contains(job.as_str()) {
                    problems.push(anyhow!(
                        "environment '{profile}': job '{job}' is not partitioned"
                    ));
                }
            }
        }
        problems
    }

    /// Every workflow's authored jobs by name, including root jobs as `default`.
    fn workflow_jobs(&self) -> BTreeMap<&str, &[Job]> {
        let mut workflows: BTreeMap<&str, &[Job]> = self
            .workflows
            .iter()
            .map(|(name, workflow)| (name.as_str(), workflow.jobs.as_slice()))
            .collect();
        if !self.jobs.is_empty() {
            workflows.insert("default", &self.jobs);
        }
        workflows
    }

    /// Check every workflow-call target and reject direct and indirect call
    /// cycles across the whole document, whether or not a call is selected or
    /// its condition would pass.
    fn validate_calls(&self) -> Vec<anyhow::Error> {
        let mut problems = Vec::new();
        let workflows = self.workflow_jobs();
        let names: Vec<&str> = workflows.keys().copied().collect();
        let index: HashMap<&str, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
        // Each workflow's call edges in job order: (target index, call job name).
        let mut edges: Vec<Vec<(usize, &str)>> = vec![Vec::new(); names.len()];
        for (from, jobs) in workflows.values().enumerate() {
            for job in jobs.iter() {
                // `validate_call` already reports an empty target.
                let Some(target) = job.workflow.as_deref().filter(|t| !t.is_empty()) else {
                    continue;
                };
                let Some(&to) = index.get(target) else {
                    let mut problem = format!(
                        "workflow '{}': job '{}': unknown workflow '{}' (available: {})",
                        names[from],
                        job.name,
                        target.escape_debug(),
                        self.workflow_names().join(", ")
                    );
                    if let Some(hint) = did_you_mean(target, names.iter().copied()) {
                        problem = format!("{problem}; {hint}");
                    }
                    problems.push(anyhow!(problem));
                    continue;
                };
                edges[from].push((to, &job.name));
            }
        }
        // Iterative DFS, like `cycle_path`, so deep call chains need no depth limit.
        let mut state = vec![0u8; names.len()];
        for root in 0..names.len() {
            if state[root] != 0 {
                continue;
            }
            let mut stack = vec![(root, 0usize)];
            state[root] = 1;
            while let Some(&(node, next)) = stack.last() {
                if next == edges[node].len() {
                    state[node] = 2;
                    stack.pop();
                    continue;
                }
                stack.last_mut().expect("stack is not empty").1 += 1;
                let (to, _) = edges[node][next];
                if state[to] == 1 {
                    let start = stack
                        .iter()
                        .position(|&(index, _)| index == to)
                        .expect("active workflow is on stack");
                    let mut path: Vec<_> = stack[start..]
                        .iter()
                        .map(|&(index, next)| {
                            format!(
                                "workflow '{}' job '{}'",
                                names[index],
                                edges[index][next - 1].1
                            )
                        })
                        .collect();
                    path.push(format!("workflow '{}'", names[to]));
                    problems.push(anyhow!(
                        "workflow call cycle detected: {}",
                        path.join(" -> ")
                    ));
                    return problems;
                }
                if state[to] == 0 {
                    state[to] = 1;
                    stack.push((to, 0));
                }
            }
        }
        problems
    }

    pub fn environment(&self, requested: Option<&str>) -> Result<Option<&Environment>> {
        requested
            .map(|name| {
                self.environments.get(name).ok_or_else(|| {
                    let names = self.environments.keys().map(String::as_str);
                    let available = names.clone().collect::<Vec<_>>().join(", ");
                    ConfigError::new(
                        "unknown_environment",
                        format!("unknown environment '{name}' (available: {available})"),
                    )
                    .with_hint(did_you_mean(name, names))
                    .into()
                })
            })
            .transpose()
    }

    pub fn workflow_names(&self) -> Vec<String> {
        let mut names: Vec<_> = self.workflows.keys().cloned().collect();
        if !self.jobs.is_empty() {
            names.push("default".into());
        }
        names.sort();
        names
    }

    pub fn default_workflow(&self) -> Option<&str> {
        if !self.jobs.is_empty() || self.workflows.contains_key("default") {
            Some("default")
        } else {
            None
        }
    }

    /// Resolve a workflow, applying the selected environment profile's settings
    /// and partition counts to the returned copy. The document is not modified.
    pub fn workflow(
        &self,
        requested: Option<&str>,
        profile: Option<&str>,
    ) -> Result<(String, Config)> {
        let environment = self.environment(profile)?;
        let names = self.workflow_names();
        let available = names.join(", ");
        let name = requested.or_else(|| self.default_workflow()).ok_or_else(|| {
            ConfigError::new("no_default_workflow", format!("no default workflow; choose a workflow with 'goodflows run <workflow>' (available: {available})"))
        })?;
        let (settings, mut jobs) = if name == "default" && !self.jobs.is_empty() {
            (self.settings.clone(), self.jobs.clone())
        } else {
            let workflow = self.workflows.get(name).ok_or_else(|| {
                ConfigError::new(
                    "unknown_workflow",
                    format!("unknown workflow '{name}' (available: {available})"),
                )
                .with_hint(did_you_mean(name, names.iter().map(String::as_str)))
            })?;
            (
                workflow.settings.apply(&self.settings),
                workflow.jobs.clone(),
            )
        };
        // The environment exists exactly when a profile name was supplied.
        let selected = profile.zip(environment);
        if let Some((profile, environment)) = selected {
            for (job_name, &count) in &environment.partitions {
                // A name absent from this workflow is unused here.
                let Some(job) = jobs.iter_mut().find(|job| &job.name == job_name) else {
                    continue;
                };
                let Some(partitions) = &mut job.partitions else {
                    let workflow = if name == "default" && !self.jobs.is_empty() {
                        "workflow 'default' (root jobs)".to_owned()
                    } else {
                        format!("workflow '{name}'")
                    };
                    return Err(ConfigError::invalid(vec![format!(
                        "environment '{profile}': {workflow}: job '{job_name}' is not partitioned"
                    )]));
                };
                // Defensive: `Config::validate` already rejected out-of-range
                // counts, so this conversion cannot fail for a validated config.
                partitions.count = partition_count(count)
                    .with_context(|| format!("environment '{profile}': job '{job_name}'"))?;
                // Keep the parsed input consistent with the resolved count.
                if let Some(input) = &mut job.partitions_input {
                    input.count = count;
                }
            }
        }
        Ok((
            name.to_owned(),
            Config {
                settings: match selected {
                    Some((_, environment)) => environment.settings.apply(&settings),
                    None => settings,
                },
                jobs,
                workflows: BTreeMap::new(),
                environments: BTreeMap::new(),
                reporting: self.reporting.clone(),
                sprite: self.sprite.clone(),
            },
        ))
    }

    /// Every workflow reachable through calls from `jobs`, each resolved with
    /// its complete job list under the selected profile. Resolving checks the
    /// profile against each reachable workflow, whether or not a call's
    /// condition would pass. Validation already rejected call cycles.
    pub fn called_workflows(
        &self,
        jobs: &[Job],
        profile: Option<&str>,
    ) -> Result<BTreeMap<String, Config>> {
        let mut targets = BTreeMap::new();
        let mut pending: Vec<String> = jobs.iter().filter_map(|j| j.workflow.clone()).collect();
        while let Some(name) = pending.pop() {
            if targets.contains_key(&name) {
                continue;
            }
            let (_, config) = self.workflow(Some(&name), profile)?;
            pending.extend(config.jobs.iter().filter_map(|j| j.workflow.clone()));
            targets.insert(name, config);
        }
        Ok(targets)
    }

    pub fn select(&mut self, selected: &[String]) -> Result<()> {
        if selected.is_empty() {
            return Ok(());
        }
        let names: HashMap<_, _> = self
            .jobs
            .iter()
            .enumerate()
            .map(|(i, j)| (j.name.as_str(), i))
            .collect();
        let mut wanted = HashSet::new();
        let mut stack = Vec::new();
        for name in selected {
            let Some(&index) = names.get(name.as_str()) else {
                let jobs = self.jobs.iter().map(|job| job.name.as_str());
                let available = jobs.clone().collect::<Vec<_>>().join(", ");
                return Err(ConfigError::new(
                    "unknown_job",
                    format!("unknown job '{name}' (available: {available})"),
                )
                .with_hint(did_you_mean(name, jobs))
                .into());
            };
            stack.push(index);
        }
        while let Some(index) = stack.pop() {
            if wanted.insert(index) {
                for dep in &self.jobs[index].depends_on {
                    stack.push(names[dep.as_str()]);
                }
            }
        }
        let mut index = 0;
        self.jobs.retain(|_| {
            let keep = wanted.contains(&index);
            index += 1;
            keep
        });
        Ok(())
    }
}

/// Range-check a raw partition count; callers add the job or profile context.
fn partition_count(count: i64) -> Result<NonZeroU32> {
    u32::try_from(count)
        .ok()
        .filter(|count| *count <= MAX_PARTITIONS)
        .and_then(NonZeroU32::new)
        .with_context(|| {
            format!("partitions count must be from 1 to {MAX_PARTITIONS}, got {count}")
        })
}

fn store_partitions(jobs: &mut [Job], partitions: Vec<Option<Partitions>>) {
    for (job, partitions) in jobs.iter_mut().zip(partitions) {
        job.partitions = partitions;
    }
}

/// Returns each job's validated partition setting, in job order, and every
/// problem found in the jobs.
fn validate_jobs(
    jobs: &[Job],
    reporting: Option<&Reporting>,
) -> (Vec<Option<Partitions>>, Vec<anyhow::Error>) {
    let mut problems = Vec::new();
    if jobs.is_empty() {
        problems.push(anyhow!("no jobs defined"));
        return (Vec::new(), problems);
    }
    let no_outputs: NoOutputs = jobs
        .iter()
        .filter_map(|job| {
            let kind = if job.workflow.is_some() {
                "workflow-call"
            } else if job.partitions_input.is_some() {
                "partitioned"
            } else {
                return None;
            };
            Some((job.name.as_str(), kind))
        })
        .collect();
    let mut names = HashMap::new();
    let mut partitions = Vec::with_capacity(jobs.len());
    // A duplicate name or a bad dependency leaves no graph to check for cycles.
    let mut graph = true;
    for (index, job) in jobs.iter().enumerate() {
        if job.name.trim().is_empty() {
            problems.push(anyhow!("empty job name"));
        }
        if names.insert(job.name.as_str(), index).is_some() {
            problems.push(anyhow!("duplicate job name '{}'", job.name));
            graph = false;
        }
        let (job_partitions, job_problems) = validate_job(job, reporting, &no_outputs);
        partitions.push(job_partitions);
        problems.extend(
            job_problems
                .into_iter()
                .map(|problem| problem.context(format!("job '{}'", job.name))),
        );
        if job.timeout_secs == Some(0) {
            problems.push(anyhow!(
                "job '{}' timeout_secs must be greater than zero",
                job.name
            ));
        }
        // Names appear in text output and rerun hints, so no control characters
        // (including escape sequences). Other name rules are unchanged.
        if job.name.chars().any(char::is_control) {
            problems.push(anyhow!(
                "job '{}' contains invalid control characters",
                job.name.escape_debug()
            ));
        }
    }
    let mut indegrees = vec![0; jobs.len()];
    let mut dependents = vec![Vec::new(); jobs.len()];
    for (index, job) in jobs.iter().enumerate() {
        let mut seen = HashSet::new();
        for dep in &job.depends_on {
            if dep == &job.name {
                problems.push(anyhow!("job '{}' depends on itself", job.name));
                graph = false;
                continue;
            }
            if !seen.insert(dep) {
                problems.push(anyhow!(
                    "job '{}' has duplicate dependency '{}'",
                    job.name,
                    dep
                ));
                continue;
            }
            let Some(&dep_index) = names.get(dep.as_str()) else {
                let mut problem = format!("job '{}' depends on unknown job '{}'", job.name, dep);
                if let Some(hint) = did_you_mean(dep, names.keys().copied()) {
                    problem = format!("{problem}; {hint}");
                }
                problems.push(anyhow!(problem));
                graph = false;
                continue;
            };
            indegrees[index] += 1;
            dependents[dep_index].push(index);
        }
    }
    if !graph {
        return (partitions, problems);
    }
    let mut ready: VecDeque<_> = indegrees
        .iter()
        .enumerate()
        .filter_map(|(i, &count)| (count == 0).then_some(i))
        .collect();
    let mut visited = 0;
    while let Some(index) = ready.pop_front() {
        visited += 1;
        for &next in &dependents[index] {
            indegrees[next] -= 1;
            if indegrees[next] == 0 {
                ready.push_back(next);
            }
        }
    }
    if visited != jobs.len() {
        problems.push(anyhow!(
            "dependency cycle detected: {}",
            cycle_path(jobs, &names)
        ));
    }
    (partitions, problems)
}

fn validate_command(command: &str, shell: bool) -> Result<()> {
    if command.trim().is_empty() {
        bail!("empty command");
    }
    if command.contains('\0') {
        bail!("command contains invalid control characters");
    }
    if !shell {
        let args = shell_words::split(command).context("invalid command quoting")?;
        if args.first().is_none_or(|arg| arg.is_empty()) {
            bail!("empty command executable");
        }
    }
    Ok(())
}

fn validate_env<'a>(
    env: impl IntoIterator<Item = (&'a String, &'a String)>,
    reporting: Option<&Reporting>,
) -> Result<()> {
    for (key, value) in env {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            bail!("invalid environment variable");
        }
        if RESERVED_ENV.contains(&key.as_str())
            || PARTITION_ENV.contains(&key.as_str())
            || key == ENVIRONMENT_ENV
            || key == SPRITE_STATE_ENV
        {
            bail!("env must not set reserved variable {key}; goodflows sets it itself");
        }
        if reporting.is_some_and(|r| r.token_env == *key) {
            // The token is removed from every command, so an explicit value would be silently dropped.
            bail!("env must not set {key}: it is the reporting token_env, which goodflows removes from every command");
        }
    }
    Ok(())
}

/// Settings env additionally rejects `GOODFLOWS_OUTPUT`: the runner owns it for
/// every command, so a settings default could only leak into expressions. Job and
/// step env keep accepting it for compatibility with existing configs.
fn validate_settings_env(
    env: &BTreeMap<String, String>,
    reporting: Option<&Reporting>,
) -> Result<()> {
    if env.contains_key(OUTPUT_ENV) {
        bail!("env must not set reserved variable {OUTPUT_ENV}; goodflows sets it itself");
    }
    validate_env(env, reporting)
}

/// Jobs in one workflow that publish no job outputs, with their kind:
/// partitioned jobs and workflow-call jobs.
type NoOutputs<'a> = HashMap<&'a str, &'static str>;

fn validate_expression(
    source: &str,
    needs: &[String],
    steps: &[String],
    no_outputs: &NoOutputs,
) -> Result<()> {
    let expression = Expression::parse(source)?;
    for reference in expression.references() {
        match reference {
            Reference::Needs { job, .. } if !needs.contains(job) => {
                bail!("output reference requires declared dependency '{job}'");
            }
            Reference::Needs { job, .. } if no_outputs.contains_key(job.as_str()) => {
                bail!(
                    "{} job '{job}' publishes no outputs",
                    no_outputs[job.as_str()]
                );
            }
            Reference::Step { step, .. } if !steps.contains(step) => {
                bail!("output reference requires an earlier step '{step}'");
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_condition(
    condition: Option<&Condition>,
    needs: &[String],
    steps: &[String],
    no_outputs: &NoOutputs,
) -> Result<()> {
    match condition {
        Some(Condition::Expression { expression }) => {
            validate_expression(expression, needs, steps, no_outputs)
                .context("invalid condition expression")?;
        }
        Some(Condition::LegacyShell(shell) | Condition::Shell { shell })
            if shell.trim().is_empty() || shell.contains('\0') =>
        {
            bail!("condition must be nonempty and contain no NUL bytes");
        }
        _ => {}
    }
    Ok(())
}

fn validate_reports(job: &Job, reporting: bool) -> Result<()> {
    if job.reports.is_empty() {
        return Ok(());
    }
    if !reporting {
        bail!("reports requires a top-level [reporting] block");
    }
    let mut seen = HashSet::new();
    for format in &job.reports {
        if format == "exunit-timings" {
            bail!(
                "reports: format 'exunit-timings' was retired (supported: {REPORT_FAMILY}); \
                 use reports = [\"{REPORT_FAMILY}\"] and install the ExUnit reporter \
                 (goodflows_exunit_reporter, Goodflows.ExUnitReporter); \
                 see README.md, \"Upgrading from exunit-timings\" \
                 (https://github.com/agoodway/goodflows#upgrading-from-exunit-timings)"
            );
        }
        if format != REPORT_FAMILY {
            bail!(
                "reports: unsupported format '{}' (supported: {REPORT_FAMILY})",
                format.escape_debug()
            );
        }
        if !seen.insert(format) {
            bail!("reports: duplicate format '{format}'");
        }
    }
    if !valid_report_name(&job.name) {
        bail!(
            "reporting job name must be at most 64 UTF-8 bytes and contain no control characters"
        );
    }
    Ok(())
}

fn validate_partitions(job: &Job) -> Result<Option<Partitions>> {
    let Some(input) = job.partitions_input else {
        return Ok(None);
    };
    let count = partition_count(input.count)?;
    if !job.outputs.is_empty() {
        bail!("partitioned jobs publish no job outputs; remove the outputs mapping (step outputs remain available within each instance)");
    }
    Ok(Some(Partitions {
        count,
        strategy: input.strategy,
    }))
}

/// Returns the job's validated partition setting and every problem found
/// in the job.
fn validate_job(
    job: &Job,
    reporting: Option<&Reporting>,
    no_outputs: &NoOutputs,
) -> (Option<Partitions>, Vec<anyhow::Error>) {
    let mut problems = Vec::new();
    let condition = validate_condition(job.condition.as_ref(), &job.depends_on, &[], no_outputs);
    if job.workflow.is_some() {
        note(&mut problems, validate_call(job));
        note(&mut problems, condition);
        return (None, problems);
    }
    let partitions = note(&mut problems, validate_partitions(job)).flatten();
    note(&mut problems, validate_reports(job, reporting.is_some()));
    match (&job.command, job.steps.is_empty()) {
        (Some(command), true) => {
            note(&mut problems, validate_command(command, job.shell));
            if !job.outputs.is_empty() {
                problems.push(anyhow!(
                    "command-only jobs publish outputs directly; output mappings require steps"
                ));
            }
        }
        (None, false) => {}
        _ => problems.push(anyhow!(
            "define either a command or nonempty steps, never both"
        )),
    }
    note(&mut problems, validate_env(&job.env, reporting));
    note(&mut problems, condition);
    let mut steps = Vec::new();
    for step in &job.steps {
        let duplicate = if steps.contains(&step.id) {
            Err(anyhow!("duplicate step id '{}'", step.id))
        } else {
            Ok(())
        };
        for check in [
            validate_name(&step.id, "step"),
            duplicate,
            validate_command(&step.command, step.shell.unwrap_or(job.shell)),
            validate_env(&step.env, reporting),
            validate_condition(step.condition.as_ref(), &job.depends_on, &steps, no_outputs),
        ] {
            note(
                &mut problems,
                check.with_context(|| format!("step '{}'", step.id)),
            );
        }
        steps.push(step.id.clone());
    }
    for (key, source) in &job.outputs {
        note(&mut problems, validate_name(key, "output"));
        note(
            &mut problems,
            validate_expression(source, &job.depends_on, &steps, no_outputs)
                .with_context(|| format!("output '{key}'")),
        );
    }
    (partitions, problems)
}

/// A workflow-call job has no other execution form and none of the
/// execution-only modifiers; defaults and empty collections are harmless.
/// Its target is checked against the whole document by `validate_calls`.
fn validate_call(job: &Job) -> Result<()> {
    if job.command.is_some() || !job.steps.is_empty() {
        bail!("define exactly one of a command, nonempty steps, or a workflow");
    }
    if job.workflow.as_deref() == Some("") {
        bail!("workflow-call target must be a nonempty workflow name");
    }
    for (set, field) in [
        (job.timeout_secs.is_some(), "timeout_secs"),
        (job.partitions_input.is_some(), "partitions"),
        (!job.env.is_empty(), "env"),
        (!job.outputs.is_empty(), "outputs"),
        (!job.reports.is_empty(), "reports"),
        (!job.shell, "shell = false"),
    ] {
        if set {
            bail!("workflow-call jobs do not support {field}");
        }
    }
    Ok(())
}

fn cycle_path(jobs: &[Job], names: &HashMap<&str, usize>) -> String {
    // Iterative DFS avoids stack overflow on deep graphs. Only the segment
    // between two visits to an active node is a cycle; downstream jobs aren't.
    let mut state = vec![0u8; jobs.len()];
    for root in 0..jobs.len() {
        if state[root] != 0 {
            continue;
        }
        let mut stack = vec![(root, 0usize)];
        state[root] = 1;
        while let Some(&(node, next)) = stack.last() {
            if next == jobs[node].depends_on.len() {
                state[node] = 2;
                stack.pop();
                continue;
            }
            stack.last_mut().expect("stack is not empty").1 += 1;
            let dep = names[jobs[node].depends_on[next].as_str()];
            if state[dep] == 1 {
                let start = stack
                    .iter()
                    .position(|&(index, _)| index == dep)
                    .expect("active node is on stack");
                let mut path: Vec<_> = stack[start..]
                    .iter()
                    .map(|&(index, _)| jobs[index].name.as_str())
                    .collect();
                path.push(&jobs[dep].name);
                return path.join(" -> ");
            }
            if state[dep] == 0 {
                state[dep] = 1;
                stack.push((dep, 0));
            }
        }
    }
    unreachable!("topological validation proved that a cycle exists")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(partitions: &str) -> std::result::Result<Job, String> {
        toml::from_str::<Job>(&format!(
            "name = 'test'\ncommand = 'true'\npartitions = {partitions}\n"
        ))
        .map_err(|error| error.to_string())
    }

    /// A parsed job with its partition setting validated, as `Config::validate` leaves it.
    fn validated(partitions: &str) -> Job {
        let mut job = job(partitions).unwrap();
        job.partitions = validate_partitions(&job).unwrap();
        job
    }

    fn index(count: u32) -> Option<Partitions> {
        Some(Partitions {
            count: NonZeroU32::new(count).unwrap(),
            strategy: Strategy::Index,
        })
    }

    #[test]
    fn partitions_accept_integer_and_table_forms() {
        let input = |count| {
            Some(PartitionsInput {
                count,
                strategy: Strategy::Index,
            })
        };
        assert_eq!(job("3").unwrap().partitions_input, input(3));
        // Parsing alone never yields a scheduled partition setting.
        assert_eq!(job("3").unwrap().partitions, None);
        assert_eq!(validated("3").partitions, index(3));
        assert_eq!(
            validated("{ count = 2, strategy = 'index' }").partitions,
            index(2)
        );
        assert_eq!(validated("{ count = 4 }").partitions, index(4));
        let unpartitioned: Job = toml::from_str("name = 'a'\ncommand = 'true'").unwrap();
        assert_eq!(unpartitioned.partitions_input, None);
        assert_eq!(validate_partitions(&unpartitioned).unwrap(), None);
        assert_eq!(unpartitioned.instances(), 1);
        assert_eq!(validated("5").instances(), 5);
    }

    #[test]
    fn config_validation_stores_validated_partitions_for_every_workflow() {
        let mut config: Config = toml::from_str(
            "[[workflows.ci.jobs]]\nname = 'test'\ncommand = 'true'\npartitions = 3\n",
        )
        .unwrap();
        config.validate().unwrap();
        assert_eq!(config.workflows["ci"].jobs[0].partitions, index(3));
    }

    #[test]
    fn partitions_serialize_in_normalized_table_form() {
        let value = serde_json::to_value(validated("3")).unwrap();
        assert_eq!(
            value["partitions"],
            serde_json::json!({"count": 3, "strategy": "index"})
        );
        let unpartitioned: Job = toml::from_str("name = 'a'\ncommand = 'true'").unwrap();
        let value = serde_json::to_value(unpartitioned).unwrap();
        assert!(value.get("partitions").is_none());
    }

    #[test]
    fn partitions_reject_unsupported_values_with_specific_messages() {
        for (value, message) in [
            (
                "{ count = 3, strategy = 'timings' }",
                "partitions strategy 'timings' is not supported (supported: index)",
            ),
            (
                "{ count = 3, files = ['a'] }",
                "partitions key 'files' is not supported",
            ),
            ("{ strategy = 'index' }", "missing field `count`"),
            ("{ count = 'three' }", "partitions count must be an integer"),
            ("{ count = 2.5 }", "partitions count must be an integer"),
            (
                "{ count = 2, strategy = 1 }",
                "partitions strategy must be a string",
            ),
            (
                "'three'",
                "expected a partition count integer or a { count, strategy } table",
            ),
            (
                "2.5",
                "expected a partition count integer or a { count, strategy } table",
            ),
            (
                "true",
                "expected a partition count integer or a { count, strategy } table",
            ),
        ] {
            let error = job(value).unwrap_err();
            assert!(error.contains(message), "{value}: {error}");
            assert!(
                !error.contains("did not match any variant"),
                "{value}: {error}"
            );
        }
    }

    #[test]
    fn partition_counts_are_validated_from_1_to_64() {
        for (count, valid) in [
            (i64::MIN, false),
            (-1, false),
            (0, false),
            (1, true),
            (64, true),
            (65, false),
            (4_294_967_296, false),
            (i64::MAX, false),
        ] {
            for form in [count.to_string(), format!("{{ count = {count} }}")] {
                let result = validate_partitions(&job(&form).unwrap());
                assert_eq!(result.is_ok(), valid, "{form}");
                if !valid {
                    assert_eq!(
                        result.unwrap_err().to_string(),
                        format!("partitions count must be from 1 to 64, got {count}")
                    );
                }
            }
        }
    }

    #[test]
    fn partition_ids_expand_in_index_order() {
        let ids: Vec<_> = validated("3").partition_ids().collect();
        let id = |index| Some(Partition { index, count: 3 });
        assert_eq!(ids, [id(1), id(2), id(3)]);
        let unpartitioned: Job = toml::from_str("name = 'a'\ncommand = 'true'").unwrap();
        assert_eq!(unpartitioned.partition_ids().collect::<Vec<_>>(), [None]);
    }

    fn parse(source: &str) -> std::result::Result<Config, String> {
        toml::from_str::<Config>(source).map_err(|error| error.to_string())
    }

    fn validate(source: &str) -> std::result::Result<Config, String> {
        let mut config = parse(source)?;
        config.validate().map_err(|error| format!("{error:#}"))?;
        Ok(config)
    }

    #[test]
    fn reports_name_the_test_timings_family() {
        assert_eq!(REPORT_FAMILY, "test-timings");
        assert_eq!(crate::reporting::contract::REPORT_FORMAT, "test-timings/1");
        let config = |reports: &str| {
            format!(
                "[reporting]\nproject = 'goodcases'\n[[jobs]]\nname = 'test'\ncommand = 'true'\nreports = {reports}\n"
            )
        };
        assert!(validate(&config("['test-timings']")).is_ok());
        let error = validate(&config("['exunit-timings']")).unwrap_err();
        assert_eq!(
            error,
            "job 'test': reports: format 'exunit-timings' was retired (supported: test-timings); \
             use reports = [\"test-timings\"] and install the ExUnit reporter \
             (goodflows_exunit_reporter, Goodflows.ExUnitReporter); \
             see README.md, \"Upgrading from exunit-timings\" \
             (https://github.com/agoodway/goodflows#upgrading-from-exunit-timings)"
        );
        let error = validate(&config("['test-timings/1']")).unwrap_err();
        assert!(
            error.contains("unsupported format") && error.contains("(supported: test-timings)"),
            "{error}"
        );
        let error = validate(&config("['test-timings', 'test-timings']")).unwrap_err();
        assert!(error.contains("duplicate format"), "{error}");
    }

    #[test]
    fn reporting_history_is_an_optional_boolean() {
        let config = |extra: &str| {
            format!("[reporting]\nproject = 'goodcases'\n{extra}[[jobs]]\nname = 'test'\ncommand = 'true'\n")
        };
        let history = |extra: &str| validate(&config(extra)).unwrap().reporting.unwrap().history;
        assert!(!history(""));
        assert!(!history("history = false\n"));
        assert!(history("history = true\n"));
        for value in ["'yes'", "'true'", "1", "[true]", "{ upload = true }"] {
            let error = validate(&config(&format!("history = {value}\n"))).unwrap_err();
            assert!(
                error.contains("reporting.history must be a boolean"),
                "{value}: {error}"
            );
        }
        // The upload setting never turns on the local history file.
        assert!(
            !validate(&config("history = true\n"))
                .unwrap()
                .settings
                .history
        );
    }

    #[test]
    fn job_names_reject_control_characters_only() {
        for escape in ["\\n", "\\r", "\\u0000", "\\t", "\\u001b", "\\u009b"] {
            let error = validate(&format!(
                "[[jobs]]\nname = \"a{escape}b\"\ncommand = 'true'\n"
            ))
            .unwrap_err();
            assert!(
                error.contains("contains invalid control characters"),
                "{error}"
            );
        }
        // Spaces, commas and long names stay valid.
        let long = "x".repeat(100);
        validate(&format!(
            "[[jobs]]\nname = 'a b,c'\ncommand = 'true'\n[[jobs]]\nname = '{long}'\ncommand = 'true'\n"
        ))
        .unwrap();
    }

    fn resolved_env(
        config: &Config,
        workflow: &str,
        profile: Option<&str>,
    ) -> BTreeMap<String, String> {
        let (_, mut selected) = config.workflow(Some(workflow), profile).unwrap();
        selected.settings.workdir = Some(PathBuf::from("/"));
        selected.settings.history_dir = Some(PathBuf::from("/history"));
        selected
            .settings
            .resolve(None, None, &HistoryOverrides::default())
            .unwrap()
            .env
    }

    fn map(entries: &[(&str, &str)]) -> BTreeMap<String, String> {
        entries
            .iter()
            .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
            .collect()
    }

    const JOB: &str = "[[workflows.ci.jobs]]\nname = 'a'\ncommand = 'true'\n[[workflows.other.jobs]]\nname = 'a'\ncommand = 'true'\n";

    #[test]
    fn settings_env_merges_per_key_and_keeps_empty_strings() {
        let config = validate(&format!(
            "{JOB}[settings.env]\nMIX_ENV = 'test'\nKEEP = 'shared'\nVALUE = 'shared'\n\
             [workflows.ci.settings.env]\nMIX_ENV = 'dev'\n\
             [workflows.other.settings.env]\n\
             [environments.ci.settings.env]\nMIX_ENV = 'prod'\nCI = 'true'\nVALUE = ''\nLITERAL = '$HOME ${{X}}'\n"
        ))
        .unwrap();
        assert_eq!(
            resolved_env(&config, "ci", Some("ci")),
            map(&[
                ("CI", "true"),
                ("KEEP", "shared"),
                ("LITERAL", "$HOME ${X}"),
                ("MIX_ENV", "prod"),
                ("VALUE", ""),
            ])
        );
        assert_eq!(
            resolved_env(&config, "ci", None),
            map(&[("KEEP", "shared"), ("MIX_ENV", "dev"), ("VALUE", "shared")])
        );
        // An empty workflow map inherits everything.
        assert_eq!(
            resolved_env(&config, "other", None),
            map(&[("KEEP", "shared"), ("MIX_ENV", "test"), ("VALUE", "shared")])
        );
    }

    #[test]
    fn absent_settings_env_resolves_to_empty_map() {
        let config = validate(JOB).unwrap();
        assert!(resolved_env(&config, "ci", None).is_empty());
        assert!(config.settings.env.is_empty());
    }

    #[test]
    fn env_only_overrides_are_not_empty() {
        let overrides: SettingsOverrides = toml::from_str("env = { A = '1' }").unwrap();
        assert!(!overrides.is_empty());
        assert!(SettingsOverrides::default().is_empty());
        let config = validate(&format!(
            "{JOB}[environments.local.settings.env]\nA = '1'\n"
        ))
        .unwrap();
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("[environments.local.settings.env]"), "{text}");
        let shared = toml::to_string(&Settings::default()).unwrap();
        assert!(!shared.contains("env"), "{shared}");
    }

    #[test]
    fn settings_env_rejects_non_string_values() {
        for scope in [
            "settings",
            "workflows.ci.settings",
            "environments.local.settings",
        ] {
            for value in ["1", "true", "['a']", "{ a = 'b' }"] {
                let error = parse(&format!("{JOB}[{scope}.env]\nVALUE = {value}\n")).unwrap_err();
                assert!(error.contains("invalid type"), "{scope} {value}: {error}");
            }
        }
    }

    #[test]
    fn settings_env_validates_every_scope_without_exposing_values() {
        let custom = "[reporting]\nproject = 'demo'\ntoken_env = 'PRIVATE_TOKEN'\n";
        let default = "[reporting]\nproject = 'demo'\n";
        for (scope, context) in [
            ("settings", "settings env"),
            ("workflows.ci.settings", "workflow 'ci' settings env"),
            ("workflows.other.settings", "workflow 'other' settings env"),
            (
                "environments.unused.settings",
                "environment 'unused' settings env",
            ),
        ] {
            for (entry, message) in [
                (
                    "GOODFLOWS_PARTITION = 'secret-value'",
                    "reserved variable GOODFLOWS_PARTITION",
                ),
                (
                    "GOODFLOWS_PARTITION_COUNT = 'secret-value'",
                    "reserved variable",
                ),
                ("GOODFLOWS_RUN_ID = 'secret-value'", "reserved variable"),
                ("GOODFLOWS_REPORT_DIR = 'secret-value'", "reserved variable"),
                (
                    "GOODFLOWS_ENVIRONMENT = 'secret-value'",
                    "reserved variable GOODFLOWS_ENVIRONMENT",
                ),
                (
                    "GOODFLOWS_OUTPUT = 'secret-value'",
                    "reserved variable GOODFLOWS_OUTPUT",
                ),
                ("PRIVATE_TOKEN = 'secret-value'", "reporting token_env"),
                ("'' = 'secret-value'", "invalid environment variable"),
                ("'A=B' = 'secret-value'", "invalid environment variable"),
                (
                    "A = \"secret-value\\u0000\"",
                    "invalid environment variable",
                ),
            ] {
                let error =
                    validate(&format!("{JOB}{custom}[{scope}.env]\n{entry}\n")).unwrap_err();
                assert!(error.contains(context), "{scope} {entry}: {error}");
                assert!(error.contains(message), "{scope} {entry}: {error}");
                assert!(!error.contains("secret-value"), "{scope} {entry}: {error}");
            }
            // The default token_env is rejected the same way.
            let entry = "GOODFLOWS_API_KEY = 'secret-value'";
            let error = validate(&format!("{JOB}{default}[{scope}.env]\n{entry}\n")).unwrap_err();
            assert!(error.contains(context), "{scope} {entry}: {error}");
            assert!(
                error.contains("GOODFLOWS_API_KEY: it is the reporting token_env"),
                "{scope} {entry}: {error}"
            );
            assert!(!error.contains("secret-value"), "{scope} {entry}: {error}");
        }
    }

    #[test]
    fn sprite_configuration_is_validated_strictly() {
        // Partial configurations stay valid for inspection.
        for partial in [
            "[settings]\nsprite_prefix = 'goodflows'\n",
            "[sprite]\nsetup = 'true'\n",
            "[sprite]\npass_env = ['A', '_B1']\n",
            "[settings]\nsprite_prefix = 'a'\n[sprite]\nsetup = 'bash s.sh'\nresume = 'true'\npass_env = []\n",
        ] {
            let config = validate(&format!("{partial}{JOB}")).unwrap();
            // The prefix applies to every resolved workflow.
            if partial.contains("sprite_prefix") {
                let (_, ci) = config.workflow(Some("ci"), None).unwrap();
                assert!(ci.settings.sprite_prefix.is_some());
            }
        }
        for (source, message) in [
            ("[sprite]\nunknown = 1\n", "unknown field"),
            (
                "[settings]\nsprite_prefix = ''\n",
                "sprite_prefix must be 1-20",
            ),
            (
                "[settings]\nsprite_prefix = 'Upper'\n",
                "sprite_prefix must be 1-20",
            ),
            (
                "[settings]\nsprite_prefix = '-lead'\n",
                "sprite_prefix must be 1-20",
            ),
            (
                "[settings]\nsprite_prefix = 'has_underscore'\n",
                "sprite_prefix must be 1-20",
            ),
            (
                "[settings]\nsprite_prefix = 'abcdefghijklmnopqrstu'\n",
                "sprite_prefix must be 1-20",
            ),
            ("[sprite]\nsetup = '  '\n", "setup must be a nonempty"),
            ("[sprite]\nresume = ''\n", "resume must be a nonempty"),
            ("[sprite]\npass_env = ['1BAD']\n", "pass_env entry '1BAD'"),
            ("[sprite]\npass_env = ['A=B']\n", "pass_env entry"),
            (
                "[sprite]\npass_env = ['A', 'A']\n",
                "pass_env lists 'A' more than once",
            ),
            (
                "[workflows.ci.settings]\nsprite_prefix = 'x'\n",
                "sprite_prefix is project-wide",
            ),
            (
                "[environments.e.settings]\nsprite_prefix = 'x'\n",
                "sprite_prefix is project-wide",
            ),
            ("[workflows.ci.sprite]\nsetup = 'true'\n", "unknown field"),
        ] {
            let error = validate(&format!("{source}{JOB}")).unwrap_err();
            assert!(error.contains(message), "{source}: {error}");
        }
    }

    #[test]
    fn sprite_state_dir_is_reserved_in_every_env_layer() {
        for entry in [
            "[settings.env]\nGOODFLOWS_SPRITE_STATE_DIR = 'x'\n",
            "[environments.e.settings.env]\nGOODFLOWS_SPRITE_STATE_DIR = 'x'\n",
            "[[workflows.third.jobs]]\nname = 'j'\ncommand = 'true'\nenv = { GOODFLOWS_SPRITE_STATE_DIR = 'x' }\n",
        ] {
            let error = validate(&format!("{JOB}{entry}")).unwrap_err();
            assert!(
                error.contains("reserved variable GOODFLOWS_SPRITE_STATE_DIR"),
                "{entry}: {error}"
            );
        }
    }

    #[test]
    fn job_and_step_env_may_still_set_goodflows_output() {
        // Compatibility: only settings env rejects it; commands still get their own file.
        validate(&format!(
            "{JOB}[[jobs]]\nname = 'b'\nenv = {{ GOODFLOWS_OUTPUT = 'x' }}\n[[jobs.steps]]\nid = 's'\nenv = {{ GOODFLOWS_OUTPUT = 'y' }}\ncommand = 'true'\n"
        ))
        .unwrap();
    }

    fn counts(config: &Config, workflow: &str, profile: Option<&str>) -> Vec<Option<Partitions>> {
        let (_, selected) = config.workflow(Some(workflow), profile).unwrap();
        selected.jobs.iter().map(|job| job.partitions).collect()
    }

    #[test]
    fn selected_profile_replaces_declared_partition_counts() {
        let config = validate(
            "[[workflows.ci.jobs]]\nname = 'test'\ncommand = 'true'\npartitions = { count = 3, strategy = 'index' }\n\
             [[workflows.ci.jobs]]\nname = 'lint'\ncommand = 'true'\npartitions = 2\n\
             [[workflows.ci.jobs]]\nname = 'compile'\ncommand = 'true'\n\
             [[workflows.solo.jobs]]\nname = 'compile'\ncommand = 'true'\n\
             [[workflows.nightly.jobs]]\nname = 'test'\ncommand = 'true'\n\
             [environments.local.settings]\nparallelism = 2\n\
             [environments.local.partitions]\ntest = 6\n\
             [environments.orb.partitions]\ntest = 1\nlint = 4\n\
             [environments.empty.partitions]\n",
        )
        .unwrap();
        let six = index(6);
        // Replaced count keeps the table form's strategy; unnamed jobs keep theirs.
        assert_eq!(counts(&config, "ci", Some("local")), [six, index(2), None]);
        assert_eq!(six.unwrap().strategy, Strategy::Index);
        // Bare run and an empty map keep the declared counts.
        assert_eq!(counts(&config, "ci", None), [index(3), index(2), None]);
        assert_eq!(
            counts(&config, "ci", Some("empty")),
            [index(3), index(2), None]
        );
        // A sibling profile resolves independently of earlier resolutions.
        assert_eq!(
            counts(&config, "ci", Some("orb")),
            [index(1), index(4), None]
        );
        assert_eq!(counts(&config, "ci", Some("local")), [six, index(2), None]);
        // The loaded document is never mutated.
        assert_eq!(config.workflows["ci"].jobs[0].partitions, index(3));
        assert_eq!(config.environments["local"].partitions["test"], 6);
        // A name absent from the selected workflow is ignored.
        assert_eq!(counts(&config, "solo", Some("local")), [None]);
        // The selected workflow's unpartitioned job with that name fails.
        let error = config
            .workflow(Some("nightly"), Some("local"))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "environment 'local': workflow 'nightly': job 'test' is not partitioned"
        );
        // Settings precedence is unchanged by the partition map.
        let (_, selected) = config.workflow(Some("ci"), Some("local")).unwrap();
        assert_eq!(selected.settings.parallelism, Some(2));
        // The resolved copy's parsed input follows the effective count.
        assert_eq!(
            selected.jobs[0].partitions_input.map(|input| input.count),
            Some(6)
        );
        assert_eq!(
            config.workflows["ci"].jobs[0]
                .partitions_input
                .map(|input| input.count),
            Some(3)
        );
    }

    #[test]
    fn unpartitioned_root_job_error_names_the_default_workflow() {
        let config = validate(
            "[[jobs]]\nname = 'test'\ncommand = 'true'\n\
             [[workflows.ci.jobs]]\nname = 'test'\ncommand = 'true'\npartitions = 3\n\
             [environments.local.partitions]\ntest = 6\n",
        )
        .unwrap();
        let error = config
            .workflow(None, Some("local"))
            .map(|_| ())
            .unwrap_err();
        assert_eq!(
            format!("{error:#}"),
            "environment 'local': workflow 'default' (root jobs): job 'test' is not partitioned"
        );
    }

    #[test]
    fn profile_partitions_are_validated_for_every_profile() {
        let base = "[[jobs]]\nname = 'test'\ncommand = 'true'\npartitions = 3\n[[jobs]]\nname = 'plain'\ncommand = 'true'\n";
        for (entry, message) in [
            (
                "test = 0",
                "environment 'unused': job 'test': partitions count must be from 1 to 64, got 0",
            ),
            (
                "test = 65",
                "environment 'unused': job 'test': partitions count must be from 1 to 64, got 65",
            ),
            (
                "test = -1",
                "environment 'unused': job 'test': partitions count must be from 1 to 64, got -1",
            ),
            (
                "test = 4294967302",
                "environment 'unused': job 'test': partitions count must be from 1 to 64, got 4294967302",
            ),
            (
                "missing = 2",
                "environment 'unused': unknown job 'missing' (partitioned jobs: test)",
            ),
            (
                "plain = 2",
                "environment 'unused': job 'plain' is not partitioned",
            ),
        ] {
            let error = validate(&format!(
                "{base}[environments.unused.partitions]\n{entry}\n"
            ))
            .unwrap_err();
            assert_eq!(error, message, "{entry}");
        }
        for entry in ["test = 'six'", "test = { count = 6 }", "test = 6.0"] {
            let error = parse(&format!(
                "{base}[environments.unused.partitions]\n{entry}\n"
            ))
            .unwrap_err();
            assert!(error.contains("invalid type"), "{entry}: {error}");
            assert!(error.contains("expected i64"), "{entry}: {error}");
            assert!(error.contains("line 9"), "{entry}: {error}");
            assert!(error.contains(entry), "{entry}: {error}");
        }
        validate(base).unwrap();
        validate(&format!(
            "{base}[environments.unused.partitions]\ntest = 64\n"
        ))
        .unwrap();
        let error = validate(
            "[[jobs]]\nname = 'plain'\ncommand = 'true'\n[environments.unused.partitions]\nmissing = 2\n",
        )
        .unwrap_err();
        assert_eq!(
            error,
            "environment 'unused': unknown job 'missing' (partitioned jobs: none)"
        );
        let config = validate(&format!("{base}[environments.local.partitions]\n")).unwrap();
        assert!(config.environments["local"].partitions.is_empty());
        let text = toml::to_string(&config).unwrap();
        assert!(!text.contains("[environments.local.partitions]"), "{text}");
        let config = validate(&format!(
            "{base}[environments.local.partitions]\ntest = 6\n"
        ))
        .unwrap();
        let text = toml::to_string(&config).unwrap();
        assert!(text.contains("[environments.local.partitions]"), "{text}");
    }

    fn history_settings(
        config: &Config,
        workflow: &str,
        profile: Option<&str>,
        cli: &HistoryOverrides,
    ) -> EffectiveSettings {
        let (_, mut selected) = config.workflow(Some(workflow), profile).unwrap();
        selected.settings.workdir = Some(PathBuf::from("/"));
        if selected.settings.history_dir.is_none() {
            selected.settings.history_dir = Some(PathBuf::from("/default"));
        }
        selected.settings.resolve(None, None, cli).unwrap()
    }

    #[test]
    fn history_settings_follow_shared_workflow_profile_precedence() {
        let config = validate(&format!(
            "{JOB}[settings]\nhistory = true\nhistory_dir = 'shared'\nlog_runs = 3\nhistory_runs = 30\nhistory_bytes = 300\n\
             [workflows.ci.settings]\nlog_runs = 4\nhistory_dir = 'wf'\n\
             [environments.off.settings]\nhistory = false\nhistory_dir = 'profile'\nhistory_bytes = 5\n\
             [environments.on.settings]\nhistory_runs = 6\n"
        ))
        .unwrap();
        let none = HistoryOverrides::default();
        // Shared values apply where nothing overrides them.
        let other = history_settings(&config, "other", None, &none);
        assert!(other.history);
        assert_eq!(other.history_dir, PathBuf::from("shared"));
        assert_eq!(
            (other.log_runs, other.history_runs, other.history_bytes),
            (3, 30, 300)
        );
        // Workflow beats shared.
        let ci = history_settings(&config, "ci", None, &none);
        assert_eq!(ci.history_dir, PathBuf::from("wf"));
        assert_eq!(ci.log_runs, 4);
        // An explicit profile false beats a shared true; the profile directory wins.
        let off = history_settings(&config, "ci", Some("off"), &none);
        assert!(!off.history);
        assert_eq!(off.history_dir, PathBuf::from("profile"));
        assert_eq!((off.log_runs, off.history_bytes), (4, 5));
        let on = history_settings(&config, "ci", Some("on"), &none);
        assert!(on.history);
        assert_eq!(on.history_runs, 6);
        // CLI overrides beat every scope.
        let cli = HistoryOverrides {
            history: Some(true),
            history_dir: Some(PathBuf::from("/cli")),
            log_runs: Some(7),
            history_runs: Some(8),
            history_bytes: Some(9),
        };
        let forced = history_settings(&config, "ci", Some("off"), &cli);
        assert!(forced.history);
        assert_eq!(forced.history_dir, PathBuf::from("/cli"));
        assert_eq!(
            (forced.log_runs, forced.history_runs, forced.history_bytes),
            (7, 8, 9)
        );
        let disabled = HistoryOverrides {
            history: Some(false),
            ..HistoryOverrides::default()
        };
        assert!(!history_settings(&config, "other", None, &disabled).history);
    }

    #[test]
    fn history_settings_default_when_unset() {
        let config = validate(JOB).unwrap();
        let settings = history_settings(&config, "ci", None, &HistoryOverrides::default());
        assert!(!settings.history);
        assert_eq!(settings.history_dir, PathBuf::from("/default"));
        assert_eq!(
            (
                settings.log_runs,
                settings.history_runs,
                settings.history_bytes
            ),
            (10, 1000, 10_485_760)
        );
    }

    #[test]
    fn zero_retention_caps_fail_validation_in_every_scope() {
        for scope in [
            "settings",
            "workflows.ci.settings",
            "environments.unused.settings",
        ] {
            for cap in ["log_runs", "history_runs", "history_bytes"] {
                let error = validate(&format!("{JOB}[{scope}]\n{cap} = 0\n")).unwrap_err();
                assert!(
                    error.contains(&format!("{cap} must be at least 1")),
                    "{scope} {cap}: {error}"
                );
            }
        }
        validate(&format!(
            "{JOB}[settings]\nlog_runs = 1\nhistory_runs = 1\nhistory_bytes = 1\n"
        ))
        .unwrap();
    }

    #[test]
    fn load_resolves_history_dirs_from_the_config_directory() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let path = base.join("goodflows.toml");
        std::fs::write(
            &path,
            format!("{JOB}[workflows.ci.settings]\nhistory_dir = 'wf'\n[environments.p.settings]\nhistory_dir = '/abs/profile'\n"),
        )
        .unwrap();
        let (config, _) = Config::load(Some(&path)).unwrap();
        assert_eq!(
            config.settings.history_dir,
            Some(base.join(DEFAULT_HISTORY_DIR))
        );
        assert_eq!(
            config.workflows["ci"].settings.history_dir,
            Some(base.join("wf"))
        );
        assert_eq!(
            config.environments["p"].settings.history_dir,
            Some(PathBuf::from("/abs/profile"))
        );
        let (_, other) = config.workflow(Some("other"), None).unwrap();
        assert_eq!(
            other.settings.history_dir,
            Some(base.join(DEFAULT_HISTORY_DIR))
        );
        std::fs::write(&path, format!("{JOB}[settings]\nhistory_dir = ''\n")).unwrap();
        let error = format!("{:#}", Config::load(Some(&path)).unwrap_err());
        assert!(error.contains("history_dir must not be empty"), "{error}");
    }

    #[test]
    fn overridden_invalid_shared_entry_still_fails() {
        let error = validate(&format!(
            "{JOB}[settings.env]\nGOODFLOWS_PARTITION = 'x'\n[workflows.ci.settings.env]\nGOODFLOWS_PARTITION = 'y'\n"
        ))
        .unwrap_err();
        // Validation reports both scopes; the shared one is not hidden by the override.
        assert!(
            error.contains("\n  - settings env: env must not set reserved variable"),
            "{error}"
        );
        assert!(
            error.contains("\n  - workflow 'ci' settings env:"),
            "{error}"
        );
    }
}
