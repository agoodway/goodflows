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

/// One API response: its status and, for a success or a `422`, the JSON
/// body.
enum Reply {
    Status(StatusCode, Option<Value>),
    /// A connection failure or a timeout.
    Down,
    /// The cache's bound or a cancellation stopped the request; the remote
    /// store stays enabled.
    Aborted,
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

/// How long a download may deliver nothing (debug builds:
/// `GOODFLOWS_TEST_CACHE_READ_TIMEOUT_SECS`).
fn read_timeout() -> Duration {
    #[cfg(debug_assertions)]
    if let Some(secs) = std::env::var("GOODFLOWS_TEST_CACHE_READ_TIMEOUT_SECS")
        .ok()
        .and_then(|v| v.parse().ok())
    {
        return Duration::from_secs(secs);
    }
    REQUEST_TIMEOUT
}

/// A client for presigned URLs: redirects only within the same host, and
/// no proxy for loopback targets.
///
/// Only downloads get a read timeout. reqwest starts it when the request
/// is sent and it covers the wait for response headers, which a store
/// sends only after the whole upload body, so it would fail any upload
/// slower than the timeout. A stalled upload ends at the cache's own bound.
fn presigned_client(url: &Url, upload: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder().connect_timeout(CONNECT_TIMEOUT);
    if !upload {
        // A stalled download ends; the cache's own bound covers the rest.
        builder = builder.read_timeout(read_timeout());
    }
    builder = builder.redirect(redirect::Policy::custom(|attempt| {
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

/// Size of one part: one ranged download request, or one multipart upload
/// part.
const PART_BYTES: u64 = 16 * 1024 * 1024;
/// Archives of at least this many parts download in ranges and upload in
/// parts; a smaller one gains little over one connection.
const PARTED_MIN_PARTS: u64 = 4;
/// Part requests (downloads or uploads) in flight at once for one archive.
const PART_CONNECTIONS: u64 = 8;
/// Tries per part before the download or upload fails.
const PART_ATTEMPTS: u32 = 3;
/// The wait before a part's next try, times the tries so far, so the tries
/// do not all land in one throttling window. Dropping the workers on an
/// abort ends the wait.
const PART_BACKOFF: Duration = Duration::from_millis(250);

/// [`PART_BYTES`]; tests may shrink it (debug builds:
/// `GOODFLOWS_TEST_CACHE_PART_BYTES`).
pub fn part_bytes() -> u64 {
    #[cfg(debug_assertions)]
    if let Some(bytes) = std::env::var("GOODFLOWS_TEST_CACHE_PART_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .filter(|b| *b > 0)
    {
        return bytes;
    }
    PART_BYTES
}

/// The `Range` header for `len` bytes from `start`.
fn range(start: u64, len: u64) -> String {
    format!("bytes={start}-{}", start + len - 1)
}

async fn create_new(dest: &Path) -> std::io::Result<tokio::fs::File> {
    tokio::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(dest)
        .await
}

/// Write a whole-object response into the new file `dest`, checking its
/// size and sha256.
async fn stream_whole(
    mut response: reqwest::Response,
    entry: &Entry,
    dest: &Path,
    abort: &AtomicBool,
) -> Result<()> {
    use sha2::Digest;
    use tokio::io::AsyncWriteExt;
    let mut file = create_new(dest).await?;
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

/// Fetch every part of `entry` into `dest`, which already has its full
/// length, over up to [`PART_CONNECTIONS`] connections. `first` is the
/// `206` for part 0.
async fn download_parts(
    client: reqwest::Client,
    entry: &Entry,
    dest: &Path,
    part: u64,
    first: reqwest::Response,
    abort: &AtomicBool,
) -> Result<()> {
    use std::sync::atomic::AtomicU64;
    use std::sync::Arc;
    let size = entry.size;
    let parts = size.div_ceil(part);
    let next = Arc::new(AtomicU64::new(1));
    let mut first = Some(first);
    // Dropping the set on an error or an abort cancels the other parts.
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..parts.min(PART_CONNECTIONS) {
        let (client, url, dest, next) = (
            client.clone(),
            entry.download_url.clone(),
            dest.to_path_buf(),
            next.clone(),
        );
        let mut response = first.take();
        workers.spawn(async move {
            let mut index = match response {
                Some(_) => 0,
                None => next.fetch_add(1, Ordering::Relaxed),
            };
            while index < parts {
                let start = index * part;
                let len = part.min(size - start);
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    match fetch_part(&client, &url, &dest, size, start, len, response.take()).await
                    {
                        Ok(()) => break,
                        Err(_) if attempt < PART_ATTEMPTS => {
                            tokio::time::sleep(PART_BACKOFF * attempt).await
                        }
                        Err(error) => return Err(error),
                    }
                }
                index = next.fetch_add(1, Ordering::Relaxed);
            }
            Ok::<_, anyhow::Error>(())
        });
    }
    while let Some(done) = abortable(abort, workers.join_next()).await? {
        done.map_err(|_| anyhow!("the cache download failed"))??;
    }
    let file = tokio::fs::OpenOptions::new().write(true).open(dest).await?;
    file.sync_all().await?;
    Ok(())
}

/// Fetch `len` bytes from `start` (reusing `response` when given) and write
/// them at that offset of `dest`.
async fn fetch_part(
    client: &reqwest::Client,
    url: &Url,
    dest: &Path,
    size: u64,
    start: u64,
    len: u64,
    response: Option<reqwest::Response>,
) -> Result<()> {
    use tokio::io::{AsyncSeekExt, AsyncWriteExt};
    let mut response = match response {
        Some(response) => response,
        None => client
            .get(url.clone())
            .header(header::RANGE, range(start, len))
            .send()
            .await
            .map_err(|_| anyhow!("the cache download failed: could not reach the object store"))?,
    };
    if response.status() != StatusCode::PARTIAL_CONTENT {
        bail!(
            "the cache download failed (HTTP {})",
            response.status().as_u16()
        );
    }
    let expected = format!("bytes {start}-{}/{size}", start + len - 1);
    if response
        .headers()
        .get(header::CONTENT_RANGE)
        .and_then(|v| v.to_str().ok())
        != Some(expected.as_str())
    {
        bail!("the object store returned a different range than requested");
    }
    let mut file = tokio::fs::OpenOptions::new().write(true).open(dest).await?;
    file.seek(std::io::SeekFrom::Start(start)).await?;
    let mut received: u64 = 0;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| anyhow!("the cache download was interrupted"))?
    {
        received += chunk.len() as u64;
        if received > len {
            bail!("the downloaded archive is larger than its metadata says");
        }
        file.write_all(&chunk).await?;
    }
    if received != len {
        bail!("the cache download was interrupted");
    }
    file.flush().await?;
    Ok(())
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
        abort: &AtomicBool,
    ) -> Reply {
        self.send_within(method, path, query, body, REQUEST_TIMEOUT, abort)
            .await
    }

    /// [`Remote::send`] with its own time bound. The body of a 2xx or a
    /// `422` is kept.
    async fn send_within(
        &self,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<Value>,
        timeout: Duration,
        abort: &AtomicBool,
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
            // A `422` body names the rejected fields.
            if status.is_success() || status == StatusCode::UNPROCESSABLE_ENTITY {
                while let Some(chunk) = response.chunk().await? {
                    if bytes.len() + chunk.len() > MAX_RESPONSE_BYTES {
                        return Ok((status, None));
                    }
                    bytes.extend_from_slice(&chunk);
                }
            }
            Ok::<_, reqwest::Error>((status, Some(bytes)))
        };
        match abortable(abort, tokio::time::timeout(timeout, exchange)).await {
            Ok(Ok(Ok((status, bytes)))) => {
                Reply::Status(status, bytes.and_then(|b| serde_json::from_slice(&b).ok()))
            }
            Ok(_) => Reply::Down,
            Err(_) => Reply::Aborted,
        }
    }

    /// Look up an exact key (`partial` is `None`) or the newest entry with a
    /// partial digest. Misses, refusals and outages all return `None`; only
    /// `abort` fails it, with [`archive::Aborted`].
    pub async fn lookup(
        &self,
        key: Option<&str>,
        partial: Option<&str>,
        events: &Events<'_>,
        abort: &AtomicBool,
    ) -> Result<Option<Entry>> {
        loop {
            if self.disabled() {
                return Ok(None);
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
            match self.send(Method::GET, &path, &query, None, abort).await {
                Reply::Aborted => return Err(archive::Aborted.into()),
                Reply::Status(StatusCode::OK, body) => {
                    let found = body.as_ref().and_then(|b| entry(b, key, partial));
                    if found.is_none() {
                        events
                            .warning("the cache server returned an invalid lookup response")
                            .await;
                    }
                    return Ok(found);
                }
                Reply::Status(StatusCode::NOT_FOUND, _) => return Ok(None),
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
                    return Ok(None);
                }
                Reply::Status(StatusCode::UNAUTHORIZED, _) => {
                    self.disable(events, "unauthorized").await;
                    return Ok(None);
                }
                Reply::Status(StatusCode::FORBIDDEN, _) => {
                    self.disable(events, "forbidden").await;
                    return Ok(None);
                }
                Reply::Status(status, _) => {
                    events
                        .warning(&format!("cache lookup failed (HTTP {})", status.as_u16()))
                        .await;
                    return Ok(None);
                }
            }
        }
    }

    /// Download `entry` into the new file `dest`. No `Authorization` is sent.
    ///
    /// An archive of at least [`PARTED_MIN_PARTS`] parts is fetched as
    /// concurrent `Range` requests, each part checked against its
    /// `Content-Range` and length; [`archive::extract`] then checks the
    /// whole file's size and sha256. A store that ignores `Range` answers
    /// the first request with the whole object, which is streamed instead.
    /// A smaller archive is streamed, checking its size and sha256 here.
    pub async fn download(&self, entry: &Entry, dest: &Path, abort: &AtomicBool) -> Result<()> {
        let client = presigned_client(&entry.download_url, false)?;
        let part = part_bytes();
        let ranged = entry.size >= part.saturating_mul(PARTED_MIN_PARTS);
        let mut request = client.get(entry.download_url.clone());
        if ranged {
            request = request.header(header::RANGE, range(0, part.min(entry.size)));
        }
        let response = abortable(abort, request.send())
            .await?
            .map_err(|_| anyhow!("the cache download failed: could not reach the object store"))?;
        match response.status() {
            StatusCode::PARTIAL_CONTENT if ranged => {
                let file = create_new(dest).await?;
                file.set_len(entry.size).await?;
                drop(file);
                download_parts(client, entry, dest, part, response, abort).await
            }
            StatusCode::OK => stream_whole(response, entry, dest, abort).await,
            status => bail!("the cache download failed (HTTP {})", status.as_u16()),
        }
    }

    /// Reserve, upload and commit the archive at `path`.
    ///
    /// An archive of at least [`PARTED_MIN_PARTS`] parts is reserved with
    /// its part MD5s; when the server returns `upload_parts`, the parts are
    /// uploaded in parallel, otherwise the archive goes up as one `PUT`.
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
        let mut body = json!({"name": name, "partial": partial, "scope": scope,
            "size": written.bytes, "sha256": written.sha256});
        let mut parts = multipart(written, part_bytes());
        let reservation = loop {
            if let Some((size, md5s)) = parts {
                body["parts"] = json!({"size": size, "md5": md5s});
            } else if let Some(body) = body.as_object_mut() {
                body.remove("parts");
            }
            match self
                .send(
                    Method::POST,
                    &self.path(&format!("entries/{key}")),
                    &[],
                    Some(body.clone()),
                    abort,
                )
                .await
            {
                // The cache's own bound reports the abandoned save.
                Reply::Aborted => return Saved::Unavailable,
                Reply::Status(StatusCode::CREATED, Some(body)) => break body,
                Reply::Status(StatusCode::CONFLICT, _) => return Saved::Stored,
                // A server without multipart support refuses `parts` by
                // name: reserve once more for a single upload.
                Reply::Status(StatusCode::UNPROCESSABLE_ENTITY, Some(errors))
                    if parts.is_some() && errors["errors"].get("parts").is_some() =>
                {
                    events
                        .run_warning(
                            "the cache server refused a multipart upload; uploading archives in one request",
                        )
                        .await;
                    parts = None;
                }
                Reply::Status(StatusCode::FORBIDDEN, _) => {
                    let first = !std::mem::replace(&mut self.state().read_only, true);
                    if first {
                        events
                            .run_warning(
                                "the cache key is read-only; no further remote saves this run",
                            )
                            .await;
                    }
                    return Saved::Skipped("read_only");
                }
                Reply::Status(StatusCode::PAYLOAD_TOO_LARGE, _) => {
                    return Saved::Skipped("too_large")
                }
                Reply::Status(StatusCode::INSUFFICIENT_STORAGE, _) => {
                    return Saved::Skipped("quota")
                }
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
            }
        };
        let data = &reservation["data"];
        let Some(upload_id) = data["upload_id"].as_str().filter(|id| {
            !id.is_empty()
                && id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        }) else {
            return Saved::Failed("the cache server returned an invalid reservation".into());
        };
        let upload = match (&data["upload_parts"], parts) {
            (Value::Null, _) => match single_upload(data) {
                Ok(upload) => upload,
                Err(error) => return Saved::Failed(error.into()),
            },
            (listed, Some((size, md5s))) => {
                match upload_parts(listed, written.bytes, *size, md5s) {
                    Ok(upload) => upload,
                    Err(error) => return Saved::Failed(error.into()),
                }
            }
            // Parts this client never asked for.
            (_, None) => {
                return Saved::Failed("the cache server returned an invalid reservation".into())
            }
        };
        let multipart = matches!(upload, Upload::Parts(_));
        let uploaded = async {
            // Streamed from the file: an archive of up to 5 GB is never held
            // in memory. An explicit length keeps every body unchunked.
            let file = tokio::fs::File::open(path).await?;
            let length = file.metadata().await?.len();
            if length != written.bytes {
                bail!("the cache archive changed before its upload");
            }
            match upload {
                Upload::Single(url, mut map) => {
                    map.insert(header::CONTENT_LENGTH, header::HeaderValue::from(length));
                    let request = presigned_client(&url, true)?
                        .put(url)
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
                }
                Upload::Parts(parts) => {
                    // Parts read this one open file at their offsets, so
                    // nothing reopens the staged archive by path.
                    put_parts(file.into_std().await, parts, abort).await?;
                }
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
        // The server completes a multipart upload during the commit, which
        // it allows up to two minutes.
        let timeout = match multipart {
            true => MULTIPART_COMMIT_TIMEOUT,
            false => REQUEST_TIMEOUT,
        };
        match self
            .send_within(
                Method::POST,
                &self.path(&format!("uploads/{upload_id}/commit")),
                &[],
                None,
                timeout,
                abort,
            )
            .await
        {
            Reply::Aborted => Saved::Unavailable,
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

/// The most parts a multipart upload may have.
const MAX_PARTS: usize = 10_000;
/// Bytes read from the archive per chunk of a part's body.
const PART_CHUNK_BYTES: u64 = 256 * 1024;

/// The part size and MD5s to reserve with, when `written` qualifies for a
/// multipart upload with `part`-byte parts: at least [`PARTED_MIN_PARTS`]
/// whole parts, and at most [`MAX_PARTS`] parts.
fn multipart(written: &archive::Written, part: u64) -> Option<&(u64, Vec<String>)> {
    written.part_md5.as_ref().filter(|(size, md5s)| {
        *size == part
            && written.bytes >= part.saturating_mul(PARTED_MIN_PARTS)
            && md5s.len() <= MAX_PARTS
    })
}
/// A multipart commit, during which the server completes the upload.
const MULTIPART_COMMIT_TIMEOUT: Duration = Duration::from_secs(180);

/// How a reservation is uploaded.
enum Upload {
    /// One `PUT` to this URL with these headers.
    Single(Url, header::HeaderMap),
    Parts(Vec<Part>),
}

/// One part of a multipart upload.
struct Part {
    url: Url,
    headers: header::HeaderMap,
    start: u64,
    len: u64,
}

/// The headers of a presigned request, refusing `Authorization`.
fn presigned_headers(
    headers: &serde_json::Map<String, Value>,
) -> Result<header::HeaderMap, &'static str> {
    let mut map = header::HeaderMap::new();
    for (name, value) in headers {
        let (Ok(name), Some(Ok(value))) = (
            header::HeaderName::from_bytes(name.as_bytes()),
            value.as_str().map(header::HeaderValue::from_str),
        ) else {
            return Err("the cache server returned invalid upload headers");
        };
        if name == header::AUTHORIZATION {
            return Err("the cache server asked for an Authorization header on a presigned upload");
        }
        map.insert(name, value);
    }
    Ok(map)
}

/// A reservation for one `PUT`: `upload_url` and `upload_headers`.
fn single_upload(data: &Value) -> Result<Upload, &'static str> {
    let (Some(url), Some(headers)) = (
        presigned(&data["upload_url"]),
        data["upload_headers"].as_object(),
    ) else {
        return Err("the cache server returned an invalid reservation");
    };
    Ok(Upload::Single(url, presigned_headers(headers)?))
}

/// A reservation's `upload_parts`, checked against the parts requested:
/// each part once in order, on the first part's host and scheme, with
/// exactly its length and MD5.
fn upload_parts(
    listed: &Value,
    bytes: u64,
    size: u64,
    md5s: &[String],
) -> Result<Upload, &'static str> {
    const INVALID: &str = "the cache server returned an invalid reservation";
    let listed = listed.as_array().ok_or(INVALID)?;
    if listed.len() != md5s.len() || bytes.div_ceil(size) != md5s.len() as u64 {
        return Err(INVALID);
    }
    let mut parts: Vec<Part> = Vec::with_capacity(listed.len());
    for (index, (item, md5)) in listed.iter().zip(md5s).enumerate() {
        let start = index as u64 * size;
        let len = size.min(bytes - start);
        if item["number"].as_u64() != Some(index as u64 + 1) {
            return Err(INVALID);
        }
        let url = presigned(&item["url"]).ok_or(INVALID)?;
        if let Some(first) = parts.first() {
            if first.url.host_str() != url.host_str()
                || first.url.scheme() != url.scheme()
                || first.url.port_or_known_default() != url.port_or_known_default()
            {
                return Err(INVALID);
            }
        }
        let headers = presigned_headers(item["headers"].as_object().ok_or(INVALID)?)?;
        let text = |name: header::HeaderName| headers.get(name).and_then(|v| v.to_str().ok());
        if text(header::CONTENT_LENGTH) != Some(len.to_string().as_str())
            || text(header::HeaderName::from_static("content-md5")) != Some(md5.as_str())
        {
            return Err(INVALID);
        }
        parts.push(Part {
            url,
            headers,
            start,
            len,
        });
    }
    Ok(Upload::Parts(parts))
}

/// Upload every part of the archive `file` over up to
/// [`PART_CONNECTIONS`] connections, each part tried up to
/// [`PART_ATTEMPTS`] times.
async fn put_parts(file: std::fs::File, parts: Vec<Part>, abort: &AtomicBool) -> Result<()> {
    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;
    let client = presigned_client(&parts[0].url, true)?;
    let file = Arc::new(file);
    let parts = Arc::new(parts);
    let next = Arc::new(AtomicUsize::new(0));
    // Dropping the set on an error or an abort cancels the other parts.
    let mut workers = tokio::task::JoinSet::new();
    for _ in 0..(parts.len() as u64).min(PART_CONNECTIONS) {
        let (client, file, parts, next) =
            (client.clone(), file.clone(), parts.clone(), next.clone());
        workers.spawn(async move {
            loop {
                let Some(part) = parts.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    return Ok::<_, anyhow::Error>(());
                };
                let mut attempt = 0;
                loop {
                    attempt += 1;
                    match put_part(&client, &file, part).await {
                        Ok(()) => break,
                        Err(_) if attempt < PART_ATTEMPTS => {
                            tokio::time::sleep(PART_BACKOFF * attempt).await
                        }
                        Err(error) => return Err(error),
                    }
                }
            }
        });
    }
    while let Some(done) = abortable(abort, workers.join_next()).await? {
        done.map_err(|_| anyhow!("the cache upload failed"))??;
    }
    Ok(())
}

/// `PUT` one part, streamed from its offset of `file` with positioned
/// reads, so concurrent parts and retries never share a file offset and
/// each try starts again from the part's first byte.
async fn put_part(
    client: &reqwest::Client,
    file: &std::sync::Arc<std::fs::File>,
    part: &Part,
) -> Result<()> {
    let chunks = futures_util::stream::try_unfold(
        (file.clone(), part.start, part.len),
        |(file, at, left)| async move {
            if left == 0 {
                return Ok(None);
            }
            let len = left.min(PART_CHUNK_BYTES);
            let (file, chunk) = tokio::task::spawn_blocking(move || {
                use std::os::unix::fs::FileExt;
                let mut chunk = vec![0; len as usize];
                file.read_exact_at(&mut chunk, at).map(|()| (file, chunk))
            })
            .await
            .map_err(std::io::Error::other)??;
            Ok::<_, std::io::Error>(Some((chunk, (file, at + len, left - len))))
        },
    );
    let mut headers = part.headers.clone();
    headers.insert(header::CONTENT_LENGTH, header::HeaderValue::from(part.len));
    let response = client
        .put(part.url.clone())
        .headers(headers)
        .body(reqwest::Body::wrap_stream(chunks))
        .send()
        .await
        .map_err(|_| anyhow!("the cache upload failed: could not reach the object store"))?;
    if !response.status().is_success() {
        bail!(
            "the cache upload failed (HTTP {})",
            response.status().as_u16()
        );
    }
    Ok(())
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

    /// Three parts of a 40-byte archive in 16-byte parts, as a server
    /// would list them.
    fn listed() -> (Value, Vec<String>) {
        let md5s: Vec<String> = [
            "AAAAAAAAAAAAAAAAAAAAAA==",
            "AQEBAQEBAQEBAQEBAQEBAQ==",
            "AgICAgICAgICAgICAgICAg==",
        ]
        .map(String::from)
        .to_vec();
        let parts = (0..3)
            .map(|i| {
                json!({"number": i + 1, "url": format!("https://bucket.example/o?partNumber={}", i + 1),
                    "headers": {"content-length": if i < 2 { "16" } else { "8" }, "content-md5": md5s[i]}})
            })
            .collect();
        (Value::Array(parts), md5s)
    }

    #[test]
    fn upload_parts_accepts_each_part_once_in_order() {
        let (parts, md5s) = listed();
        let Ok(Upload::Parts(parts)) = upload_parts(&parts, 40, 16, &md5s) else {
            panic!("rejected");
        };
        let spans: Vec<(u64, u64)> = parts.iter().map(|p| (p.start, p.len)).collect();
        assert_eq!(spans, [(0, 16), (16, 16), (32, 8)]);
    }

    #[test]
    fn upload_parts_rejects_every_mismatch() {
        type Change = fn(&mut Value);
        let cases: Vec<(&str, Change)> = vec![
            ("one part missing", |p| {
                p.as_array_mut().unwrap().pop();
            }),
            ("numbers swapped", |p| {
                p[0]["number"] = json!(2);
                p[1]["number"] = json!(1);
            }),
            ("parts swapped", |p| p.as_array_mut().unwrap().swap(0, 1)),
            ("wrong md5", |p| {
                p[1]["headers"]["content-md5"] = json!("AwMDAwMDAwMDAwMDAwMDAw==")
            }),
            ("md5 missing", |p| {
                p[1]["headers"]
                    .as_object_mut()
                    .unwrap()
                    .remove("content-md5");
            }),
            ("wrong length", |p| {
                p[2]["headers"]["content-length"] = json!("16")
            }),
            ("length missing", |p| {
                p[0]["headers"]
                    .as_object_mut()
                    .unwrap()
                    .remove("content-length");
            }),
            ("another host", |p| {
                p[1]["url"] = json!("https://elsewhere.example/o?partNumber=2")
            }),
            ("another port", |p| {
                p[1]["url"] = json!("https://bucket.example:8443/o?partNumber=2")
            }),
            ("http after https", |p| {
                p[1]["url"] = json!("http://bucket.example/o?partNumber=2")
            }),
            ("plain http", |p| {
                p[0]["url"] = json!("http://bucket.example/o")
            }),
            ("http after https on one host and port", |p| {
                p[0]["url"] = json!("https://localhost/o?partNumber=1");
                p[1]["url"] = json!("https://localhost/o?partNumber=2");
                p[2]["url"] = json!("http://localhost:443/o?partNumber=3");
            }),
            ("authorization", |p| {
                p[2]["headers"]["authorization"] = json!("Bearer x")
            }),
            ("headers not strings", |p| {
                p[0]["headers"]["x-extra"] = json!(1)
            }),
            ("not a list", |p| *p = json!({})),
        ];
        for (name, change) in cases {
            let (mut parts, md5s) = listed();
            change(&mut parts);
            assert!(
                upload_parts(&parts, 40, 16, &md5s).is_err(),
                "{name} was accepted"
            );
        }
        // The default port counts as the same port.
        let (mut parts, md5s) = listed();
        parts[1]["url"] = json!("https://bucket.example:443/o?partNumber=2");
        assert!(upload_parts(&parts, 40, 16, &md5s).is_ok());
    }

    #[test]
    fn an_archive_of_at_least_four_whole_parts_reserves_parts() {
        let written = |bytes: u64, part: u64| archive::Written {
            bytes,
            sha256: String::new(),
            part_md5: Some((part, vec![String::new(); bytes.div_ceil(part) as usize])),
        };
        let w = written(64, 16);
        assert!(multipart(&w, 16).is_some());
        let w = written(63, 16);
        assert!(multipart(&w, 16).is_none());
        // MD5s of another part size, or more than 10,000 parts, are not sent.
        let w = written(64, 16);
        assert!(multipart(&w, 8).is_none());
        let w = written(10_001, 1);
        assert!(multipart(&w, 1).is_none());
        let w = written(10_000, 1);
        assert!(multipart(&w, 1).is_some());
        let none = archive::Written {
            bytes: 64,
            sha256: String::new(),
            part_md5: None,
        };
        assert!(multipart(&none, 16).is_none());
    }

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
