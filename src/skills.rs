//! `goodflows skills install`: write the bundled agent skills into the
//! directory the current coding agent harness loads skills from.

use crate::config::ConfigError;
use crate::output::Output;
use anyhow::{Context, Result};
use serde_json::json;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::Path;

/// The skills this binary installs, embedded as they are in `skills/`: each
/// skill's name and its files, `SKILL.md` first.
const SKILLS: [(&str, &[(&str, &str)]); 7] = [
    (
        "create-ci-skill",
        &[(
            "SKILL.md",
            include_str!("../skills/create-ci-skill/SKILL.md"),
        )],
    ),
    (
        "gf-change-build",
        &[
            (
                "SKILL.md",
                include_str!("../skills/gf-change-build/SKILL.md"),
            ),
            (
                "build-prompt.md",
                include_str!("../skills/gf-change-build/build-prompt.md"),
            ),
            (
                "close-prompt.md",
                include_str!("../skills/gf-change-build/close-prompt.md"),
            ),
            (
                "fix-prompt.md",
                include_str!("../skills/gf-change-build/fix-prompt.md"),
            ),
            (
                "preflight-prompt.md",
                include_str!("../skills/gf-change-build/preflight-prompt.md"),
            ),
            (
                "ship-prompt.md",
                include_str!("../skills/gf-change-build/ship-prompt.md"),
            ),
            (
                "verify-prompt.md",
                include_str!("../skills/gf-change-build/verify-prompt.md"),
            ),
        ],
    ),
    (
        "gf-change-merge",
        &[(
            "SKILL.md",
            include_str!("../skills/gf-change-merge/SKILL.md"),
        )],
    ),
    (
        "gf-change-request",
        &[(
            "SKILL.md",
            include_str!("../skills/gf-change-request/SKILL.md"),
        )],
    ),
    (
        "gf-change-write",
        &[
            (
                "SKILL.md",
                include_str!("../skills/gf-change-write/SKILL.md"),
            ),
            (
                "builder-check-prompt.md",
                include_str!("../skills/gf-change-write/builder-check-prompt.md"),
            ),
            (
                "over-spec-check-prompt.md",
                include_str!("../skills/gf-change-write/over-spec-check-prompt.md"),
            ),
            (
                "review-check-prompt.md",
                include_str!("../skills/gf-change-write/review-check-prompt.md"),
            ),
        ],
    ),
    (
        "gf-setup",
        &[
            ("SKILL.md", include_str!("../skills/gf-setup/SKILL.md")),
            ("replay.py", include_str!("../skills/gf-setup/replay.py")),
        ],
    ),
    (
        "maintain-ci-skill",
        &[(
            "SKILL.md",
            include_str!("../skills/maintain-ci-skill/SKILL.md"),
        )],
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
    for (name, files) in SKILLS {
        let skill = root.join(dir).join(name);
        std::fs::create_dir_all(&skill).with_context(|| format!("create {}", skill.display()))?;
        for (file, content) in files {
            let path = skill.join(file);
            std::fs::write(&path, content).with_context(|| format!("write {}", path.display()))?;
            written.push(format!("{dir}/{name}/{file}"));
        }
    }
    Ok(written)
}

/// The lines that open and close goodflows's block in a project's AGENTS.md.
const FORMATS_START: &str = "<!-- goodflows:formats -->";
const FORMATS_END: &str = "<!-- /goodflows:formats -->";

/// The block that points agents at `goodflows schema`.
fn formats_block() -> String {
    use crate::change_requests::{markdown, verify_run};
    format!(
        "{FORMATS_START}
## File formats

This project lands work through goodflows. Before writing a scenario set, a verify run's run.md, a change
request's claims file, a feature file, or a review policy, run `goodflows schema show <format>` and follow it instead of a
skill's default format:

- Scenario sets: `{set}` (`--source intent` or `--source diff`)
- Verify run notes: `{notes}`
- Change request claims: `{claims}`
- Feature files: `{feature}`
- Review policy (TOML): `{policy}`

After writing, run the validator the schema names and fix every error it lists.
{FORMATS_END}
",
        set = markdown::SET_FORMAT,
        notes = verify_run::NOTES_FORMAT,
        claims = markdown::CLAIMS_FORMAT,
        feature = crate::schema::FEATURE_FORMAT,
        policy = crate::change_requests::POLICY_FORMAT,
    )
}

/// `text` with goodflows's formats block replaced in place, or appended
/// when it has none. The block is a line equal to [`FORMATS_START`] up to
/// the next line equal to [`FORMATS_END`], with no other start line
/// between; a marker anywhere else is the project's text. Every byte
/// outside the block is kept.
fn with_formats_block(text: &str) -> String {
    let block = formats_block();
    let mut start = None;
    let mut offset = 0;
    let mut found = None;
    for line in text.split_inclusive('\n') {
        let bare = line.trim_end_matches(['\n', '\r']);
        if bare == FORMATS_START {
            start = Some(offset);
        } else if let (FORMATS_END, Some(start)) = (bare, start) {
            found = Some((start, offset + line.len()));
            break;
        }
        offset += line.len();
    }
    match found {
        Some((start, end)) => format!("{}{block}{}", &text[..start], &text[end..]),
        None if text.is_empty() => block,
        None if text.ends_with('\n') => format!("{text}\n{block}"),
        None => format!("{text}\n\n{block}"),
    }
}

/// Writes the formats block into `path`, creating the file when missing,
/// and returns whether the file changed.
fn write_formats_block(path: &Path) -> Result<bool> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "could not read {} as UTF-8 text; fix or move it, then run goodflows skills install again",
                    path.display()
                )
            })
        }
    };
    let updated = with_formats_block(&text);
    if updated == text {
        return Ok(false);
    }
    std::fs::write(path, updated).with_context(|| {
        format!(
            "could not write {}; make it writable, then run goodflows skills install again",
            path.display()
        )
    })?;
    Ok(true)
}

/// Whether CLAUDE.md already loads AGENTS.md with an `@AGENTS.md` import line.
fn imports_agents_md(root: &Path) -> bool {
    std::fs::read_to_string(root.join("CLAUDE.md"))
        .is_ok_and(|text| text.lines().any(|line| line.trim() == "@AGENTS.md"))
}

/// Writes the formats block where the harnesses read project guidance:
/// AGENTS.md always, and CLAUDE.md for Claude Code, which does not read
/// AGENTS.md, unless CLAUDE.md already imports it. Returns the files that
/// changed.
fn write_guidance(root: &Path, harnesses: &[Harness]) -> Result<Vec<String>> {
    let mut files = vec!["AGENTS.md"];
    if harnesses.contains(&Harness::Claude) && !imports_agents_md(root) {
        files.push("CLAUDE.md");
    }
    let mut written = Vec::new();
    for file in files {
        if write_formats_block(&root.join(file))? {
            written.push(file.to_owned());
        }
    }
    Ok(written)
}

/// `goodflows skills install`, in the current directory.
pub async fn install(harness: Option<Harness>, output: &Output) -> Result<u8> {
    let target = target(harness, std::env::vars_os())?;
    let root = std::env::current_dir()?;
    let mut files = write(&root, target.dir)?;
    files.extend(write_guidance(&root, &target.harnesses)?);
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
    fn the_formats_block_is_replaced_in_place_and_other_text_kept() {
        let block = formats_block();
        assert_eq!(with_formats_block(""), block);
        assert_eq!(
            with_formats_block("# Rules\r\nNo tabs."),
            format!("# Rules\r\nNo tabs.\n\n{block}")
        );
        let old = "<!-- goodflows:formats -->\nold text\n<!-- /goodflows:formats -->\r\n";
        let file = format!("# Rules\n\n{old}After.\n");
        let updated = with_formats_block(&file);
        assert_eq!(updated, format!("# Rules\n\n{block}After.\n"));
        assert_eq!(with_formats_block(&updated), updated);
        assert!(block.contains("goodflows schema show <format>"));
        // A marker inside a line, or a start line with no end line, is the
        // project's text: the block is appended, and a rerun replaces only it.
        for project in [
            "Mention <!-- goodflows:formats --> inline.\nKeep this line.\n",
            "<!-- goodflows:formats -->\nKeep this line.\n",
        ] {
            let once = with_formats_block(project);
            assert_eq!(once, format!("{project}\n{block}"));
            assert_eq!(with_formats_block(&once), once);
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
                    ".claude/skills/gf-change-build/SKILL.md",
                    ".claude/skills/gf-change-build/build-prompt.md",
                    ".claude/skills/gf-change-build/close-prompt.md",
                    ".claude/skills/gf-change-build/fix-prompt.md",
                    ".claude/skills/gf-change-build/preflight-prompt.md",
                    ".claude/skills/gf-change-build/ship-prompt.md",
                    ".claude/skills/gf-change-build/verify-prompt.md",
                    ".claude/skills/gf-change-merge/SKILL.md",
                    ".claude/skills/gf-change-request/SKILL.md",
                    ".claude/skills/gf-change-write/SKILL.md",
                    ".claude/skills/gf-change-write/builder-check-prompt.md",
                    ".claude/skills/gf-change-write/over-spec-check-prompt.md",
                    ".claude/skills/gf-change-write/review-check-prompt.md",
                    ".claude/skills/gf-setup/SKILL.md",
                    ".claude/skills/gf-setup/replay.py",
                    ".claude/skills/maintain-ci-skill/SKILL.md"
                ]
            );
        }
        // The embedded copies are the repository's skill files, byte for byte,
        // and every file in a skill's directory is embedded.
        let repo = Path::new(env!("CARGO_MANIFEST_DIR")).join("skills");
        let mut names: Vec<_> = std::fs::read_dir(&repo)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, SKILLS.map(|(name, _)| name));
        for (name, files) in SKILLS {
            let mut on_disk: Vec<_> = std::fs::read_dir(repo.join(name))
                .unwrap()
                .map(|e| e.unwrap().file_name().into_string().unwrap())
                .collect();
            on_disk.sort();
            let mut embedded: Vec<_> = files.iter().map(|(file, _)| *file).collect();
            embedded.sort();
            assert_eq!(on_disk, embedded, "{name}");
            for (file, _) in files {
                let installed = std::fs::read_to_string(
                    root.path().join(format!(".claude/skills/{name}/{file}")),
                )
                .unwrap();
                let expected = std::fs::read_to_string(repo.join(name).join(file)).unwrap();
                assert_eq!(installed, expected, "{name}/{file}");
            }
            let skill = std::fs::read_to_string(repo.join(name).join("SKILL.md")).unwrap();
            assert!(skill.starts_with(&format!("---\nname: {name}\n")));
        }
        assert!(!root.path().join(".agents").exists());
    }
}
