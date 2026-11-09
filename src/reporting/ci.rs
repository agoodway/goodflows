//! CI context for an uploaded run record, from GitHub Actions variables only
//! (design D3). The values are identities and links, not secrets, but they
//! are still never printed in diagnostics.

// Read by the run-record upload (GOO-245 task 4.3).
#![cfg_attr(not(test), expect(dead_code))]

use super::contract::MAX_SAFE_INTEGER;
use super::run_contract::{valid_label, valid_name, MAX_CI_RUN_ID_BYTES, MAX_URL_BYTES};
use reqwest::Url;
use serde::Serialize;

/// `ci` in the uploaded record. Each key is omitted when its variable is
/// unset or fails its rule; a bad value never makes the record invalid.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Ci {
    provider: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    event: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pull_request: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    runner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    url: Option<String>,
}

/// The CI context from the caller's environment, read through `lookup`
/// (`std::env::var` in a run, never settings `env`). `None` unless
/// `GITHUB_ACTIONS` is exactly `true`.
pub fn capture(lookup: impl Fn(&str) -> Option<String>) -> Option<Ci> {
    if lookup("GITHUB_ACTIONS").as_deref() != Some("true") {
        return None;
    }
    let run_id = lookup("GITHUB_RUN_ID").filter(|id| valid_label(id, MAX_CI_RUN_ID_BYTES));
    let url = run_url(
        lookup("GITHUB_SERVER_URL").as_deref(),
        lookup("GITHUB_REPOSITORY").as_deref(),
        run_id.as_deref(),
    );
    Some(Ci {
        provider: "github_actions",
        event: lookup("GITHUB_EVENT_NAME").filter(|event| valid_name(event)),
        run_id,
        attempt: lookup("GITHUB_RUN_ATTEMPT").as_deref().and_then(counter),
        pull_request: lookup("GITHUB_REF").as_deref().and_then(|reference| {
            counter(
                reference
                    .strip_prefix("refs/pull/")?
                    .strip_suffix("/merge")?,
            )
        }),
        runner: lookup("RUNNER_NAME").filter(|runner| valid_name(runner)),
        url,
    })
}

/// ASCII digits only, from 1 to 2^53-1.
fn counter(text: &str) -> Option<u64> {
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    text.parse()
        .ok()
        .filter(|n| (1..=MAX_SAFE_INTEGER).contains(n))
}

/// `<server>/<owner>/<name>/actions/runs/<run id>`, from an origin-only
/// `https` server URL, an `owner/name` repository and a numeric run id.
fn run_url(server: Option<&str>, repository: Option<&str>, run_id: Option<&str>) -> Option<String> {
    let server = Url::parse(server?).ok()?;
    if server.scheme() != "https"
        || server.host_str().is_none()
        || !server.username().is_empty()
        || server.password().is_some()
        || server.path() != "/"
        || server.query().is_some()
        || server.fragment().is_some()
    {
        return None;
    }
    let (owner, name) = repository?.split_once('/')?;
    let segment = |s: &str| {
        !s.is_empty()
            && s != "."
            && s != ".."
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'-'))
    };
    let run_id = run_id?;
    if !segment(owner)
        || !segment(name)
        || run_id.is_empty()
        || !run_id.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let url = format!(
        "{}/{owner}/{name}/actions/runs/{run_id}",
        server.origin().ascii_serialization()
    );
    let parsed = Url::parse(&url).ok()?;
    (parsed.scheme() == "https" && parsed.host_str().is_some() && url.len() <= MAX_URL_BYTES)
        .then_some(url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::collections::HashMap;

    const PUSH: &[(&str, &str)] = &[
        ("GITHUB_ACTIONS", "true"),
        ("GITHUB_EVENT_NAME", "push"),
        ("GITHUB_RUN_ID", "123"),
        ("GITHUB_RUN_ATTEMPT", "2"),
        ("RUNNER_NAME", "runner-1"),
        ("GITHUB_SERVER_URL", "https://github.com"),
        ("GITHUB_REPOSITORY", "acme/app"),
    ];

    /// `ci` for the push environment with `changes` applied; `None` unsets.
    fn ci(changes: &[(&str, Option<&str>)]) -> Value {
        let mut env: HashMap<String, String> = PUSH
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect();
        for (key, value) in changes {
            match value {
                Some(value) => env.insert(key.to_string(), value.to_string()),
                None => env.remove(*key),
            };
        }
        serde_json::to_value(capture(|name| env.get(name).cloned())).unwrap()
    }

    #[test]
    fn a_push_run_has_every_key() {
        let expected = json!({"provider":"github_actions","event":"push","run_id":"123","attempt":2,"runner":"runner-1","url":"https://github.com/acme/app/actions/runs/123"});
        assert_eq!(ci(&[]), expected);
        // Keys keep the documented order.
        let env: HashMap<_, _> = PUSH.iter().copied().collect();
        assert_eq!(
            serde_json::to_string(&capture(|name| env.get(name).map(|v| v.to_string()))).unwrap(),
            r#"{"provider":"github_actions","event":"push","run_id":"123","attempt":2,"runner":"runner-1","url":"https://github.com/acme/app/actions/runs/123"}"#
        );
        // A trailing slash and an explicit port are still an origin.
        assert_eq!(
            ci(&[("GITHUB_SERVER_URL", Some("https://ghe.example:8443/"))])["url"],
            "https://ghe.example:8443/acme/app/actions/runs/123"
        );
    }

    #[test]
    fn only_a_pull_request_merge_ref_sets_pull_request() {
        assert_eq!(
            ci(&[("GITHUB_REF", Some("refs/pull/42/merge"))])["pull_request"],
            42
        );
        for reference in [
            "refs/pull/0/merge",
            "refs/pull/x/merge",
            "refs/pull/+4/merge",
            "refs/pull/42/head",
            "refs/heads/main",
            "refs/pull//merge",
        ] {
            let ci = ci(&[("GITHUB_REF", Some(reference))]);
            assert!(ci.get("pull_request").is_none(), "{reference}: {ci}");
        }
    }

    #[test]
    fn invalid_values_are_dropped_one_at_a_time() {
        for attempt in ["0", "+1", "1.0", "9007199254740992", "", " 1", "1e3"] {
            let ci = ci(&[("GITHUB_RUN_ATTEMPT", Some(attempt))]);
            assert!(ci.get("attempt").is_none(), "{attempt:?}");
            assert_eq!(ci["runner"], "runner-1");
        }
        assert_eq!(
            ci(&[("GITHUB_RUN_ATTEMPT", Some("9007199254740991"))])["attempt"],
            9007199254740991u64
        );
        let long = "r".repeat(65);
        for (key, field) in [("RUNNER_NAME", "runner"), ("GITHUB_EVENT_NAME", "event")] {
            for value in [long.as_str(), "   ", "", "a\u{1b}b"] {
                let ci = ci(&[(key, Some(value))]);
                assert!(ci.get(field).is_none(), "{key}={value:?}");
                assert_eq!(ci["provider"], "github_actions");
            }
            assert_eq!(ci(&[(key, Some(&long[..64]))])[field], &long[..64]);
        }
        for run_id in [String::new(), "1".repeat(257), "1\n2".into()] {
            let ci = ci(&[("GITHUB_RUN_ID", Some(&run_id))]);
            assert!(ci.get("run_id").is_none() && ci.get("url").is_none());
        }
        // A non-numeric run id is still an identity, but makes no URL.
        let ci_abc = ci(&[("GITHUB_RUN_ID", Some("abc"))]);
        assert_eq!(ci_abc["run_id"], "abc");
        assert!(ci_abc.get("url").is_none());
    }

    #[test]
    fn a_url_needs_an_https_origin_and_an_owner_name_repository() {
        for server in [
            "http://ghe.local",
            "https://github.com/enterprise",
            "https://user@github.com",
            "https://user:pw@github.com",
            "https://github.com?x=1",
            "https://github.com#x",
            "github.com",
            "",
        ] {
            let ci = ci(&[("GITHUB_SERVER_URL", Some(server))]);
            assert!(ci.get("url").is_none(), "{server}: {ci}");
            assert_eq!(ci["run_id"], "123");
        }
        for repository in [
            "acme",
            "acme/app/extra",
            "/app",
            "acme/",
            "acme/a pp",
            "acme/..",
            "acme/a?b",
            "acme/a#b",
            "acme/é",
        ] {
            let ci = ci(&[("GITHUB_REPOSITORY", Some(repository))]);
            assert!(ci.get("url").is_none(), "{repository}: {ci}");
        }
        assert!(ci(&[("GITHUB_SERVER_URL", None)]).get("url").is_none());
        assert_eq!(
            ci(&[("GITHUB_REPOSITORY", Some("A-b_c/d.e-1"))])["url"],
            "https://github.com/A-b_c/d.e-1/actions/runs/123"
        );
    }

    #[test]
    fn ci_is_absent_off_github_actions() {
        for value in [None, Some("1"), Some("TRUE"), Some("")] {
            assert_eq!(ci(&[("GITHUB_ACTIONS", value)]), Value::Null, "{value:?}");
        }
        // Only the provider survives when nothing else is set.
        let only: HashMap<_, _> = [("GITHUB_ACTIONS", "true")].into();
        assert_eq!(
            serde_json::to_value(capture(|name| only.get(name).map(|v| v.to_string()))).unwrap(),
            json!({"provider": "github_actions"})
        );
    }
}
