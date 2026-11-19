mod common;
use common::*;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::Output;

/// A private user cache directory and home for one test.
struct Env {
    _dir: tempfile::TempDir,
    cache_home: PathBuf,
    home: PathBuf,
}

impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let (cache_home, home) = (root.join("xdg"), root.join("home"));
        std::fs::create_dir_all(&home).unwrap();
        Self {
            _dir: dir,
            cache_home,
            home,
        }
    }

    fn fresh_home(&self, name: &str) -> PathBuf {
        let home = self.home.parent().unwrap().join(name);
        std::fs::create_dir_all(&home).unwrap();
        home
    }

    fn run_in(&self, p: &Project, home: &Path, args: &[&str], vars: &[(&str, &str)]) -> Output {
        let mut command = p.command();
        command
            .env("XDG_CACHE_HOME", &self.cache_home)
            .env("HOME", home)
            .env_remove("GOODFLOWS_CACHE_KEY")
            .env_remove("GITHUB_SHA")
            .args(args);
        for (key, value) in vars {
            command.env(key, value);
        }
        command.output().unwrap()
    }

    fn run(&self, p: &Project, args: &[&str]) -> Output {
        self.run_in(p, &self.home, args, &[])
    }

    /// Every stored archive in every project store.
    fn entries(&self) -> Vec<PathBuf> {
        files_under(&self.cache_home)
            .into_iter()
            .filter(|f| f.extension().is_some_and(|e| e == "tgz"))
            .collect()
    }
}

fn git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .current_dir(dir)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
}

/// A project in a Git checkout whose origin is `remote`, so two checkouts
/// with the same remote share one local store.
fn checkout(config: &str, remote: &str) -> Project {
    let p = Project::new(config);
    git(&p.dir, &["init", "-q"]);
    git(&p.dir, &["remote", "add", "origin", remote]);
    p
}

fn of_type(output: &Output, kind: &str) -> Vec<Value> {
    events(output)
        .into_iter()
        .filter(|e| e["type"] == kind)
        .collect()
}

fn restored(output: &Output) -> Value {
    let events = of_type(output, "cache_restored");
    assert_eq!(events.len(), 1, "{}", stdout(output));
    events[0].clone()
}

fn result_of(event: &Value) -> (&str, Value) {
    (event["result"].as_str().unwrap(), event["tier"].clone())
}

const DEPS: &str = r#"
[[jobs]]
name = "deps"
command = "test -d deps || { mkdir -p deps && cat mix.lock > deps/built; }"
[[jobs.caches]]
name = "deps"
paths = ["deps"]
key.files = ["mix.lock"]
"#;

#[test]
fn a_miss_saves_and_a_fresh_copy_restores_the_exact_key() {
    let env = Env::new();
    let a = checkout(DEPS, "git@example.com:org/app.git");
    std::fs::write(a.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&a, &["--json"]);
    success(&output);
    let event = restored(&output);
    assert_eq!(result_of(&event), ("miss", Value::Null));
    assert_eq!(event["job"], "deps");
    assert_eq!(event["cache"], "deps");
    let saved = of_type(&output, "cache_saved");
    assert_eq!(saved.len(), 1, "{}", stdout(&output));
    assert_eq!(saved[0]["tiers"], json!(["local"]));
    assert_eq!(env.entries().len(), 1);
    // The store is private.
    use std::os::unix::fs::PermissionsExt;
    let store = env.entries()[0].parent().unwrap().to_path_buf();
    let mode = std::fs::metadata(&store).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o700);

    // Another checkout of the same repository, by a different remote spelling.
    let b = checkout(DEPS, "https://example.com/org/app");
    std::fs::write(b.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&b, &["--json"]);
    success(&output);
    let event = restored(&output);
    assert_eq!(result_of(&event), ("exact", json!("local")));
    assert!(event["bytes"].as_u64().unwrap() > 0);
    assert_eq!(b.read("deps/built"), "v1");
    let skipped = of_type(&output, "cache_save_skipped");
    assert_eq!(skipped[0]["reason"], "hit");
    assert!(of_type(&output, "cache_saved").is_empty());

    // Text mode prints one line per restore and save.
    let c = checkout(DEPS, "https://example.com/org/app.git");
    std::fs::write(c.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&c, &[]);
    success(&output);
    let text = stdout(&output);
    assert!(
        text.contains("deps cache deps: restored (exact, local,"),
        "{text}"
    );
    assert!(text.contains("deps cache deps: not saved (hit)"), "{text}");
}

#[test]
fn two_worktrees_without_a_remote_share_entries() {
    let env = Env::new();
    let main = Project::new(DEPS);
    std::fs::write(main.dir.join("mix.lock"), "v1").unwrap();
    git(&main.dir, &["init", "-q"]);
    git(&main.dir, &["add", "goodflows.toml", "mix.lock"]);
    git(&main.dir, &["commit", "-qm", "init"]);
    success(&env.run(&main, &[]));
    let second = Project::new("");
    std::fs::remove_dir_all(&second.dir).unwrap();
    git(
        &main.dir,
        &[
            "worktree",
            "add",
            "-q",
            "--detach",
            second.dir.to_str().unwrap(),
        ],
    );
    let output = env.run(&second, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("exact", json!("local")));
    assert_eq!(second.read("deps/built"), "v1");
}

#[test]
fn a_fallback_fills_a_missing_path_and_an_existing_tree_is_present() {
    let env = Env::new();
    let p = checkout(DEPS, "https://example.com/org/fallback");
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    success(&env.run(&p, &[]));
    std::fs::remove_dir_all(p.dir.join("deps")).unwrap();
    std::fs::write(p.dir.join("mix.lock"), "v2").unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("fallback", json!("local")));
    // The fallback's tree is kept, so the job did not rebuild it.
    assert_eq!(p.read("deps/built"), "v1");
    // The job passed after a fallback, so the new key is saved.
    assert_eq!(of_type(&output, "cache_saved").len(), 1);

    // An existing tree with only a fallback stored stays as it is.
    std::fs::write(p.dir.join("mix.lock"), "v3").unwrap();
    std::fs::write(p.dir.join("deps/mine"), "edited").unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("present", json!("workdir")));
    assert_eq!(p.read("deps/mine"), "edited");
}

#[test]
fn a_rerun_extracts_nothing_and_a_changed_lockfile_saves_a_new_key() {
    let env = Env::new();
    let p = checkout(DEPS, "https://example.com/org/rerun");
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    success(&env.run(&p, &[]));
    std::fs::write(p.dir.join("deps/local-edit"), "kept").unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    let event = restored(&output);
    assert_eq!(result_of(&event), ("exact", json!("workdir")));
    assert_eq!(event["bytes"], 0);
    assert_eq!(p.read("deps/local-edit"), "kept");
    assert_eq!(of_type(&output, "cache_save_skipped")[0]["reason"], "hit");

    std::fs::write(p.dir.join("mix.lock"), "v2").unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("present", json!("workdir")));
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
    assert_eq!(env.entries().len(), 2);
    // The stamp now matches, so the next run is an exact workdir hit.
    let output = env.run(&p, &["--json"]);
    assert_eq!(result_of(&restored(&output)), ("exact", json!("workdir")));
}

#[test]
fn an_existing_key_is_not_archived_again() {
    let env = Env::new();
    let a = checkout(DEPS, "https://example.com/org/exists");
    std::fs::write(a.dir.join("mix.lock"), "v1").unwrap();
    success(&env.run(&a, &[]));
    // A present tree with no stamp whose key is already stored.
    let b = checkout(DEPS, "https://example.com/org/exists");
    std::fs::write(b.dir.join("mix.lock"), "v1").unwrap();
    std::fs::create_dir(b.dir.join("deps")).unwrap();
    let output = env.run(&b, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("present", json!("workdir")));
    assert_eq!(
        of_type(&output, "cache_save_skipped")[0]["reason"],
        "exists"
    );
}

fn failing(save: &str, command: &str, extra: &str) -> String {
    format!(
        "[[jobs]]\nname = \"deps\"\ncommand = \"{command}\"\n{extra}[[jobs.caches]]\nname = \"deps\"\npaths = [\"deps\"]\nsave = \"{save}\"\n"
    )
}

#[test]
fn save_always_keeps_a_failed_jobs_cache_but_never_a_timed_out_one() {
    let env = Env::new();
    let p = checkout(
        &failing("always", "mkdir -p deps && exit 1", ""),
        "https://example.com/org/always",
    );
    let output = env.run(&p, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(of_type(&output, "cache_saved").len(), 1);

    let env = Env::new();
    let p = checkout(
        &failing("passed", "mkdir -p deps && exit 1", ""),
        "https://example.com/org/passed",
    );
    let output = env.run(&p, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        of_type(&output, "cache_save_skipped")[0]["reason"],
        "not_passed"
    );
    assert!(env.entries().is_empty());

    let p = checkout(
        &failing("always", "mkdir -p deps && sleep 5", "timeout_secs = 1\n"),
        "https://example.com/org/timeout",
    );
    let output = env.run(&p, &["--json"]);
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        of_type(&output, "cache_save_skipped")[0]["reason"],
        "not_passed"
    );
    assert!(env.entries().is_empty());
    assert!(!p.dir.join(".goodflows/cache-stamps/deps").exists());
}

#[test]
fn a_cancelled_job_saves_nothing() {
    let env = Env::new();
    let p = checkout(
        &failing("always", "mkdir -p deps && touch started && sleep 30", ""),
        "https://example.com/org/cancel",
    );
    let mut command = p.command();
    command
        .env("XDG_CACHE_HOME", &env.cache_home)
        .env("HOME", &env.home)
        .arg("--json")
        .stdout(std::process::Stdio::piped());
    let child = command.spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while !p.dir.join("started").exists() {
        assert!(
            std::time::Instant::now() < deadline,
            "the job never started"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(unsafe { libc::kill(child.id() as i32, libc::SIGINT) }, 0);
    let output = child.wait_with_output().unwrap();
    assert_eq!(output.status.code(), Some(130));
    assert_eq!(
        of_type(&output, "cache_save_skipped")[0]["reason"],
        "not_passed"
    );
    assert!(env.entries().is_empty());
}

#[test]
fn a_read_only_profile_restores_but_never_saves() {
    let env = Env::new();
    let config = format!("{DEPS}[environments.pr.cache]\nwrite = false\n");
    let p = checkout(&config, "https://example.com/org/readonly");
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&p, &["--json", "--env", "pr"]);
    success(&output);
    assert_eq!(
        of_type(&output, "cache_save_skipped")[0]["reason"],
        "read_only"
    );
    assert!(env.entries().is_empty());
    // The stamp still records the key the tree was built for.
    let output = env.run(&p, &["--json", "--env", "pr"]);
    assert_eq!(result_of(&restored(&output)), ("exact", json!("workdir")));
}

#[test]
fn partitions_saving_one_key_leave_one_entry() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "mkdir -p deps && if [ \"$GOODFLOWS_PARTITION\" = 2 ]; then sleep 1; echo x > deps/late; fi; echo x > deps/f-$GOODFLOWS_PARTITION"
partitions = 2
[[jobs.caches]]
name = "deps"
paths = ["deps"]
"#,
        "https://example.com/org/partitions",
    );
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert_eq!(env.entries().len(), 1);
    // Instance 1 ends a second before instance 2 finishes writing `deps`;
    // its save waits, so the one entry holds the finished tree.
    assert_eq!(
        archived(&env.entries()[0]),
        [
            "workdir/deps",
            "workdir/deps/f-1",
            "workdir/deps/f-2",
            "workdir/deps/late"
        ]
    );
    assert!(
        of_type(&output, "cache_warning").is_empty(),
        "{}",
        stdout(&output)
    );
    // Each instance either wrote the entry or found it already stored.
    let saved = of_type(&output, "cache_saved");
    let skipped = of_type(&output, "cache_save_skipped");
    assert_eq!(saved.len() + skipped.len(), 2, "{}", stdout(&output));
    assert!(!saved.is_empty());
    assert!(skipped.iter().all(|e| e["reason"] == "exists"));
    let mut partitions: Vec<_> = saved
        .iter()
        .chain(&skipped)
        .map(|e| e["partition"]["index"].as_u64().unwrap())
        .collect();
    partitions.sort();
    assert_eq!(partitions, [1, 2]);
}

/// The paths an entry holds, sorted, without the header.
fn archived(entry: &Path) -> Vec<String> {
    let output = std::process::Command::new("tar")
        .arg("tzf")
        .arg(entry)
        .output()
        .unwrap();
    assert!(output.status.success(), "{}", stderr(&output));
    let mut names: Vec<_> = stdout(&output)
        .lines()
        .filter(|l| l.starts_with("workdir/"))
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

#[test]
fn a_save_never_archives_while_a_dependent_writes_the_same_paths() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "mkdir -p _build && echo x > _build/deps"
[[jobs.caches]]
name = "build"
paths = ["_build"]
[[jobs]]
name = "test"
depends_on = ["deps"]
command = "sleep 1; echo x > _build/test"
"#,
        "https://example.com/org/dependent",
    );
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert!(
        of_type(&output, "cache_warning").is_empty(),
        "{}",
        stdout(&output)
    );
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
    // The dependent started right away and the save waited for it.
    assert_eq!(
        archived(&env.entries()[0]),
        [
            "workdir/_build",
            "workdir/_build/deps",
            "workdir/_build/test"
        ]
    );
}

#[test]
fn a_step_is_skipped_on_an_exact_hit() {
    let env = Env::new();
    let config = r#"
[[jobs]]
name = "build"
steps = [
  { id = "fetch", command = "mkdir -p deps && touch deps/f && touch fetched", condition = { expression = "!caches.deps.hit" } },
  { id = "test", command = "test -f deps/f" },
]
outputs = { hit = "caches.deps.hit" }
[[jobs.caches]]
name = "deps"
paths = ["deps"]
"#;
    let a = checkout(config, "https://example.com/org/steps");
    let output = env.run(&a, &["--json"]);
    success(&output);
    assert!(a.dir.join("fetched").exists());
    let b = checkout(config, "https://example.com/org/steps");
    let output = env.run(&b, &["--json"]);
    success(&output);
    assert!(!b.dir.join("fetched").exists());
    let finished = of_type(&output, "job_finished");
    assert_eq!(finished[0]["result"]["outputs"]["hit"], true);
    let steps = of_type(&output, "step_finished");
    assert_eq!(steps[0]["result"]["status"], "skipped");
    assert_eq!(steps[1]["result"]["status"], "passed");
}

#[test]
fn a_home_path_is_restored_into_a_fresh_home_and_kept_when_present() {
    let env = Env::new();
    let config = r#"
[[jobs]]
name = "tools"
command = "test -d $HOME/.toolcache || { mkdir -p $HOME/.toolcache/pkg && echo built > $HOME/.toolcache/pkg/f; }"
[[jobs.caches]]
name = "tools"
paths = ["~/.toolcache"]
"#;
    let a = checkout(config, "https://example.com/org/home");
    success(&env.run(&a, &[]));
    let b = checkout(config, "https://example.com/org/home");
    let home = env.fresh_home("home2");
    let output = env.run_in(&b, &home, &["--json"], &[]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("exact", json!("local")));
    assert_eq!(
        std::fs::read_to_string(home.join(".toolcache/pkg/f")).unwrap(),
        "built\n"
    );
    std::fs::write(home.join(".toolcache/pkg/f"), "mine").unwrap();
    let c = checkout(config, "https://example.com/org/home");
    let output = env.run_in(&c, &home, &["--json"], &[]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("present", json!("workdir")));
    assert_eq!(
        std::fs::read_to_string(home.join(".toolcache/pkg/f")).unwrap(),
        "mine"
    );
    // No staging directory is left in the home directory.
    let names: Vec<_> = std::fs::read_dir(&home)
        .unwrap()
        .flatten()
        .map(|e| e.file_name())
        .collect();
    assert_eq!(names, [std::ffi::OsString::from(".toolcache")]);
}

#[test]
fn a_rolling_cache_restores_the_previous_entry_and_saves_a_new_one() {
    let env = Env::new();
    let config = r#"
[[jobs]]
name = "build"
command = "mkdir -p ccache && echo $GITHUB_SHA >> ccache/log"
[[jobs.caches]]
name = "ccache"
paths = ["ccache"]
key.unique = ["env.GITHUB_SHA"]
"#;
    let a = checkout(config, "https://example.com/org/rolling");
    let output = env.run_in(&a, &env.home, &["--json"], &[("GITHUB_SHA", "one")]);
    success(&output);
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
    let b = checkout(config, "https://example.com/org/rolling");
    let output = env.run_in(&b, &env.home, &["--json"], &[("GITHUB_SHA", "two")]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("fallback", json!("local")));
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
    assert_eq!(b.read("ccache/log"), "one\ntwo\n");
    assert_eq!(env.entries().len(), 2);
}

#[test]
fn a_corrupt_entry_leaves_the_job_passing_with_a_warning() {
    let env = Env::new();
    let a = checkout(DEPS, "https://example.com/org/corrupt");
    std::fs::write(a.dir.join("mix.lock"), "v1").unwrap();
    success(&env.run(&a, &[]));
    let entry = env.entries().pop().unwrap();
    std::fs::write(&entry, b"not a gzip archive").unwrap();
    let b = checkout(DEPS, "https://example.com/org/corrupt");
    std::fs::write(b.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&b, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("miss", Value::Null));
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"].as_str().unwrap().contains("sha256"));
    // The job rebuilt deps and saved a good entry in place of the bad one.
    assert_eq!(b.read("deps/built"), "v1");
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
}

#[test]
fn a_failed_save_never_changes_the_result() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "mkdir -p deps && mkfifo deps/pipe"
[[jobs.caches]]
name = "deps"
paths = ["deps"]
"#,
        "https://example.com/org/special",
    );
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert!(of_type(&output, "cache_warning")[0]["message"]
        .as_str()
        .unwrap()
        .contains("special file"));
    assert_eq!(of_type(&output, "cache_save_skipped")[0]["reason"], "error");
}

#[test]
fn a_bad_key_value_misses_with_a_warning() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "true"
[[jobs.caches]]
name = "deps"
paths = ["deps"]
key.values = ["env.OTP_VERSION < 26"]
"#,
        "https://example.com/org/badvalue",
    );
    let output = env.run_in(&p, &env.home, &["--json"], &[("OTP_VERSION", "26.1")]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("miss", Value::Null));
    assert_eq!(of_type(&output, "cache_warning").len(), 1);
    assert_eq!(of_type(&output, "cache_save_skipped")[0]["reason"], "error");
}

#[test]
fn a_skipped_job_neither_restores_nor_saves() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "mkdir deps"
condition = { expression = "false" }
[[jobs.caches]]
name = "deps"
paths = ["deps"]
[[jobs]]
name = "fails"
command = "exit 1"
[[jobs]]
name = "after"
command = "mkdir deps"
depends_on = ["fails"]
[[jobs.caches]]
name = "after"
paths = ["deps"]
"#,
        "https://example.com/org/skipped",
    );
    let output = env.run(&p, &["--json", "--no-fail-fast"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(stdout(&output).lines().all(|l| !l.contains("\"cache_")));
    assert!(!env.cache_home.exists() || env.entries().is_empty());
}

#[test]
fn summary_output_and_history_include_cache_results() {
    let env = Env::new();
    let p = checkout(DEPS, "https://example.com/org/summary");
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&p, &["--json=summary", "--history"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("miss", Value::Null));
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
    let summary = of_type(&output, "summary").pop().unwrap();
    let caches = &summary["results"][0]["caches"];
    assert_eq!(
        caches,
        &json!([{"name": "deps", "result": "miss", "tier": null, "saved": ["local"]}])
    );
    let history: Value = serde_json::from_str(
        &std::fs::read_to_string(summary["history_file"].as_str().unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(&history["results"][0]["caches"], caches);
}

#[test]
fn outside_git_the_store_is_keyed_by_the_config_directory() {
    let env = Env::new();
    let p = Project::new(DEPS);
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    success(&env.run(&p, &[]));
    std::fs::remove_dir_all(p.dir.join("deps")).unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert_eq!(result_of(&restored(&output)), ("exact", json!("local")));
}

#[test]
fn local_false_without_a_remote_writes_nothing_under_the_cache_directory() {
    let env = Env::new();
    let config = format!("{DEPS}[cache]\nlocal = false\n");
    let p = checkout(&config, "https://example.com/org/nolocal");
    std::fs::write(p.dir.join("mix.lock"), "v1").unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert!(!env.cache_home.exists());
    assert!(of_type(&output, "cache_saved").is_empty());
    assert_eq!(of_type(&output, "cache_save_skipped")[0]["reason"], "error");
    // One warning names the cause.
    let warnings = of_type(&output, "cache_warning");
    assert_eq!(warnings.len(), 1, "{}", stdout(&output));
    assert!(warnings[0]["message"]
        .as_str()
        .unwrap()
        .contains("local = false"));
}

#[test]
fn hashing_key_files_is_bounded_by_the_caches_timeout() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "touch ran"
[[jobs.caches]]
name = "deps"
paths = ["deps"]
key.files = ["big"]
timeout_secs = 1
"#,
        "https://example.com/org/hashing",
    );
    // Sparse: minutes of hashing, no disk.
    let big = std::fs::File::create(p.dir.join("big")).unwrap();
    big.set_len(200 << 30).unwrap();
    let started = std::time::Instant::now();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(15),
        "{:?}",
        started.elapsed()
    );
    assert!(p.dir.join("ran").exists());
    let restored = of_type(&output, "cache_restored");
    assert_eq!(restored[0]["result"], "miss");
    let warning = &of_type(&output, "cache_warning")[0]["message"];
    assert!(
        warning.as_str().unwrap().contains("timeout_secs"),
        "{warning}"
    );
}

#[test]
fn a_save_waiting_for_a_quiet_run_frees_its_slot() {
    let env = Env::new();
    let p = checkout(
        r#"
[[jobs]]
name = "deps"
command = "mkdir -p deps && echo x > deps/f"
[[jobs.caches]]
name = "deps"
paths = ["deps"]
[[jobs]]
name = "lint"
command = "sleep 4; date +%s%N > lint-end"
[[jobs]]
name = "test"
depends_on = ["deps"]
command = "date +%s%N > test-start; sleep 1"
"#,
        "https://example.com/org/slots",
    );
    let output = env.run(&p, &["--json", "-p", "2"]);
    success(&output);
    let read = |name: &str| p.read(name).trim().parse::<u128>().unwrap();
    // deps's save waits for lint, but test gets deps's slot meanwhile.
    let ahead = read("lint-end").saturating_sub(read("test-start")) / 1_000_000;
    assert!(
        ahead >= 2_500,
        "test started only {ahead} ms before lint ended"
    );
    assert!(
        of_type(&output, "cache_warning").is_empty(),
        "{}",
        stdout(&output)
    );
    assert_eq!(of_type(&output, "cache_saved").len(), 1);
}

#[test]
fn a_save_in_a_called_workflow_does_not_hold_back_the_calls_dependents() {
    let env = Env::new();
    let p = checkout(
        r#"
[[workflows.ci.jobs]]
name = "lint"
command = "sleep 4; date +%s%N > lint-end"
[[workflows.ci.jobs]]
name = "call"
workflow = "child"
[[workflows.ci.jobs]]
name = "after"
depends_on = ["call"]
command = "date +%s%N > after-start"
[[workflows.child.jobs]]
name = "x"
command = "mkdir -p x && echo x > x/f"
[[workflows.child.jobs.caches]]
name = "x"
paths = ["x"]
"#,
        "https://example.com/org/called",
    );
    let output = env.run(&p, &["run", "ci", "--json", "-p", "3"]);
    success(&output);
    let read = |name: &str| p.read(name).trim().parse::<u128>().unwrap();
    let ahead = read("lint-end").saturating_sub(read("after-start")) / 1_000_000;
    assert!(
        ahead >= 2_500,
        "after started only {ahead} ms before lint ended"
    );
    // The child's save still finished, and its summary and the run's list it.
    let saved = of_type(&output, "cache_saved");
    assert_eq!(saved.len(), 1, "{}", stdout(&output));
    assert_eq!(saved[0]["job"], "x");
    assert_eq!(env.entries().len(), 1);
    let summaries = of_type(&output, "summary");
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0]["workflow"], "child");
    assert_eq!(
        summaries[0]["results"][0]["caches"][0]["saved"],
        json!(["local"])
    );
    assert_eq!(summaries[1]["workflow"], "ci");
    assert_eq!(summaries[1]["outcome"], "passed", "{}", summaries[1]);
}

#[test]
fn a_stale_aside_is_removed_before_the_job_runs() {
    use sha2::{Digest, Sha256};
    let tag = |text: &[u8]| {
        Sha256::digest(text)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()[..8]
            .to_owned()
    };
    let mut host = [0u8; 256];
    // SAFETY: a valid buffer of its length.
    assert_eq!(
        unsafe { libc::gethostname(host.as_mut_ptr().cast(), host.len()) },
        0
    );
    let host = &host[..host.iter().position(|b| *b == 0).unwrap()];
    let boot = std::fs::read("/proc/sys/kernel/random/boot_id").unwrap_or_default();
    let mut child = std::process::Command::new("true").spawn().unwrap();
    let dead = child.id();
    child.wait().unwrap();
    let aside = format!(
        ".deps.goodflows-aside-{}-{}-{dead}-0123abcd",
        tag(host),
        tag(boot.trim_ascii())
    );
    let env = Env::new();
    let p = checkout(
        &format!(
            "[[jobs]]\nname = 'deps'\ncommand = 'test ! -e {aside} && mkdir -p deps'\n\
             [[jobs.caches]]\nname = 'deps'\npaths = ['deps']\n"
        ),
        "https://example.com/org/aside",
    );
    std::fs::create_dir_all(p.dir.join(&aside).join("old")).unwrap();
    let output = env.run(&p, &["--json"]);
    success(&output);
    assert!(!p.dir.join(&aside).exists());
}
