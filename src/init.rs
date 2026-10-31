use crate::config::{Config, Job, Settings};
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
    output: &Output,
) -> Result<()> {
    let root = std::env::current_dir()?.canonicalize()?;
    let path = destination.unwrap_or(Path::new("goodci.toml"));
    let exists = path.try_exists()?;
    let template = match template {
        Some(t) => t,
        None => detect(&root)?,
    };
    // Resolve the template before touching an existing file or making directories.
    let jobs = jobs(template, &root)?;
    if exists && !force {
        if output.json {
            if io::stdin().is_terminal() {
                eprint!("{} already exists. Overwrite? [y/N] ", path.display());
                io::stderr().flush()?;
            }
        } else {
            print!("{} already exists. Overwrite? [y/N] ", path.display());
            io::stdout().flush()?;
        }
        let mut answer = String::new();
        io::stdin()
            .read_line(&mut answer)
            .context("failed to read overwrite confirmation")?;
        if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
            return output
                .emit(json!({"type":"init_cancelled", "path":path}))
                .await;
        }
    }
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
    let config = Config {
        settings: Settings {
            workdir: Some(relative_path(&parent, &root)),
            ..Settings::default()
        },
        jobs,
    };
    config.validate()?;
    let content = format!("# goodci.toml — {} project checks\n# Commands run relative to settings.workdir (resolved from this file).\n\n{}", template.name(), toml::to_string_pretty(&config)?);
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
        command: command.into(),
        depends_on: deps.iter().map(|s| (*s).into()).collect(),
        timeout_secs: None,
        env: HashMap::new(),
        shell: true,
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
        Template::Generic => vec![job("check", "echo 'Replace this command with your project checks'", &[])],
        Template::Node => node_jobs(root)?,
    })
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
