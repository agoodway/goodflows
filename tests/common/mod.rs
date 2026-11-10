#![allow(dead_code)]

use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub struct Project {
    pub dir: PathBuf,
}

impl Project {
    pub fn new(config: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "goodflows test {} {}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        // Resolve symlinked temp dirs (macOS /var -> /private/var) so paths the
        // CLI prints compare equal to paths the tests build.
        let dir = std::fs::canonicalize(&dir).unwrap();
        std::fs::write(dir.join("goodflows.toml"), config).unwrap();
        Self { dir }
    }

    pub fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_goodflows"));
        command.current_dir(&self.dir).env("NO_COLOR", "1");
        // Tests must never reach a real reporting endpoint through inherited variables.
        for name in [
            "GOODFLOWS_URL",
            "GOODFLOWS_API_KEY",
            "GOODFLOWS_RUN_ID",
            "GOODFLOWS_REPORT_DIR",
        ] {
            command.env_remove(name);
        }
        command
    }

    pub fn run(&self, args: &[&str]) -> Output {
        self.command().args(args).output().unwrap()
    }

    pub fn read(&self, path: &str) -> String {
        std::fs::read_to_string(self.dir.join(path)).unwrap()
    }
}

impl Drop for Project {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

pub fn success(output: &Output) {
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        stdout(output),
        stderr(output)
    );
}

/// Minimal threaded HTTP/1.1 server for reporting tests. One request per connection.
pub mod mock {
    use std::collections::HashMap;
    use std::io::{BufRead, BufReader, Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    #[derive(Clone, Debug)]
    pub struct Request {
        pub method: String,
        pub path: String,
        pub headers: HashMap<String, String>,
        pub body: Vec<u8>,
    }

    impl Request {
        pub fn json(&self) -> serde_json::Value {
            serde_json::from_slice(&self.body).unwrap()
        }
    }

    pub enum Reply {
        Status(u16, Vec<(String, String)>, String),
        Hang(Duration),
        Close,
    }

    type Handler = dyn Fn(&Request, usize) -> Reply + Send + Sync;

    pub struct Mock {
        pub url: String,
        pub requests: Arc<Mutex<Vec<Request>>>,
        pub connections: Arc<Mutex<usize>>,
    }

    impl Mock {
        pub fn start(handler: impl Fn(&Request, usize) -> Reply + Send + Sync + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            let requests = Arc::new(Mutex::new(Vec::new()));
            let connections = Arc::new(Mutex::new(0));
            let handler: Arc<Handler> = Arc::new(handler);
            let (log, count) = (requests.clone(), connections.clone());
            std::thread::spawn(move || {
                for stream in listener.incoming() {
                    let Ok(mut stream) = stream else { break };
                    *count.lock().unwrap() += 1;
                    let (log, handler) = (log.clone(), handler.clone());
                    std::thread::spawn(move || {
                        let Some(request) = read(&mut stream) else {
                            return;
                        };
                        let index = {
                            let mut log = log.lock().unwrap();
                            log.push(request.clone());
                            log.len() - 1
                        };
                        match handler(&request, index) {
                            Reply::Status(code, headers, body) => {
                                let mut head = format!(
                                    "HTTP/1.1 {code} Mock\r\nContent-Length: {}\r\nConnection: close\r\n",
                                    body.len()
                                );
                                for (k, v) in headers {
                                    head.push_str(&format!("{k}: {v}\r\n"));
                                }
                                let _ = write!(stream, "{head}\r\n{body}");
                            }
                            Reply::Hang(duration) => std::thread::sleep(duration),
                            Reply::Close => {}
                        }
                    });
                }
            });
            Self {
                url,
                requests,
                connections,
            }
        }

        /// Emulates the server's idempotent store: 201 new, 200 exact retry,
        /// 409 different payload, for test reports and run records.
        pub fn store() -> Self {
            let rows: Mutex<HashMap<String, (Vec<u8>, String)>> = Mutex::new(HashMap::new());
            let next = Mutex::new(0u32);
            Self::start(move |request, _| store_reply(&rows, &next, request))
        }

        pub fn requests(&self) -> Vec<Request> {
            self.requests.lock().unwrap().clone()
        }

        pub fn connections(&self) -> usize {
            *self.connections.lock().unwrap()
        }
    }

    pub fn store_reply(
        rows: &Mutex<HashMap<String, (Vec<u8>, String)>>,
        next: &Mutex<u32>,
        request: &Request,
    ) -> Reply {
        let body = request.json();
        let run_record = request.path.ends_with("/workflow-runs");
        let report_id = if run_record {
            body["run_id"].as_str().unwrap().to_owned()
        } else {
            body["report"]["report_id"].as_str().unwrap().to_owned()
        };
        let key = if run_record {
            format!("{}|{report_id}", request.path)
        } else {
            format!(
                "{}|{}|{}|{}",
                request.path, body["workflow_run_id"], body["job"], report_id
            )
        };
        let mut rows = rows.lock().unwrap();
        let (code, id, duplicate) = match rows.get(&key) {
            Some((stored, id)) if stored == &request.body => (200, id.clone(), true),
            Some(_) => {
                return Reply::Status(409, vec![], r#"{"errors":{"detail":"conflict"}}"#.into())
            }
            None => {
                let mut next = next.lock().unwrap();
                *next += 1;
                let id = format!("00000000-0000-4000-8000-{:012}", *next);
                rows.insert(key, (request.body.clone(), id.clone()));
                (201, id, false)
            }
        };
        let body = if run_record {
            run_ack(&id, &report_id, duplicate)
        } else {
            ack(&id, &report_id, duplicate)
        };
        Reply::Status(code, vec![], body)
    }

    /// A `workflow-runs` acknowledgement.
    pub fn run_ack(id: &str, run_id: &str, duplicate: bool) -> String {
        format!(r#"{{"data":{{"id":"{id}","run_id":"{run_id}","duplicate":{duplicate}}}}}"#)
    }

    pub fn ack(id: &str, report_id: &str, duplicate: bool) -> String {
        format!(
            r#"{{"data":{{"id":"{id}","report_id":"{report_id}","status":"complete","duplicate":{duplicate}}}}}"#
        )
    }

    fn read(stream: &mut std::net::TcpStream) -> Option<Request> {
        let mut reader = BufReader::new(stream.try_clone().ok()?);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let mut parts = line.split_whitespace();
        let method = parts.next()?.to_owned();
        let path = parts.next()?.to_owned();
        let mut headers = HashMap::new();
        loop {
            let mut header = String::new();
            reader.read_line(&mut header).ok()?;
            let header = header.trim_end();
            if header.is_empty() {
                break;
            }
            let (key, value) = header.split_once(':')?;
            headers.insert(key.trim().to_ascii_lowercase(), value.trim().to_owned());
        }
        let length = headers
            .get("content-length")
            .and_then(|v| v.parse().ok())
            .unwrap_or(0);
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        Some(Request {
            method,
            path,
            headers,
            body,
        })
    }
}

/// One complete synthetic `test-timings/1` report as JSONL: one group holding
/// a test inside a describe path, with runner metadata in `meta`.
pub fn timing_report(report_id: &str, partition: Option<&str>) -> String {
    let partition = serde_json::json!(partition);
    let group = serde_json::json!({"group":"MyTest","file":"test/my_test.exs","execution_id":1,"started_offset_us":2});
    let test = serde_json::json!({"group":"MyTest","path":["math"],"name":"test works","file":"test/my_test.exs",
        "line":3,"execution_id":1,"started_offset_us":5});
    let record = |event: &str, offset: u64, core: &serde_json::Value, extra: serde_json::Value| {
        let mut record = serde_json::json!({"event":event,"suite_id":"suite-1","report_id":report_id,
            "received_offset_us":offset});
        for fields in [core, &extra] {
            for (key, value) in fields.as_object().unwrap() {
                record[key] = value.clone();
            }
        }
        record
    };
    let none = serde_json::json!({});
    [
        record("suite_started", 0, &none, serde_json::json!({"format":"test-timings/1",
            "started_at":"2026-11-01T00:00:00Z","runner":{"name":"exunit","version":"1.18.0"},
            "partition":partition,"partition_count":3,"seed":1,"filters":{"include":[],"exclude":[]},
            "meta":{"elixir_version":"1.18.0","otp_release":"27","max_cases":2,"schedulers":2}})),
        record("group_started", 2, &group, serde_json::json!({"meta":{"async":true}})),
        record("test_started", 5, &test, serde_json::json!({"meta":{"async":true}})),
        record("test_finished", 10, &test, serde_json::json!({"duration_us":42,"outcome":"passed"})),
        record("group_finished", 60, &group, serde_json::json!({"duration_us":50,"outcome":"passed",
            "counts":{"passed":1}})),
        record("suite_finished", 99, &none, serde_json::json!({"aborted":false,"unfinished_tests":0,
            "unfinished_groups":0,"counts":{"passed":1},"duration_us":100,
            "meta":{"run_us":100,"async_us":60,"sync_us":40,"load_us":null,"total_us":100}})),
    ]
    .iter()
    .map(|r| format!("{r}\n"))
    .collect()
}

pub fn events(output: &Output) -> Vec<serde_json::Value> {
    stdout(output)
        .lines()
        .map(|line| {
            let event: serde_json::Value = serde_json::from_str(line)
                .unwrap_or_else(|e| panic!("invalid JSON line {line:?}: {e}"));
            assert_eq!(event["schema_version"], 1);
            event
        })
        .collect()
}

/// Recursively list regular files under a directory.
pub fn files_under(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
            let path = entry.path();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                stack.push(path);
            } else if kind.is_file() {
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// Whether a name is a run stem: `YYYYMMDDTHHMMSSZ-<lowercase uuid>`.
pub fn is_stem(name: &str) -> bool {
    let b = name.as_bytes();
    b.len() == 53
        && b[..8].iter().all(u8::is_ascii_digit)
        && b[8] == b'T'
        && b[9..15].iter().all(u8::is_ascii_digit)
        && b[15] == b'Z'
        && b[16] == b'-'
        && name[17..]
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c) || c == b'-')
}

/// Log run directories (stem-named directories, never history files) directly
/// under `dir`, oldest first, as absolute paths.
pub fn run_dirs(dir: &std::path::Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter(|e| e.file_type().unwrap().is_dir())
        .map(|e| e.path())
        .filter(|p| is_stem(p.file_name().unwrap().to_str().unwrap_or_default()))
        .collect();
    dirs.sort();
    dirs
}

/// The single log run directory directly under `dir`, as an absolute path.
/// Panics unless there is exactly one.
pub fn run_dir(dir: &std::path::Path) -> PathBuf {
    let dirs = run_dirs(dir);
    assert_eq!(
        dirs.len(),
        1,
        "expected one run directory in {}: {dirs:?}",
        dir.display()
    );
    dirs.into_iter().next().unwrap()
}

/// Log file names in a run directory, excluding the run's lock file.
pub fn log_names(dir: &std::path::Path) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|name| name != ".goodflows.lock")
        .collect();
    names.sort();
    names
}
