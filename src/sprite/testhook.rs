//! Deterministic fault injection for the Sprite contract tests.
//!
//! Compiled only into debug builds: release binaries contain no hook and read
//! none of these variables. With `GOODFLOWS_TEST_PAUSE_DIR` set (or, for a
//! debug runner on a real Sprite, `$HOME/.goodflows-test-pause`), a process
//! reaching point `P` claims `<dir>/P.pause` by renaming it to `P.reached`
//! and waits (at most two minutes) until the test removes `P.reached`. Only
//! one process claims each armed point.

/// The armed fault directory, if any (debug builds only).
#[cfg(debug_assertions)]
fn fault_dir() -> Option<std::path::PathBuf> {
    // On a real Sprite the supervisor service and its workers do not inherit
    // exec environment, so a debug runner also honours `$HOME/.goodflows-test-pause`.
    let dir = match std::env::var_os("GOODFLOWS_TEST_PAUSE_DIR") {
        Some(dir) => std::path::PathBuf::from(dir),
        None => std::path::PathBuf::from(std::env::var_os("HOME")?).join(".goodflows-test-pause"),
    };
    dir.is_dir().then_some(dir)
}

#[cfg(debug_assertions)]
pub fn pause(point: &str) {
    use std::time::{Duration, Instant};
    let Some(dir) = fault_dir() else {
        return;
    };
    let reached = dir.join(format!("{point}.reached"));
    if std::fs::rename(dir.join(format!("{point}.pause")), &reached).is_err() {
        return;
    }
    // The paused process's PID, so a test can kill exactly that process.
    let _ = std::fs::write(&reached, std::process::id().to_string());
    let deadline = Instant::now() + Duration::from_secs(120);
    while reached.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Injected Tasks API failure while `<fault dir>/api.fail` exists.
#[cfg(debug_assertions)]
pub fn api_fault() -> bool {
    fault_dir().is_some_and(|dir| dir.join("api.fail").exists())
}

#[cfg(not(debug_assertions))]
pub fn api_fault() -> bool {
    false
}

/// Injected capability-drop failure for the workflow runner launch while
/// `<fault dir>/caps.fail` exists.
#[cfg(debug_assertions)]
pub fn caps_fault() -> bool {
    fault_dir().is_some_and(|dir| dir.join("caps.fail").exists())
}

#[cfg(not(debug_assertions))]
pub fn caps_fault() -> bool {
    false
}

/// Injected error at point `name` while `<fault dir>/<name>.fail` exists.
#[cfg(debug_assertions)]
pub fn fault(name: &str) -> bool {
    fault_dir().is_some_and(|dir| dir.join(format!("{name}.fail")).exists())
}

#[cfg(not(debug_assertions))]
pub fn fault(_name: &str) -> bool {
    false
}

#[cfg(not(debug_assertions))]
pub fn pause(_point: &str) {}

/// An alternate in-Sprite Tasks API socket for the contract tests'
/// fake API (debug builds only: `GOODFLOWS_SPRITE_API_SOCKET`). Release
/// builds always use the platform socket.
#[cfg(debug_assertions)]
pub fn api_socket() -> Option<std::path::PathBuf> {
    std::env::var_os("GOODFLOWS_SPRITE_API_SOCKET").map(std::path::PathBuf::from)
}

#[cfg(not(debug_assertions))]
pub fn api_socket() -> Option<std::path::PathBuf> {
    None
}

/// Release download base for runner provisioning tests (debug builds only).
#[cfg(debug_assertions)]
pub fn release_base() -> Option<String> {
    std::env::var("GOODFLOWS_TEST_RELEASE_URL").ok()
}

#[cfg(not(debug_assertions))]
pub fn release_base() -> Option<String> {
    None
}

/// How long a non-creating controller waits for another creator's ownership
/// initialization. Tests may shorten it (debug builds only).
pub fn owner_wait() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_OWNER_WAIT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return std::time::Duration::from_secs(secs);
    }
    std::time::Duration::from_secs(60)
}

/// How long an attached controller keeps reconnecting its follow stream
/// without new output before it reports the run as unresolved. Tests may
/// change it (debug builds only).
pub fn follow_budget() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_FOLLOW_BUDGET_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return std::time::Duration::from_secs(secs);
    }
    std::time::Duration::from_secs(300)
}

/// How long an attached controller's follow connection may deliver nothing
/// before it is dropped and reconnected (debug builds:
/// `GOODFLOWS_TEST_FOLLOW_IDLE_SECS`).
pub fn follow_idle() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_FOLLOW_IDLE_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return std::time::Duration::from_secs(secs);
    }
    std::time::Duration::from_secs(120)
}

/// How long a destroy keeps re-listing the Sprite after its delete call
/// before deciding (debug builds: `GOODFLOWS_TEST_DESTROY_POLL_SECS`).
pub fn destroy_poll() -> std::time::Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_DESTROY_POLL_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return std::time::Duration::from_secs(secs);
    }
    std::time::Duration::from_secs(60)
}
