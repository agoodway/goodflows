//! `goodflows skills install`: write the bundled agent skills into the
//! directory the current coding agent harness loads skills from.

use crate::config::ConfigError;
use crate::output::Output;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

/// The skills this binary installs, embedded as they are in `skills/`.
const SKILLS: [(&str, &str); 2] = [
    (
        "create-ci-skill",
        include_str!("../skills/create-ci-skill/SKILL.md"),
    ),
    (
        "maintain-ci-skill",
        include_str!("../skills/maintain-ci-skill/SKILL.md"),
    ),
];

/// The directory for an unknown harness.
const DEFAULT_DIR: &str = ".agents/skills";

/// A coding agent harness, by the skills directory it loads.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Harness {
    /// Amp: .agents/skills
    Amp,
    /// Claude Code: .claude/skills
    Claude,
    /// Codex: .agents/skills
    Codex,
    /// Cursor: .agents/skills
    Cursor,
    /// Gemini CLI: .agents/skills, its workspace alias for .gemini/skills
    Gemini,
    /// Grok CLI (Grok Build): .grok/skills
    Grok,
    /// OpenCode: .agents/skills
    Opencode,
    /// T3 Code: .agents/skills
    T3,
}

impl Harness {
    fn dir(self) -> &'static str {
        match self {
            Self::Claude => ".claude/skills",
            Self::Grok => ".grok/skills",
            Self::Amp | Self::Codex | Self::Cursor | Self::Gemini | Self::Opencode | Self::T3 => {
                DEFAULT_DIR
            }
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Amp => "amp",
            Self::Claude => "claude",
            Self::Codex => "codex",
            Self::Cursor => "cursor",
            Self::Gemini => "gemini",
            Self::Grok => "grok",
            Self::Opencode => "opencode",
            Self::T3 => "t3",
        }
    }

    /// The harness that a variable with a non-empty value names in the
    /// commands it runs.
    fn of_var(name: &str, value: &str) -> Option<Self> {
        Some(match name {
            "CLAUDECODE" => Self::Claude,
            "AGENT" | "AI_AGENT" if value == "amp" => Self::Amp,
            // Claude Code's value carries its version, such as
            // `claude-code_2-1-310_agent`.
            "AI_AGENT" if value.starts_with("claude-code") => Self::Claude,
            "AMP_CURRENT_THREAD_ID" => Self::Amp,
            "CURSOR_AGENT" => Self::Cursor,
            "GEMINI_CLI" => Self::Gemini,
            "GROK_AGENT" | "GROK_SESSION_ID" => Self::Grok,
            "OPENCODE" => Self::Opencode,
            "T3CODE_PROJECT_ROOT" | "T3CODE_WORKTREE_PATH" => Self::T3,
            // As for text output mode: `CODEX_HOME` is set by users themselves.
            name if name.starts_with("CODEX_") && name != "CODEX_HOME" => Self::Codex,
            _ => return None,
        })
    }
}

/// Where to install and why.
#[derive(Debug, PartialEq)]
struct Target {
    dir: &'static str,
    /// The harnesses `--harness` or the environment named; empty when none did.
    harnesses: Vec<Harness>,
    source: &'static str,
}

/// The install target: `--harness` when given, else the harnesses the
/// environment names. Detected harnesses that load different directories
/// are a usage error; none at all falls back to `.agents/skills`.
fn target(
    flag: Option<Harness>,
    vars: impl IntoIterator<Item = (OsString, OsString)>,
) -> Result<Target> {
    if let Some(harness) = flag {
        return Ok(Target {
            dir: harness.dir(),
            harnesses: vec![harness],
            source: "flag",
        });
    }
    // Each detected harness with the variables that named it.
    let mut found: BTreeMap<Harness, Vec<String>> = BTreeMap::new();
    for (name, value) in vars {
        let (Some(name), Some(value)) = (name.to_str(), value.to_str()) else {
            continue;
        };
        if value.is_empty() {
            continue;
        }
        if let Some(harness) = Harness::of_var(name, value) {
            found.entry(harness).or_default().push(name.to_owned());
        }
    }
    let harnesses: Vec<_> = found.keys().copied().collect();
    let Some(first) = harnesses.first() else {
        return Ok(Target {
            dir: DEFAULT_DIR,
            harnesses,
            source: "default",
        });
    };
    if harnesses.iter().any(|h| h.dir() != first.dir()) {
        let named: Vec<_> = found
            .iter_mut()
            .map(|(harness, vars)| {
                vars.sort();
                format!("{} ({})", harness.name(), vars.join(", "))
            })
            .collect();
        let mut error = ConfigError::new(
            "ambiguous_harness",
            format!(
                "the environment names more than one harness, which load different skill directories: {}",
                named.join(", ")
            ),
        );
        error.hint = Some("pass --harness to choose one".into());
        return Err(error.into());
    }
    Ok(Target {
        dir: first.dir(),
        harnesses,
        source: "environment",
    })
}

/// Write every bundled skill under `root`, replacing any earlier copy, and
/// return the files written, relative to `root`.
fn write(root: &Path, dir: &str) -> Result<Vec<String>> {
    let mut written = Vec::new();
    for (name, content) in SKILLS {
        let relative = format!("{dir}/{name}/SKILL.md");
        let path = root.join(&relative);
        let parent = path.parent().expect("a skill file has a directory");
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
        std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
        written.push(relative);
    }
    Ok(written)
}

/// `goodflows skills install`, in the current directory.
pub async fn install(harness: Option<Harness>, output: &Output) -> Result<u8> {
    let target = target(harness, std::env::vars_os())?;
    let files = write(&std::env::current_dir()?, target.dir)?;
    let harnesses: Vec<_> = target.harnesses.iter().map(|h| h.name()).collect();
    output
        .emit(json!({"type":"skills_installed", "directory":target.dir, "harnesses":harnesses, "source":target.source, "files":files}))
        .await?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vars(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
        pairs
            .iter()
            .map(|(name, value)| (name.into(), value.into()))
            .collect()
    }

    fn detect(pairs: &[(&str, &str)]) -> Result<Target> {
        target(None, vars(pairs))
    }

    #[track_caller]
    fn detected(pairs: &[(&str, &str)], harness: Harness, dir: &str) {
        let target = detect(pairs).unwrap();
        assert_eq!(
            (target.harnesses, target.dir, target.source),
            (vec![harness], dir, "environment"),
            "{pairs:?}"
        );
    }

    #[test]
    fn each_harness_is_detected_from_the_variables_it_sets() {
        use Harness::*;
        detected(&[("CLAUDECODE", "1")], Claude, ".claude/skills");
        detected(
            &[("AI_AGENT", "claude-code_2-1-310_agent")],
            Claude,
            ".claude/skills",
        );
        detected(&[("AGENT", "amp")], Amp, ".agents/skills");
        detected(&[("AI_AGENT", "amp")], Amp, ".agents/skills");
        detected(&[("AMP_CURRENT_THREAD_ID", "T-1")], Amp, ".agents/skills");
        detected(&[("CURSOR_AGENT", "1")], Cursor, ".agents/skills");
        detected(&[("GEMINI_CLI", "1")], Gemini, ".agents/skills");
        detected(&[("GROK_AGENT", "1")], Grok, ".grok/skills");
        detected(&[("GROK_SESSION_ID", "s")], Grok, ".grok/skills");
        // OpenCode also sets AGENT=1, which names no harness by itself.
        detected(
            &[("OPENCODE", "1"), ("AGENT", "1")],
            Opencode,
            ".agents/skills",
        );
        detected(&[("CODEX_THREAD_ID", "t")], Codex, ".agents/skills");
        detected(&[("CODEX_CI", "1")], Codex, ".agents/skills");
        detected(&[("T3CODE_PROJECT_ROOT", "/p")], T3, ".agents/skills");
    }

    #[test]
    fn unknown_or_empty_variables_fall_back_to_agents_skills() {
        for pairs in [
            &[][..],
            &[("CLAUDECODE", "")],
            &[("CODEX_HOME", "/home/me/.codex")],
            &[("AGENT", "goose"), ("AI_AGENT", "pi")],
            &[("CLAUDE_CONFIG_DIR", "/c"), ("PATH", "/bin")],
        ] {
            let target = detect(pairs).unwrap();
            assert_eq!(
                (target.dir, target.harnesses.len(), target.source),
                (".agents/skills", 0, "default"),
                "{pairs:?}"
            );
        }
    }

    #[test]
    fn harnesses_that_load_different_directories_are_ambiguous() {
        let error = detect(&[("CLAUDECODE", "1"), ("AGENT", "amp")]).unwrap_err();
        let config = error.downcast_ref::<ConfigError>().unwrap();
        assert_eq!(config.code, "ambiguous_harness");
        let message = format!("{error:#}");
        assert!(message.contains("amp (AGENT)"), "{message}");
        assert!(message.contains("claude (CLAUDECODE)"), "{message}");
        assert!(config.hint.as_deref().unwrap().contains("--harness"));
        assert!(detect(&[("GROK_AGENT", "1"), ("OPENCODE", "1")]).is_err());
        // T3 Code driving Codex sets both; both load .agents/skills.
        let target = detect(&[("T3CODE_PROJECT_ROOT", "/p"), ("CODEX_CI", "1")]).unwrap();
        assert_eq!(target.dir, ".agents/skills");
        assert_eq!(target.harnesses, [Harness::Codex, Harness::T3]);
    }

    #[test]
    fn the_harness_flag_overrides_the_environment() {
        let claude_session = vars(&[("CLAUDECODE", "1"), ("AGENT", "amp")]);
        for (harness, dir) in [
            (Harness::Grok, ".grok/skills"),
            (Harness::Amp, ".agents/skills"),
            (Harness::Gemini, ".agents/skills"),
            (Harness::Claude, ".claude/skills"),
        ] {
            let target = target(Some(harness), claude_session.clone()).unwrap();
            assert_eq!(
                (target.dir, target.harnesses, target.source),
                (dir, vec![harness], "flag")
            );
        }
    }

    #[test]
    fn install_writes_each_skill_and_refreshes_it_on_rerun() {
        let root = tempfile::tempdir().unwrap();
        let dir = detect(&[("CLAUDECODE", "1")]).unwrap().dir;
        let skill = root.path().join(".claude/skills/create-ci-skill/SKILL.md");
        std::fs::create_dir_all(skill.parent().unwrap()).unwrap();
        std::fs::write(&skill, "stale").unwrap();
        for _ in 0..2 {
            let files = write(root.path(), dir).unwrap();
            assert_eq!(
                files,
                [
                    ".claude/skills/create-ci-skill/SKILL.md",
                    ".claude/skills/maintain-ci-skill/SKILL.md"
                ]
            );
        }
        let read = |name: &str| {
            std::fs::read_to_string(root.path().join(format!(".claude/skills/{name}/SKILL.md")))
                .unwrap()
        };
        // The embedded copies are the repository's skill files, byte for byte.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
        for name in ["create-ci-skill", "maintain-ci-skill"] {
            let expected = std::fs::read_to_string(repo.join(name).join("SKILL.md")).unwrap();
            assert_eq!(read(name), expected, "{name}");
            assert!(expected.starts_with(&format!("---\nname: {name}\n")));
        }
        assert!(!root.path().join(".agents").exists());
    }
}
