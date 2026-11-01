use crate::expression::{Expression, Reference};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
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
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(deny_unknown_fields)]
pub struct Settings {
    pub parallelism: Option<usize>,
    #[serde(default = "default_true")]
    pub fail_fast: bool,
    pub workdir: Option<PathBuf>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            parallelism: None,
            fail_fast: true,
            workdir: None,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EffectiveSettings {
    pub parallelism: usize,
    pub fail_fast: bool,
    pub workdir: PathBuf,
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
}

impl SettingsOverrides {
    fn is_empty(&self) -> bool {
        self.parallelism.is_none() && self.fail_fast.is_none() && self.workdir.is_none()
    }

    fn apply(&self, shared: &Settings) -> Settings {
        Settings {
            parallelism: self.parallelism.or(shared.parallelism),
            fail_fast: self.fail_fast.unwrap_or(shared.fail_fast),
            workdir: self.workdir.clone().or_else(|| shared.workdir.clone()),
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

pub fn workflow_log_component(name: &str) -> String {
    // Encode uppercase too so distinct workflow names never alias on macOS's
    // commonly case-insensitive filesystem. Percent itself is always encoded.
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
        config.validate()?;
        Ok((config, path))
    }

    pub fn validate(&self) -> Result<()> {
        if self.jobs.is_empty() && self.workflows.is_empty() {
            bail!("no jobs defined");
        }
        if !self.jobs.is_empty() {
            if self.workflows.contains_key("default") {
                bail!("root jobs conflict with named workflow 'default'");
            }
            validate_jobs(&self.jobs)?;
        }
        for (name, workflow) in &self.workflows {
            validate_workflow_name(name)?;
            validate_jobs(&workflow.jobs).with_context(|| format!("workflow '{name}'"))?;
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

fn validate_jobs(jobs: &[Job]) -> Result<()> {
    if jobs.is_empty() {
        bail!("no jobs defined");
    }
    let mut names = HashMap::new();
    for (index, job) in jobs.iter().enumerate() {
        if job.name.trim().is_empty() {
            bail!("empty job name");
        }
        if names.insert(job.name.as_str(), index).is_some() {
            bail!("duplicate job name '{}'", job.name);
        }
        validate_job(job).with_context(|| format!("job '{}'", job.name))?;
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
    Ok(())
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

fn validate_env(env: &HashMap<String, String>) -> Result<()> {
    for (key, value) in env {
        if key.is_empty() || key.contains(['=', '\0']) || value.contains('\0') {
            bail!("invalid environment variable");
        }
    }
    Ok(())
}

fn validate_expression(source: &str, needs: &[String], steps: &[String]) -> Result<()> {
    let expression = Expression::parse(source)?;
    for reference in expression.references() {
        match reference {
            Reference::Needs { job, .. } if !needs.contains(job) => {
                bail!("output reference requires declared dependency '{job}'");
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
) -> Result<()> {
    match condition {
        Some(Condition::Expression { expression }) => {
            validate_expression(expression, needs, steps)
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

fn validate_job(job: &Job) -> Result<()> {
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
    validate_env(&job.env)?;
    validate_condition(job.condition.as_ref(), &job.depends_on, &[])?;
    let mut steps = Vec::new();
    for step in &job.steps {
        (|| {
            validate_name(&step.id, "step")?;
            if steps.contains(&step.id) {
                bail!("duplicate step id '{}'", step.id);
            }
            validate_command(&step.command, step.shell.unwrap_or(job.shell))?;
            validate_env(&step.env)?;
            validate_condition(step.condition.as_ref(), &job.depends_on, &steps)
        })()
        .with_context(|| format!("step '{}'", step.id))?;
        steps.push(step.id.clone());
    }
    for (key, source) in &job.outputs {
        validate_name(key, "output")?;
        validate_expression(source, &job.depends_on, &steps)
            .with_context(|| format!("output '{key}'"))?;
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
