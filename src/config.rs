use crate::expression::{Expression, Reference};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};

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
}

pub const REPORT_FORMAT: &str = crate::reporting::contract::REPORT_FORMAT;
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
    // Present only to reject credentials written inline with a clear message.
    #[serde(default, skip_serializing)]
    token: Option<toml::Value>,
    #[serde(default, skip_serializing)]
    api_key: Option<toml::Value>,
    #[serde(default, skip_serializing)]
    url: Option<toml::Value>,
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
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallelism: None,
            fail_fast: true,
            workdir: None,
            env: BTreeMap::new(),
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
}

impl Settings {
    pub fn resolve(
        &self,
        parallelism: Option<usize>,
        fail_fast: Option<bool>,
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
        })
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub name: String,
    pub command: Option<String>,
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
}

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct SettingsOverrides {
    pub parallelism: Option<usize>,
    pub fail_fast: Option<bool>,
    pub workdir: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
}

impl SettingsOverrides {
    fn is_empty(&self) -> bool {
        self.parallelism.is_none()
            && self.fail_fast.is_none()
            && self.workdir.is_none()
            && self.env.is_empty()
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
                .context("no config found (looked for goodflows.toml and .goodflows.toml)")?,
        };
        let path = path
            .canonicalize()
            .with_context(|| format!("failed to read {}", path.display()))?;
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let mut config: Self = toml::from_str(&content)
            .with_context(|| format!("failed to parse {}", path.display()))?;
        let base = path.parent().context("config has no parent directory")?;
        let shared_workdir = config.settings.workdir.clone();
        if !config.jobs.is_empty() {
            config.settings.workdir = Some(resolve_workdir(base, shared_workdir.as_deref())?);
        }
        for (name, workflow) in &mut config.workflows {
            workflow.settings.workdir = Some(
                resolve_workdir(
                    base,
                    workflow
                        .settings
                        .workdir
                        .as_deref()
                        .or(shared_workdir.as_deref()),
                )
                .with_context(|| format!("workflow '{name}'"))?,
            );
        }
        for (name, environment) in &mut config.environments {
            if let Some(workdir) = &environment.settings.workdir {
                environment.settings.workdir = Some(
                    resolve_workdir(base, Some(workdir))
                        .with_context(|| format!("environment '{name}'"))?,
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
        config.validate()?;
        Ok((config, path))
    }

    /// Validate every job and store the validated partition settings.
    pub fn validate(&mut self) -> Result<()> {
        if self.jobs.is_empty() && self.workflows.is_empty() {
            bail!("no jobs defined");
        }
        if let Some(reporting) = &self.reporting {
            reporting.validate().context("reporting")?;
        }
        let reporting = self.reporting.as_ref();
        // Validate every authored settings map, including unselected workflows and
        // profiles and entries a later scope overrides.
        validate_settings_env(&self.settings.env, reporting).context("settings env")?;
        for (name, workflow) in &self.workflows {
            validate_settings_env(&workflow.settings.env, reporting)
                .with_context(|| format!("workflow '{name}' settings env"))?;
        }
        for (name, environment) in &self.environments {
            validate_settings_env(&environment.settings.env, reporting)
                .with_context(|| format!("environment '{name}' settings env"))?;
        }
        if !self.jobs.is_empty() {
            if self.workflows.contains_key("default") {
                bail!("root jobs conflict with named workflow 'default'");
            }
            let partitions = validate_jobs(&self.jobs, reporting)?;
            store_partitions(&mut self.jobs, partitions);
        }
        for (name, workflow) in &mut self.workflows {
            validate_workflow_name(name)?;
            let partitions = validate_jobs(&workflow.jobs, reporting)
                .with_context(|| format!("workflow '{name}'"))?;
            store_partitions(&mut workflow.jobs, partitions);
        }
        for name in self.environments.keys() {
            validate_name(name, "environment")?;
        }
        Ok(())
    }

    pub fn environment(&self, requested: Option<&str>) -> Result<Option<&SettingsOverrides>> {
        requested
            .map(|name| {
                self.environments
                    .get(name)
                    .map(|environment| &environment.settings)
                    .with_context(|| {
                        let available = self
                            .environments
                            .keys()
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("unknown environment '{name}' (available: {available})")
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

    pub fn workflow(
        &self,
        requested: Option<&str>,
        environment: Option<&SettingsOverrides>,
    ) -> Result<(String, Config)> {
        let available = self.workflow_names().join(", ");
        let name = requested.or_else(|| self.default_workflow())
            .with_context(|| format!("no default workflow; choose a workflow with 'goodflows run <workflow>' (available: {available})"))?;
        let (settings, jobs) = if name == "default" && !self.jobs.is_empty() {
            (self.settings.clone(), self.jobs.clone())
        } else {
            let workflow = self
                .workflows
                .get(name)
                .with_context(|| format!("unknown workflow '{name}' (available: {available})"))?;
            (
                workflow.settings.apply(&self.settings),
                workflow.jobs.clone(),
            )
        };
        Ok((
            name.to_owned(),
            Config {
                settings: match environment {
                    Some(overrides) => overrides.apply(&settings),
                    None => settings,
                },
                jobs,
                workflows: BTreeMap::new(),
                environments: BTreeMap::new(),
                reporting: self.reporting.clone(),
            },
        ))
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
            stack.push(
                *names
                    .get(name.as_str())
                    .with_context(|| format!("unknown job '{name}'"))?,
            );
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

fn store_partitions(jobs: &mut [Job], partitions: Vec<Option<Partitions>>) {
    for (job, partitions) in jobs.iter_mut().zip(partitions) {
        job.partitions = partitions;
    }
}

/// Returns each job's validated partition setting, in job order.
fn validate_jobs(jobs: &[Job], reporting: Option<&Reporting>) -> Result<Vec<Option<Partitions>>> {
    if jobs.is_empty() {
        bail!("no jobs defined");
    }
    let partitioned: HashSet<&str> = jobs
        .iter()
        .filter(|job| job.partitions_input.is_some())
        .map(|job| job.name.as_str())
        .collect();
    let mut names = HashMap::new();
    let mut partitions = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        if job.name.trim().is_empty() {
            bail!("empty job name");
        }
        if names.insert(job.name.as_str(), index).is_some() {
            bail!("duplicate job name '{}'", job.name);
        }
        partitions.push(
            validate_job(job, reporting, &partitioned)
                .with_context(|| format!("job '{}'", job.name))?,
        );
        if job.timeout_secs == Some(0) {
            bail!("job '{}' timeout_secs must be greater than zero", job.name);
        }
        if job.name.contains(['\n', '\r', '\0']) {
            bail!(
                "job '{}' contains invalid control characters",
                job.name.escape_debug()
            );
        }
    }
    let mut indegrees = vec![0; jobs.len()];
    let mut dependents = vec![Vec::new(); jobs.len()];
    for (index, job) in jobs.iter().enumerate() {
        let mut seen = HashSet::new();
        for dep in &job.depends_on {
            if dep == &job.name {
                bail!("job '{}' depends on itself", job.name);
            }
            if !seen.insert(dep) {
                bail!("job '{}' has duplicate dependency '{}'", job.name, dep);
            }
            let &dep_index = names
                .get(dep.as_str())
                .with_context(|| format!("job '{}' depends on unknown job '{}'", job.name, dep))?;
            indegrees[index] += 1;
            dependents[dep_index].push(index);
        }
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
        bail!("dependency cycle detected: {}", cycle_path(jobs, &names));
    }
    Ok(partitions)
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

/// Names of the partitioned jobs in one workflow; they publish no job outputs.
type Partitioned<'a> = HashSet<&'a str>;

fn validate_expression(
    source: &str,
    needs: &[String],
    steps: &[String],
    partitioned: &Partitioned,
) -> Result<()> {
    let expression = Expression::parse(source)?;
    for reference in expression.references() {
        match reference {
            Reference::Needs { job, .. } if !needs.contains(job) => {
                bail!("output reference requires declared dependency '{job}'");
            }
            Reference::Needs { job, .. } if partitioned.contains(job.as_str()) => {
                bail!("partitioned job '{job}' publishes no outputs");
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
    partitioned: &Partitioned,
) -> Result<()> {
    match condition {
        Some(Condition::Expression { expression }) => {
            validate_expression(expression, needs, steps, partitioned)
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
        if format != REPORT_FORMAT {
            bail!(
                "reports: unsupported format '{}' (supported: {REPORT_FORMAT})",
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
    let count = u32::try_from(input.count)
        .ok()
        .filter(|count| *count <= MAX_PARTITIONS)
        .and_then(NonZeroU32::new)
        .with_context(|| {
            format!(
                "partitions count must be from 1 to {MAX_PARTITIONS}, got {}",
                input.count
            )
        })?;
    if !job.outputs.is_empty() {
        bail!("partitioned jobs publish no job outputs; remove the outputs mapping (step outputs remain available within each instance)");
    }
    Ok(Some(Partitions {
        count,
        strategy: input.strategy,
    }))
}

/// Returns the job's validated partition setting.
fn validate_job(
    job: &Job,
    reporting: Option<&Reporting>,
    partitioned: &Partitioned,
) -> Result<Option<Partitions>> {
    let partitions = validate_partitions(job)?;
    validate_reports(job, reporting.is_some())?;
    match (&job.command, job.steps.is_empty()) {
        (Some(command), true) => {
            validate_command(command, job.shell)?;
            if !job.outputs.is_empty() {
                bail!("command-only jobs publish outputs directly; output mappings require steps");
            }
        }
        (None, false) => {}
        _ => bail!("define either a command or nonempty steps, never both"),
    }
    validate_env(&job.env, reporting)?;
    validate_condition(job.condition.as_ref(), &job.depends_on, &[], partitioned)?;
    let mut steps = Vec::new();
    for step in &job.steps {
        (|| {
            validate_name(&step.id, "step")?;
            if steps.contains(&step.id) {
                bail!("duplicate step id '{}'", step.id);
            }
            validate_command(&step.command, step.shell.unwrap_or(job.shell))?;
            validate_env(&step.env, reporting)?;
            validate_condition(
                step.condition.as_ref(),
                &job.depends_on,
                &steps,
                partitioned,
            )
        })()
        .with_context(|| format!("step '{}'", step.id))?;
        steps.push(step.id.clone());
    }
    for (key, source) in &job.outputs {
        validate_name(key, "output")?;
        validate_expression(source, &job.depends_on, &steps, partitioned)
            .with_context(|| format!("output '{key}'"))?;
    }
    Ok(partitions)
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

    fn resolved_env(
        config: &Config,
        workflow: &str,
        profile: Option<&str>,
    ) -> BTreeMap<String, String> {
        let environment = config.environment(profile).unwrap();
        let (_, mut selected) = config.workflow(Some(workflow), environment).unwrap();
        selected.settings.workdir = Some(PathBuf::from("/"));
        selected.settings.resolve(None, None).unwrap().env
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
    fn job_and_step_env_may_still_set_goodflows_output() {
        // Compatibility: only settings env rejects it; commands still get their own file.
        validate(&format!(
            "{JOB}[[jobs]]\nname = 'b'\nenv = {{ GOODFLOWS_OUTPUT = 'x' }}\n[[jobs.steps]]\nid = 's'\nenv = {{ GOODFLOWS_OUTPUT = 'y' }}\ncommand = 'true'\n"
        ))
        .unwrap();
    }

    #[test]
    fn overridden_invalid_shared_entry_still_fails() {
        let error = validate(&format!(
            "{JOB}[settings.env]\nGOODFLOWS_PARTITION = 'x'\n[workflows.ci.settings.env]\nGOODFLOWS_PARTITION = 'y'\n"
        ))
        .unwrap_err();
        assert!(error.starts_with("settings env"), "{error}");
    }
}
