//! In-Sprite Tasks API wake holds over `/.sprite/api.sock` (virtual host
//! `sprite`). A hold prevents sleep; it does not supervise processes, and its
//! expiry is never evidence that a worker died.

use anyhow::{bail, Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_SOCKET: &str = "/.sprite/api.sock";

/// The Tasks API socket. Only debug builds honour a test override.
pub fn socket() -> PathBuf {
    super::testhook::api_socket().unwrap_or_else(|| PathBuf::from(DEFAULT_SOCKET))
}

/// The Tasks API name for one run.
pub fn task_name(run_id: &str) -> String {
    format!("gf-{run_id}")
}

fn request(method: &str, path: &str, body: Option<&str>) -> Result<u16> {
    if super::testhook::api_fault() {
        bail!("injected Tasks API failure (debug build fault hook)");
    }
    let mut stream = UnixStream::connect(socket()).context("connect to the Sprite Tasks API")?;
    stream.set_read_timeout(Some(Duration::from_secs(3)))?;
    stream.set_write_timeout(Some(Duration::from_secs(3)))?;
    let body = body.unwrap_or("");
    write!(
        stream,
        "{method} {path} HTTP/1.1\r\nHost: sprite\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    )?;
    let mut response = Vec::new();
    stream
        .take(64 * 1024)
        .read_to_end(&mut response)
        .context("read Tasks API response")?;
    let line = response
        .split(|b| *b == b'\n')
        .next()
        .context("empty Tasks API response")?;
    let status = std::str::from_utf8(line)
        .ok()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .context("malformed Tasks API response")?;
    Ok(status)
}

/// Create or refresh the run's hold (`PUT /v1/tasks/<name>`).
pub fn renew(run_id: &str, expire_secs: u64) -> Result<()> {
    let status = request(
        "PUT",
        &format!("/v1/tasks/{}", task_name(run_id)),
        Some(&format!("{{\"expire\":\"{expire_secs}s\"}}")),
    )?;
    if !(200..300).contains(&status) {
        bail!("Tasks API hold renewal returned HTTP {status}");
    }
    Ok(())
}

/// Release the run's hold. Called only after terminal state is durable; no
/// required write depends on its response.
pub fn release(run_id: &str) -> Result<()> {
    let status = request("DELETE", &format!("/v1/tasks/{}", task_name(run_id)), None)?;
    if !(200..300).contains(&status) && status != 404 {
        bail!("Tasks API hold release returned HTTP {status}");
    }
    Ok(())
}
