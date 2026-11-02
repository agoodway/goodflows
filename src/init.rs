use crate::config::{Config, Job, Settings, SettingsOverrides, Workflow};
use crate::output::Output;
use anyhow::{bail, Context, Result};
use clap::ValueEnum;
use serde_json::json;
use std::collections::HashMap;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Template {
    Rust,
    Elixir,
    Node,
    Go,
    Generic,
}

impl Template {
    fn name(self) -> &'static str {
        match self {
            Self::Rust => "rust",
            Self::Elixir => "elixir",
            Self::Node => "node",
            Self::Go => "go",
            Self::Generic => "generic",
        }
    }
}

pub async fn generate(
    destination: Option<&Path>,
    template: Option<Template>,
    force: bool,
    workflow: Option<&str>,
    output: &Output,
) -> Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let path = destination.unwrap_or(Path::new("goodflows.toml"));
    let exists = path.try_exists()?;
    if exists && !force {
        let terminal = io::stdin().is_terminal();
        if terminal {
            if output.json {
                eprint!("{} already exists. Overwrite? [y/N] ", path.display());
                io::stderr().flush()?;
            } else {
                print!("{} already exists. Overwrite? [y/N] ", path.display());
                io::stdout().flush()?;
            }
        }
        let answer = if terminal {
            let mut answer = String::new();
            io::stdin()
                .read_line(&mut answer)
                .context("failed to read overwrite confirmation")?;
            answer
        } else {
            available_confirmation().context("failed to read overwrite confirmation")?
        };
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return output
                .emit(json!({"type":"init_cancelled", "path":path}))
                .await;
        }
    }
    let template = match template {
        Some(t) => t,
        None => detect(&root)?,
    };
    // Resolve the template before touching an existing file or making directories.
    let jobs = jobs(template, &root)?;
    let absolute = if exists {
        path.canonicalize()?
    } else {
        root.join(path)
    };
    let parent = absolute
        .parent()
        .context("config path has no parent directory")?;
    std::fs::create_dir_all(parent)
        .with_context(|| format!("failed to create {}", parent.display()))?;
    let parent = parent.canonicalize()?;
    let mut config = Config {
        settings: Settings {
            workdir: Some(relative_path(&parent, &root)),
            ..Settings::default()
        },
        jobs,
        workflows: Default::default(),
        environments: Default::default(),
        reporting: None,
    };
    if let Some(name) = workflow {
        config.workflows.insert(
            name.into(),
            Workflow {
                settings: SettingsOverrides::default(),
                jobs: std::mem::take(&mut config.jobs),
            },
        );
    }
    config.validate()?;
    let content = format!("# goodflows.toml — {} project checks\n# Commands run relative to settings.workdir (resolved from this file).\n# Omitted parallelism uses every logical CPU.\n# Set parallelism to the machine size before a run.\n\n{}", template.name(), generated_toml(&config)?);
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(exists)
        .truncate(exists)
        .create_new(!exists)
        .open(path)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.write_all(content.as_bytes())
        .with_context(|| format!("failed to write {}", path.display()))?;
    output
        .emit(json!({"type":"config_written", "path":path, "template":template.name()}))
        .await
}

fn detect(root: &Path) -> Result<Template> {
    let templates: Vec<_> = [
        ("Cargo.toml", Template::Rust),
        ("mix.exs", Template::Elixir),
        ("package.json", Template::Node),
        ("go.mod", Template::Go),
    ]
    .into_iter()
    .filter(|(file, _)| root.join(file).is_file())
    .map(|(_, t)| t)
    .collect();
    match templates.as_slice() {
        [] => Ok(Template::Generic),
        [template] => Ok(*template),
        _ => bail!(
            "multiple project types detected ({}); choose --template",
            templates
                .iter()
                .map(|t| t.name())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn job(name: &str, command: &str, deps: &[&str]) -> Job {
    Job {
        name: name.into(),
        command: Some(command.into()),
        condition: None,
        steps: Vec::new(),
        outputs: Default::default(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        timeout_secs: None,
        env: HashMap::new(),
        shell: true,
        reports: Vec::new(),
        partitions_input: None,
        partitions: None,
    }
}

fn jobs(template: Template, root: &Path) -> Result<Vec<Job>> {
    Ok(match template {
        Template::Rust => vec![
            job("fmt", "cargo fmt --check", &[]),
            job("clippy", "cargo clippy --locked --all-targets -- -D warnings", &["fmt"]),
            job("test", "cargo test --locked --all", &["fmt"]),
            job("doc", "cargo doc --locked --no-deps", &["fmt"]),
        ],
        Template::Elixir => {
            let mut jobs = vec![
                job("fmt", "mix format --check-formatted", &[]),
                job("compile", "mix compile --warnings-as-errors", &["fmt"]),
                job("test", "mix test", &["compile"]),
            ];
            for job in &mut jobs { job.env.insert("MIX_ENV".into(), "test".into()); }
            jobs
        }
        Template::Go => vec![
            job("fmt", "files=$(gofmt -l .) && if [ -n \"$files\" ]; then printf '%s\\n' \"$files\"; exit 1; fi", &[]),
            job("vet", "go vet ./...", &["fmt"]),
            job("test", "go test ./...", &["fmt"]),
        ],
        Template::Generic => vec![job("check", "printf '%s\\n' 'Replace this command with your project checks' >&2; exit 1", &[])],
        Template::Node => node_jobs(root)?,
    })
}

// Only generated TOML omits default values. JSON inspection retains every field.
fn generated_toml(config: &Config) -> Result<String> {
    let mut document = toml::Value::try_from(config)?;
    clean_jobs(document.get_mut("jobs"));
    if let Some(workflows) = document
        .get_mut("workflows")
        .and_then(toml::Value::as_table_mut)
    {
        for (_, workflow) in workflows {
            clean_jobs(workflow.get_mut("jobs"));
        }
    }
    Ok(toml::to_string_pretty(&document)?)
}

fn clean_jobs(value: Option<&mut toml::Value>) {
    if let Some(jobs) = value.and_then(toml::Value::as_array_mut) {
        for job in jobs {
            if let Some(table) = job.as_table_mut() {
                table.retain(|key, value| match key {
                    "depends_on" => !value.as_array().is_some_and(Vec::is_empty),
                    "env" => !value.as_table().is_some_and(toml::map::Map::is_empty),
                    "shell" => value.as_bool() != Some(true),
                    _ => true,
                });
            }
        }
    }
}

// A readiness check followed by read_line can still block on a partial line.
// Use nonblocking reads, restoring the inherited open-file status flags on exit.
fn available_confirmation() -> io::Result<String> {
    struct RestoreFlags(i32);
    impl Drop for RestoreFlags {
        fn drop(&mut self) {
            // SAFETY: restore the original flags of the inherited stdin descriptor.
            unsafe {
                libc::fcntl(0, libc::F_SETFL, self.0);
            }
        }
    }
    // SAFETY: fcntl validates stdin and reports an error if it is closed.
    let flags = unsafe { libc::fcntl(0, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    if unsafe { libc::fcntl(0, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(io::Error::last_os_error());
    }
    let _restore = RestoreFlags(flags);
    let mut answer = Vec::new();
    while answer.len() < 4096 {
        let mut byte = 0u8;
        // SAFETY: byte is a writable one-byte buffer; no other code reads stdin.
        match unsafe { libc::read(0, (&mut byte as *mut u8).cast(), 1) } {
            0 => return Ok(String::from_utf8_lossy(&answer).into_owned()),
            1 => {
                if byte == b'\n' {
                    return Ok(String::from_utf8_lossy(&answer).into_owned());
                }
                answer.push(byte);
            }
            _ => match io::Error::last_os_error() {
                error if error.kind() == io::ErrorKind::WouldBlock => return Ok(String::new()),
                error if error.kind() == io::ErrorKind::Interrupted => continue,
                error => return Err(error),
            },
        }
    }
    Ok(String::new())
}

fn node_jobs(root: &Path) -> Result<Vec<Job>> {
    let managers: Vec<_> = [
        (&["package-lock.json", "npm-shrinkwrap.json"][..], "npm"),
        (&["pnpm-lock.yaml"][..], "pnpm"),
        (&["yarn.lock"][..], "yarn"),
        (&["bun.lock", "bun.lockb"][..], "bun"),
    ]
    .into_iter()
    .filter(|(files, _)| files.iter().any(|file| root.join(file).is_file()))
    .map(|(_, manager)| manager)
    .collect();
    if managers.len() > 1 {
        bail!("conflicting Node package-manager lockfiles; keep the lockfile for the package manager you use");
    }
    let manager = managers.first().copied().unwrap_or("npm");
    let path = root.join("package.json");
    if !path.try_exists()? {
        bail!(
            "Node template requires package.json so checks can be generated from declared scripts"
        );
    }
    let manifest: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path)?)
        .context("failed to parse package.json")?;
    let scripts = manifest["scripts"]
        .as_object()
        .context("package.json has no check scripts; use --template generic or add scripts")?;
    let mut jobs = Vec::new();
    for name in [
        "format:check",
        "lint",
        "typecheck",
        "check",
        "test",
        "build",
    ] {
        if let Some(script) = scripts.get(name) {
            if script.as_str().is_none_or(|s| s.trim().is_empty()) {
                bail!("package.json script '{name}' must be a non-empty string");
            }
            jobs.push(job(name, &format!("{manager} run {name}"), &[]));
        }
    }
    if jobs.is_empty() {
        bail!("package.json has no supported check scripts; use --template generic or add scripts");
    }
    Ok(jobs)
}

fn relative_path(base: &Path, target: &Path) -> PathBuf {
    let base: Vec<_> = base.components().collect();
    let target: Vec<_> = target.components().collect();
    let common = base.iter().zip(&target).take_while(|(a, b)| a == b).count();
    let mut result = PathBuf::new();
    for _ in common..base.len() {
        result.push("..");
    }
    for component in &target[common..] {
        result.push(component.as_os_str());
    }
    if result.as_os_str().is_empty() {
        result.push(".");
    }
    result
}
