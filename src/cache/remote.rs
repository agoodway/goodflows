//! The remote cache store on a GoodFlows server (goodflows_web's
//! `add-remote-cache-storage`): lookups, presigned downloads and uploads,
//! and minting per-run `ck_` keys for Sprite runs.
//!
//! Responses are untrusted: bodies are bounded and checked for the
//! documented shape, and diagnostics name HTTP statuses only, never bodies,
//! URLs or keys. Presigned requests never carry `Authorization`.

use super::{archive, key::valid_digest, Events};
use crate::reporting::publish::Target;
use anyhow::{anyhow, bail, Context, Result};
use reqwest::{header, redirect, Method, StatusCode, Url};
use serde_json::{json, Value};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use std::time::Duration;

const MAX_RESPONSE_BYTES: usize = 1024 * 1024;
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// One API request, including its body.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A committed entry from a lookup.
#[derive(Debug, Clone)]
pub struct Entry {
    pub key: String,
    pub partial: String,
    pub size: u64,
    pub sha256: String,
    pub download_url: Url,
}

/// What a remote save did.
#[derive(Debug, PartialEq, Eq)]
pub enum Saved {
    Stored,
    /// Not saved, for this `cache_save_skipped` reason.
    Skipped(&'static str),
    /// Not saved; the message is reported as a `cache_warning`.
    Failed(String),
    /// The remote store is off for this run.
    Unavailable,
}

#[derive(Default)]
struct State {
    disabled: bool,
    read_only: bool,
    /// Lookups no longer send a scope after a `403` on a scoped lookup.
    scope_dropped: bool,
}

pub struct Remote {
    target: Target,
    client: reqwest::Client,
    /// The run's scope, `None` when it has none.
    scope: Option<String>,
    state: Mutex<State>,
}

/// One API response: its status and, for a success, the JSON body.
enum Reply {
    Status(StatusCode, Option<Value>),
    /// A connection failure or a timeout.
    Down,
}

/// A URL a presigned request may use: https, or http on loopback.
fn presigned(value: &Value) -> Option<Url> {
    let url = Url::parse(value.as_str()?).ok()?;
    match url.scheme() {
        "https" => Some(url),
        "http" if loopback(&url) => Some(url),
        _ => None,
    }
}

fn loopback(url: &Url) -> bool {
    url.host_str().is_some_and(|h| {
        h == "localhost"
            || h.trim_matches(['[', ']'])
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    })
}

/// Whether a presigned request follows a redirect to `next`: at most five,
/// and only to the first URL's host and scheme, so never down to http.
fn follows(previous: &[Url], next: &Url) -> bool {
    let first = previous.first();
    previous.len() < 5
        && first.and_then(Url::host_str) == next.host_str()
        && first.map(Url::scheme) == Some(next.scheme())
}

/// A client for presigned URLs: redirects only within the same host, and
/// no proxy for loopback targets.
fn presigned_client(url: &Url) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        // A stalled transfer ends; the cache's own bound covers the rest.
        .read_timeout(REQUEST_TIMEOUT)
        .redirect(redirect::Policy::custom(|attempt| {
            if follows(attempt.previous(), attempt.url()) {
                attempt.follow()
            } else {
                attempt.stop()
            }
        }));
    if loopback(url) {
        builder = builder.no_proxy();
    }
    Ok(builder.build()?)
}

/// How often a transfer waiting on the network checks its abort flag.
const ABORT_POLL: Duration = Duration::from_millis(100);

/// Await `work`, giving up with [`archive::Aborted`] soon after `abort` is
/// set, so a stalled transfer ends at the cache's `timeout_secs`.
async fn abortable<T>(abort: &AtomicBool, work: impl std::future::Future<Output = T>) -> Result<T> {
    tokio::pin!(work);
    loop {
        if abort.load(Ordering::Relaxed) {
            return Err(archive::Aborted.into());
        }
        if let Ok(value) = tokio::time::timeout(ABORT_POLL, &mut work).await {
            return Ok(value);
        }
    }
}

/// A lookup hit's `data`, checked against the requested key or partial.
fn entry(body: &Value, key: Option<&str>, partial: Option<&str>) -> Option<Entry> {
    let data = body.get("data")?;
    let text = |name: &str| data.get(name)?.as_str().map(str::to_owned);
    let entry = Entry {
        key: text("key").filter(|k| valid_digest(k))?,
        partial: text("partial").filter(|p| valid_digest(p))?,
        size: data
            .get("size")?
            .as_u64()
            .filter(|s| (1..=archive::MAX_ENTRY_BYTES).contains(s))?,
        sha256: text("sha256").filter(|s| valid_digest(s))?,
        download_url: presigned(data.get("download_url")?)?,
    };
    if key.is_some_and(|k| k != entry.key) || partial.is_some_and(|p| p != entry.partial) {
        return None;
    }
    Some(entry)
}

impl Remote {
    pub fn new(target: Target, scope: Option<String>) -> Result<Self> {
        let client = target.client(CONNECT_TIMEOUT)?;
        Ok(Self {
            target,
            client,
            scope,
            state: Mutex::new(State::default()),
        })
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn disabled(&self) -> bool {
        self.state().disabled
    }

    /// The `cache_save_skipped` reason [`Remote::save`] would return before
    /// any request, so a caller can skip building the archive.
    pub fn refusal(&self) -> Option<&'static str> {
        if self.state().read_only {
            Some("read_only")
        } else if self.scope.is_none() {
            Some("no_scope")
        } else {
            None
        }
    }

    /// Turn the remote store off for the run, with one event.
    async fn disable(&self, events: &Events<'_>, reason: &str) {
        let first = {
            let mut state = self.state();
            !std::mem::replace(&mut state.disabled, true)
        };
        if first {
            events.remote_unavailable(reason).await;
        }
    }

    fn path(&self, rest: &str) -> String {
        format!("/api/v1/projects/{}/caches/{rest}", self.target.project())
    }

    async fn send(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
    ) -> Reply {
        let mut url = self.target.api(path);
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        let mut request = self
            .client
            .request(method, url)
            .header(header::ACCEPT, "application/json")
            .bearer_auth(self.target.key());
        if let Some(body) = body {
            request = request
                .header(header::CONTENT_TYPE, "application/json")
                .body(body.to_string());
        }
        let exchange = async {
            let mut response = request.send().await?;
            let status = response.status();
            let mut bytes = Vec::new();
            if status.is_success() {
                while let Some(chunk) = response.chunk().await? {
                    if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Ok((status, None));
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            Ok::<_, reqwest::Error>((status, Some(bytes)))
        };
        match tokio::time::timeout(REQUEST_TIMEOUT, exchange).await {
            Ok(Ok((status, bytes))) => {
                Reply::Status(status, bytes.and_then(|b| serde_json::from_slice(&b).ok()))
            }
            _ => Reply::Down,
        }
    }

    /// Look up an exact key (`partial` is `None`) or the newest entry with a
    /// partial digest. Misses, refusals and outages all return `None`.
    pub async fn lookup(
        &self,
        key: Option<&str>,
        partial: Option<&str>,
        events: &Events<'_>,
    ) -> Option<Entry> {
        loop {
            if self.disabled() {
                return None;
            }
            let scope = match self.state().scope_dropped {
                true => None,
                false => self.scope.clone(),
            };
            let mut query: Vec<(&str, &str)> = Vec::new();
            let path = match key {
                Some(key) => self.path(&format!("entries/{key}")),
                None => {
                    query.push(("partial", partial.unwrap_or_default()));
                    self.path("entries")
                }
            };
            if let Some(scope) = &scope {
                query.push(("scope", scope));
            }
            match self.send(Method::GET, &path, &query, None).await {
                Reply::Status(StatusCode::OK, body) => {
                    let found = body.as_ref().and_then(|b| entry(b, key, partial));
                    if found.is_none() {
                        events
                            .warning("the cache server returned an invalid lookup response")
                            .await;
                    }
                    return found;
                }
                Reply::Status(StatusCode::NOT_FOUND, _) => return None,
                Reply::Status(StatusCode::FORBIDDEN, _) if scope.is_some() => {
                    self.state().scope_dropped = true;
                    events
                        .run_warning(
                            "the cache key cannot read this run's scope; reading the default branch's caches only",
                        )
                        .await;
                }
                Reply::Status(StatusCode::SERVICE_UNAVAILABLE, _) | Reply::Down => {
                    self.disable(events, "server_unavailable").await;
                    return None;
                }
                Reply::Status(StatusCode::UNAUTHORIZED, _) => {
                    self.disable(events, "unauthorized").await;
                    return None;
                }
                Reply::Status(StatusCode::FORBIDDEN, _) => {
                    self.disable(events, "forbidden").await;
                    return None;
                }
                Reply::Status(status, _) => {
                    events
                        .warning(&format!("cache lookup failed (HTTP {})", status.as_u16()))
                        .await;
                    return None;
                }
            }
        }
    }

    /// Download `entry` into the new file `dest`, checking its size and
    /// sha256. No `Authorization` is sent.
    pub async fn download(&self, entry: &Entry, dest: &Path, abort: &AtomicBool) -> Result<()> {
        use sha2::Digest;
        use tokio::io::AsyncWriteExt;
        let client = presigned_client(&entry.download_url)?;
        let mut response = abortable(abort, client.get(entry.download_url.clone()).send())
            .await?
            .map_err(|_| anyhow!("the cache download failed: could not reach the object store"))?;
        if response.status() != StatusCode::OK {
            bail!(
                "the cache download failed (HTTP {})",
                response.status().as_u16()
            );
        }
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(dest)
            .await?;
        let mut hasher = sha2::Sha256::new();
        let mut received: u64 = 0;
        while let Some(chunk) = abortable(abort, response.chunk())
            .await?
            .map_err(|_| anyhow!("the cache download was interrupted"))?
        {
            received += chunk.len() as u64;
            if received > entry.size {
                bail!("the downloaded archive is larger than its metadata says");
            }
            hasher.update(&chunk);
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        file.sync_all().await?;
        if received != entry.size || super::key_hex(hasher.finalize()) != entry.sha256 {
            bail!("the downloaded archive does not match its size and sha256");
        }
        Ok(())
    }

    /// Reserve, upload and commit the archive at `path`.
    #[allow(clippy::too_many_arguments)]
    pub async fn save(
        &self,
        name: &str,
        partial: &str,
        key: &str,
        path: &Path,
        written: &archive::Written,
        events: &Events<'_>,
        abort: &AtomicBool,
    ) -> Saved {
        if self.disabled() {
            return Saved::Unavailable;
        }
        if self.state().read_only {
            return Saved::Skipped("read_only");
        }
        let Some(scope) = &self.scope else {
            return Saved::Skipped("no_scope");
        };
        let body = json!({"name": name, "partial": partial, "scope": scope,
            "size": written.bytes, "sha256": written.sha256});
        let reservation = match self
            .send(
                Method::POST,
                &self.path(&format!("entries/{key}")),
                &[],
                Some(body),
            )
            .await
        {
            Reply::Status(StatusCode::CREATED, Some(body)) => body,
            Reply::Status(StatusCode::CONFLICT, _) => return Saved::Stored,
            Reply::Status(StatusCode::FORBIDDEN, _) => {
                let first = !std::mem::replace(&mut self.state().read_only, true);
                if first {
                    events
                        .run_warning("the cache key is read-only; no further remote saves this run")
                        .await;
                }
                return Saved::Skipped("read_only");
            }
            Reply::Status(StatusCode::PAYLOAD_TOO_LARGE, _) => return Saved::Skipped("too_large"),
            Reply::Status(StatusCode::INSUFFICIENT_STORAGE, _) => return Saved::Skipped("quota"),
            Reply::Status(StatusCode::SERVICE_UNAVAILABLE, _) | Reply::Down => {
                self.disable(events, "server_unavailable").await;
                return Saved::Unavailable;
            }
            Reply::Status(StatusCode::UNAUTHORIZED, _) => {
                self.disable(events, "unauthorized").await;
                return Saved::Unavailable;
            }
            Reply::Status(status, _) => {
                return Saved::Failed(format!(
                    "the cache reservation failed (HTTP {})",
                    status.as_u16()
                ))
            }
        };
        let data = &reservation["data"];
        let (Some(upload_id), Some(upload_url), Some(headers)) = (
            data["upload_id"].as_str().filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
            }),
            presigned(&data["upload_url"]),
            data["upload_headers"].as_object(),
        ) else {
            return Saved::Failed("the cache server returned an invalid reservation".into());
        };
        let mut map = header::HeaderMap::new();
        for (name, value) in headers {
            let (Ok(name), Some(Ok(value))) = (
                header::HeaderName::from_bytes(name.as_bytes()),
                value.as_str().map(header::HeaderValue::from_str),
            ) else {
                return Saved::Failed("the cache server returned invalid upload headers".into());
            };
            if name == header::AUTHORIZATION {
                return Saved::Failed(
                    "the cache server asked for an Authorization header on a presigned upload"
                        .into(),
                );
            }
            map.insert(name, value);
        }
        let uploaded = async {
            // Streamed from the file: an archive of up to 5 GB is never held
            // in memory. An explicit length keeps the body unchunked.
            let file = tokio::fs::File::open(path).await?;
            let length = file.metadata().await?.len();
            if length != written.bytes {
                bail!("the cache archive changed before its upload");
            }
            map.insert(header::CONTENT_LENGTH, header::HeaderValue::from(length));
            let request = presigned_client(&upload_url)?
                .put(upload_url.clone())
                .headers(map)
                .body(reqwest::Body::from(file))
                .send();
            let response = abortable(abort, request).await?.map_err(|_| {
                anyhow!("the cache upload failed: could not reach the object store")
            })?;
            if !response.status().is_success() {
                bail!(
                    "the cache upload failed (HTTP {})",
                    response.status().as_u16()
                );
            }
            Ok::<_, anyhow::Error>(())
        }
        .await;
        if let Err(error) = uploaded {
            if error.is::<archive::Aborted>() {
                // The cache's own bound reports the abandoned save.
                return Saved::Unavailable;
            }
            return Saved::Failed(format!("{error:#}"));
        }
        match self
            .send(
                Method::POST,
                &self.path(&format!("uploads/{upload_id}/commit")),
                &[],
                None,
            )
            .await
        {
            Reply::Status(StatusCode::OK, _) => Saved::Stored,
            Reply::Status(StatusCode::UNPROCESSABLE_ENTITY, _) => Saved::Failed(
                "the cache server rejected the upload: the stored object does not match".into(),
            ),
            Reply::Status(StatusCode::SERVICE_UNAVAILABLE, _) | Reply::Down => {
                self.disable(events, "server_unavailable").await;
                Saved::Unavailable
            }
            Reply::Status(status, _) => Saved::Failed(format!(
                "the cache commit failed (HTTP {})",
                status.as_u16()
            )),
        }
    }
}

/// A minted per-run cache key.
pub struct Minted {
    pub id: String,
    pub token: String,
}

/// Mint a `ck_` key for `target`'s project with its (`sk_`) key.
pub async fn mint(
    target: &Target,
    access: &str,
    scope: Option<&str>,
    ttl_seconds: u64,
) -> Result<Minted> {
    let client = target.client(CONNECT_TIMEOUT)?;
    let mut body =
        json!({"access": access, "ttl_seconds": ttl_seconds, "name": "goodflows sprite run"});
    if let Some(scope) = scope {
        body["scope"] = json!(scope);
    }
    let url = target.api(&format!("/api/v1/projects/{}/cache-keys", target.project()));
    let exchange = async {
        let response = client
            .post(url)
            .bearer_auth(target.key())
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json")
            .body(body.to_string())
            .send()
            .await?;
        let status = response.status();
        let bytes = response.bytes().await?;
        Ok::<_, reqwest::Error>((status, bytes))
    };
    let (status, bytes) = tokio::time::timeout(REQUEST_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("minting a cache key timed out"))?
        .map_err(|_| anyhow!("minting a cache key failed: could not reach the server"))?;
    if status != StatusCode::CREATED {
        bail!("minting a cache key failed (HTTP {})", status.as_u16());
    }
    if bytes.len() > MAX_RESPONSE_BYTES {
        bail!("minting a cache key failed: the response is too large");
    }
    let body: Value = serde_json::from_slice(&bytes)
        .context("minting a cache key failed: the response is not JSON")?;
    let data = &body["data"];
    let id = match &data["id"] {
        Value::String(id) => id.clone(),
        Value::Number(id) => id.to_string(),
        _ => bail!("minting a cache key failed: the response has no id"),
    };
    let token = data["token"]
        .as_str()
        .filter(|t| {
            t.starts_with("ck_") && t.len() <= 512 && t.bytes().all(|b| b.is_ascii_graphic())
        })
        .context("minting a cache key failed: the response has no ck_ token")?;
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("minting a cache key failed: the response has an invalid id");
    }
    Ok(Minted {
        id,
        token: token.to_owned(),
    })
}

/// Revoke a minted key. A `204` and a `404` both mean it is gone.
pub async fn revoke(target: &Target, id: &str) -> Result<()> {
    let client = target.client(CONNECT_TIMEOUT)?;
    let url = target.api(&format!(
        "/api/v1/projects/{}/cache-keys/{id}",
        target.project()
    ));
    let response = tokio::time::timeout(
        REQUEST_TIMEOUT,
        client.delete(url).bearer_auth(target.key()).send(),
    )
    .await
    .map_err(|_| anyhow!("revoking the cache key timed out"))?
    .map_err(|_| anyhow!("revoking the cache key failed: could not reach the server"))?;
    match response.status() {
        StatusCode::NO_CONTENT | StatusCode::OK | StatusCode::NOT_FOUND => Ok(()),
        status => bail!("revoking the cache key failed (HTTP {})", status.as_u16()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn presigned_redirects_keep_the_host_and_scheme() {
        let url = |u: &str| Url::parse(u).unwrap();
        let first = [url("https://bucket.example/o?sig=1")];
        assert!(follows(&first, &url("https://bucket.example/other")));
        assert!(!follows(&first, &url("http://bucket.example/other")));
        assert!(!follows(&first, &url("https://elsewhere.example/o")));
        assert!(!follows(
            &[
                first[0].clone(),
                first[0].clone(),
                first[0].clone(),
                first[0].clone(),
                first[0].clone()
            ],
            &first[0]
        ));
    }
}
