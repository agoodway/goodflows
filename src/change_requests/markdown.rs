//! Markdown files with YAML front matter: claims files, QA scenario sets,
//! verify run notes and feature files.
//!
//! Files are read, never rewritten, except for the one `cr:` line that
//! `cr open` and `cr claims` fill in. A scenario set is uploaded byte for
//! byte, because its digest covers its raw bytes.

use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::ops::Range;

/// A file split at its front matter: the YAML between a first line `---`
/// and the next line `---`, and the body after it.
pub struct FrontMatter<'a> {
    pub yaml: &'a str,
    /// Byte range of `yaml` in the file.
    pub range: Range<usize>,
    pub body: &'a str,
    /// Byte offset of `body` in the file.
    pub body_start: usize,
}

/// The file's lines with their byte offsets, line endings included.
fn lines(text: &str) -> impl Iterator<Item = (usize, &str)> {
    text.split_inclusive('\n').scan(0, |offset, line| {
        let start = *offset;
        *offset += line.len();
        Some((start, line))
    })
}

fn bare(line: &str) -> &str {
    line.trim_end_matches(['\n', '\r'])
}

pub fn split(text: &str) -> Option<FrontMatter<'_>> {
    let mut lines = lines(text);
    let (_, first) = lines.next()?;
    if bare(first) != "---" {
        return None;
    }
    let start = first.len();
    for (offset, line) in lines {
        if bare(line) == "---" {
            let body_start = offset + line.len();
            return Some(FrontMatter {
                yaml: &text[start..offset],
                range: start..offset,
                body: &text[body_start..],
                body_start,
            });
        }
    }
    None
}

/// The front matter as JSON. YAML anchors, aliases and tags are refused, as
/// the server refuses them in claims.
pub fn parse(yaml: &str, file: &str) -> Result<Value> {
    let value: serde_yaml_ng::Value = serde_yaml_ng::from_str(yaml)
        .map_err(|e| anyhow!("{file}: the front matter is not valid YAML: {e}"))?;
    if !value.is_mapping() {
        bail!("{file}: the front matter is not a YAML mapping");
    }
    to_json(value).with_context(|| format!("{file}: unsupported front matter"))
}

fn to_json(value: serde_yaml_ng::Value) -> Result<Value> {
    use serde_yaml_ng::Value as Y;
    Ok(match value {
        Y::Null => Value::Null,
        Y::Bool(b) => json!(b),
        Y::Number(n) => {
            if let Some(i) = n.as_i64() {
                json!(i)
            } else if let Some(u) = n.as_u64() {
                json!(u)
            } else {
                bail!("only integer numbers are supported")
            }
        }
        Y::String(s) => json!(s),
        Y::Sequence(items) => Value::Array(items.into_iter().map(to_json).collect::<Result<_>>()?),
        Y::Mapping(map) => {
            let mut object = serde_json::Map::new();
            for (key, value) in map {
                let key = match key {
                    Y::String(s) => s,
                    Y::Number(n) => n.to_string(),
                    Y::Bool(b) => b.to_string(),
                    _ => bail!("mapping keys must be strings"),
                };
                if object.insert(key, to_json(value)?).is_some() {
                    bail!("duplicate key");
                }
            }
            Value::Object(object)
        }
        Y::Tagged(_) => bail!("YAML tags are not supported"),
    })
}

/// Refuses anchors and aliases, which serde_yaml_ng expands silently.
///
/// An `&` or `*` is an anchor or alias only where a node starts: at the
/// start of a line's content, after `- `, `? ` or `: `, after `[` or `{`,
/// or after `,` inside a flow collection, and only when a name follows it.
/// Elsewhere, as in `reach: Login & signup`, inside quotes, after `#`, or
/// in the lines of a `|` or `>` block scalar, it is text.
pub fn refuse_anchors(yaml: &str, file: &str) -> Result<()> {
    // The indentation of the node that owns an open block scalar.
    let mut block: Option<usize> = None;
    // Open `[` and `{`, which a flow collection may close on a later line.
    let mut flow = 0usize;
    for line in yaml.lines() {
        let indent = line.len() - line.trim_start_matches(' ').len();
        if let Some(parent) = block {
            if line.trim().is_empty() || indent > parent {
                continue;
            }
            block = None;
        }
        let chars: Vec<char> = line[indent..].chars().collect();
        let blank = |i: usize| chars.get(i).is_none_or(|c| c.is_whitespace());
        let mut in_quote: Option<char> = None;
        let mut node_start = true;
        // The column of the line's innermost block node: a `-` or `?`
        // indicator, or the first key after them. A block scalar's lines
        // are indented deeper than it.
        let mut owner = indent;
        let mut keyed = false;
        for (i, &c) in chars.iter().enumerate() {
            if let Some(q) = in_quote {
                if c == q {
                    in_quote = None;
                }
                continue;
            }
            if c.is_whitespace() {
                continue;
            }
            if c == '#' && (i == 0 || chars[i - 1].is_whitespace()) {
                break;
            }
            if node_start {
                match c {
                    '-' | '?' if blank(i + 1) => {
                        if !keyed {
                            owner = indent + i;
                        }
                        continue;
                    }
                    '&' | '*' if !blank(i + 1) && !",[]{}".contains(chars[i + 1]) => {
                        bail!("{file}: YAML anchors and aliases are not supported")
                    }
                    '[' | '{' => {
                        flow += 1;
                        continue;
                    }
                    '|' | '>' if flow == 0 => {
                        block = Some(owner);
                        break;
                    }
                    '"' | '\'' => in_quote = Some(c),
                    _ => {}
                }
                if flow == 0 && !keyed {
                    owner = indent + i;
                    keyed = true;
                }
                node_start = false;
            }
            match c {
                ':' if blank(i + 1) || (flow > 0 && ",]}".contains(chars[i + 1])) => {
                    node_start = true
                }
                ',' if flow > 0 => node_start = true,
                ']' | '}' if flow > 0 => flow -= 1,
                _ => {}
            }
        }
    }
    Ok(())
}

/// Every problem found in the files a command read, each naming its file.
#[derive(Debug)]
pub struct Invalid(pub Vec<String>);

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0.join("\n"))
    }
}

impl std::error::Error for Invalid {}

/// Collects problems so one pass reports all of them.
#[derive(Default)]
pub struct Problems(pub Vec<String>);

impl Problems {
    pub fn push(&mut self, problem: impl Into<String>) {
        self.0.push(problem.into());
    }

    /// The value of `result`, or `None` with its problems recorded.
    pub fn take<T>(&mut self, result: Result<T>) -> Option<T> {
        result.map_err(|error| self.0.extend(problems(error))).ok()
    }

    pub fn check(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(Invalid(self.0).into())
        }
    }
}

/// The problems an error reports: each one an [`Invalid`] lists, or the
/// error itself.
pub fn problems(error: anyhow::Error) -> Vec<String> {
    match error.downcast::<Invalid>() {
        Ok(Invalid(problems)) => problems,
        Err(error) => vec![format!("{error:#}")],
    }
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// What the CLI reads from a `change-claims/2` file.
pub struct Claims {
    pub cr: Option<u64>,
    pub head: String,
    /// The body's first `# ` heading.
    pub title: Option<String>,
}

pub fn claims(text: &str, file: &str) -> Result<Claims> {
    let front = split(text).ok_or_else(|| {
        anyhow!("{file}: a claims file must start with YAML front matter between --- lines")
    })?;
    let mut problems = Problems::default();
    problems.take(refuse_anchors(front.yaml, file));
    let Some(fields) = problems.take(parse(front.yaml, file)) else {
        return Err(Invalid(problems.0).into());
    };
    if fields["format"] != "change-claims/2" {
        problems.push(format!("{file}: format must be change-claims/2"));
    }
    let cr = match &fields["cr"] {
        Value::Null => None,
        value => {
            let number = value.as_u64().filter(|n| *n > 0);
            if number.is_none() {
                problems.push(format!(
                    "{file}: cr must be a change request number or null"
                ));
            }
            number
        }
    };
    let head = fields["head"].as_str().map(str::to_owned);
    if head.is_none() {
        problems.push(format!(
            "{file}: head must be the commit the claims describe"
        ));
    }
    // The server refuses an id that is not a nonblank string, such as a bare 1.
    for list in ["questions", "decisions"] {
        for (index, entry) in fields[list].as_array().into_iter().flatten().enumerate() {
            if entry["id"].as_str().is_none_or(|id| id.trim().is_empty()) {
                problems.push(format!(
                    "{file}: {list}[{index}].id must be a quoted string, such as id: \"1\""
                ));
            }
        }
    }
    problems.check()?;
    let title = lines(front.body)
        .map(|(_, line)| bare(line))
        .find_map(|line| line.strip_prefix("# "))
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    Ok(Claims {
        cr,
        head: head.unwrap_or_default(),
        title,
    })
}

/// The file with its front matter's `cr:` line set to `number`, and
/// nothing else changed.
pub fn set_cr(text: &str, number: u64, file: &str) -> Result<String> {
    let front = split(text).ok_or_else(|| anyhow!("{file}: no front matter"))?;
    let (offset, line) = lines(front.yaml)
        .find(|(_, line)| line.starts_with("cr:"))
        .ok_or_else(|| anyhow!("{file}: the front matter has no top-level cr: line"))?;
    let ending = &line[bare(line).len()..];
    let start = front.range.start + offset;
    let mut updated = String::with_capacity(text.len() + 8);
    updated.push_str(&text[..start]);
    updated.push_str(&format!("cr: {number}{ending}"));
    updated.push_str(&text[start + line.len()..]);
    Ok(updated)
}

/// A scenario ID in the string form the server compares: a positive integer
/// as decimal, or a string as given.
pub fn scenario_id(value: &Value) -> Option<String> {
    match value {
        Value::Number(n) => n.as_u64().filter(|n| *n > 0).map(|n| n.to_string()),
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

/// What the CLI reads from a `qa-scenarios/2` set.
pub struct ScenarioSet {
    pub source: String,
    pub ids: Vec<String>,
    /// A diff set whose revision includes uncommitted changes (`+dirty`).
    pub dirty: bool,
}

pub fn scenario_set(text: &str, file: &str) -> Result<ScenarioSet> {
    let front = split(text).ok_or_else(|| {
        anyhow!("{file}: a scenario set must start with YAML front matter between --- lines")
    })?;
    let mut problems = Problems::default();
    problems.take(refuse_anchors(front.yaml, file));
    let Some(fields) = problems.take(parse(front.yaml, file)) else {
        return Err(Invalid(problems.0).into());
    };
    if fields["format"] != "qa-scenarios/2" {
        problems.push(format!("{file}: format must be qa-scenarios/2"));
    }
    let source = match fields["source"].as_str() {
        Some(s @ ("intent" | "diff")) => s.to_owned(),
        _ => {
            problems.push(format!("{file}: source must be intent or diff"));
            String::new()
        }
    };
    let scenarios = fields["scenarios"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    if scenarios.is_empty() {
        problems.push(format!("{file}: scenarios must list at least one scenario"));
    }
    let mut ids = Vec::with_capacity(scenarios.len());
    for (index, scenario) in scenarios.iter().enumerate() {
        let Some(id) = scenario_id(&scenario["id"]) else {
            problems.push(format!(
                "{file}: scenarios[{index}]: every scenario needs an id, a positive integer or a nonempty string"
            ));
            continue;
        };
        if ids.contains(&id) {
            problems.push(format!("{file}: scenario id {id} appears twice"));
            continue;
        }
        ids.push(id);
    }
    problems.check()?;
    let dirty = fields["revision"]
        .as_str()
        .is_some_and(|r| r.ends_with("+dirty"));
    Ok(ScenarioSet { source, ids, dirty })
}

/// One `{id, sha256}` per scenario: the SHA-256 of the scenario's section.
/// Sections live under the body's `## Scenarios` heading, up to its next
/// `##` heading. A scenario's section runs from its `### <id>. <name>`
/// heading (`#### <id>.` under a group heading) to the next heading of the
/// same or a higher level. Headings inside fenced code blocks do not count,
/// and neither do group headings, which have no `<id>.`.
pub fn sections(text: &str, ids: &[String], file: &str) -> Result<Vec<Value>> {
    let body_start = split(text).map_or(0, |f| f.body_start);
    // (start offset, level, title) of every ATX heading outside fences.
    let mut headings: Vec<(usize, usize, &str)> = Vec::new();
    let mut fence: Option<&str> = None;
    for (offset, line) in lines(&text[body_start..]) {
        let trimmed = bare(line).trim_start();
        let marker = ["```", "~~~"].into_iter().find(|m| trimmed.starts_with(m));
        match (fence, marker) {
            (Some(open), Some(m)) if m == open => {
                fence = None;
                continue;
            }
            (None, Some(m)) => {
                fence = Some(m);
                continue;
            }
            _ => {}
        }
        if fence.is_some() {
            continue;
        }
        let level = line.bytes().take_while(|b| *b == b'#').count();
        if (1..=6).contains(&level) && line[level..].starts_with(' ') {
            headings.push((body_start + offset, level, bare(&line[level..]).trim()));
        }
    }
    let Some(first) = headings
        .iter()
        .position(|(_, level, title)| *level == 2 && *title == "Scenarios")
    else {
        bail!("{file}: the body has no \"## Scenarios\" heading");
    };
    let last = headings[first + 1..]
        .iter()
        .position(|(_, level, _)| *level <= 2)
        .map_or(headings.len(), |i| first + 1 + i);
    let mut found: BTreeMap<&str, Value> = BTreeMap::new();
    let mut twice = Vec::new();
    for index in first + 1..last {
        let (start, level, title) = headings[index];
        if !(3..=4).contains(&level) {
            continue;
        }
        let Some(id) = title
            .split_once(". ")
            .map(|(id, _)| id)
            .and_then(|id| ids.iter().find(|known| *known == id))
        else {
            continue;
        };
        let end = headings[index + 1..]
            .iter()
            .find(|(_, next, _)| *next <= level)
            .map_or(text.len(), |(offset, _, _)| *offset);
        let section = json!({"id": id, "sha256": sha256_hex(&text.as_bytes()[start..end])});
        if found.insert(id, section).is_some() && !twice.contains(&id) {
            twice.push(id);
        }
    }
    let mut problems = Problems::default();
    for id in twice {
        problems.push(format!("{file}: scenario {id} has more than one heading"));
    }
    let mut sections = Vec::with_capacity(ids.len());
    for id in ids {
        match found.remove(id.as_str()) {
            Some(section) => sections.push(section),
            None => problems.push(format!(
                "{file}: scenario {id} has no \"### {id}. <name>\" heading under ## Scenarios"
            )),
        }
    }
    problems.check()?;
    Ok(sections)
}

/// What `features sync` reads from a verify skill's feature file.
pub struct Feature {
    pub id: String,
    pub title: String,
    /// The `paths` globs of the code the feature covers; empty when the
    /// file sets none, and then sync skips it.
    pub paths: Vec<String>,
}

/// A feature file named `stem`, which need not have front matter.
pub fn feature(text: &str, stem: &str, file: &str) -> Result<Feature> {
    let (fields, body) = match split(text) {
        Some(front) => (parse(front.yaml, file)?, front.body),
        None => (json!({}), text),
    };
    let mut problems = Problems::default();
    let id = match &fields["id"] {
        Value::Null => stem.to_owned(),
        Value::String(id) if !id.trim().is_empty() => id.clone(),
        _ => {
            problems.push(format!("{file}: id must be a nonempty string"));
            stem.to_owned()
        }
    };
    let title = match &fields["title"] {
        Value::Null => None,
        Value::String(title) => Some(title.trim().to_owned()),
        _ => {
            problems.push(format!("{file}: title must be a string"));
            None
        }
    };
    let paths = match &fields["paths"] {
        Value::Null => Vec::new(),
        Value::Array(globs) if globs.iter().all(Value::is_string) => globs
            .iter()
            .filter_map(|glob| glob.as_str().map(str::to_owned))
            .collect(),
        _ => {
            problems.push(format!("{file}: paths must be a list of globs"));
            Vec::new()
        }
    };
    problems.check()?;
    let title = title
        .or_else(|| {
            body.lines()
                .find_map(|l| l.strip_prefix("# "))
                .map(|t| t.trim().to_owned())
        })
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| id.clone());
    Ok(Feature { id, title, paths })
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLAIMS: &str = "---\r\nformat: change-claims/2\r\ncr: null\r\nhead: abc\r\ndoor: two_way\r\n---\r\n\r\n# Add login\r\n\r\ncr: in the body\r\n";

    #[test]
    fn anchors_are_refused_only_where_a_node_starts() {
        let text = |yaml: &str| refuse_anchors(yaml, "cr.md").map(|()| yaml.to_owned());
        for plain in [
            "reach: Login & signup\n",
            "fact: 2 * 3 is 6\n",
            "reach: \"Login & signup\"\n",
            "left_open:\n  - {risk: \"R&D\", note: a & b}\n",
            "reach: x # see &a and *b\n",
            "door_reason: |\n  *Not* reversible\n  &c\nreach: page\n",
            "items:\n  - note: |\n      &a text\n    name: a & b\n",
            "items: [\n  first, a & b,\n  c * d\n]\n",
        ] {
            assert!(text(plain).is_ok(), "{plain:?}");
            // serde_yaml_ng reads these as text too.
            assert!(parse(plain, "cr.md").is_ok(), "{plain:?}");
        }
        for anchored in [
            "base: &b {x: 1}\nother: *b\n",
            "- &one {id: 1}\n",
            "list: [a, *x]\n",
            "map: {k: *x}\n",
            "? *k\n",
            "door_reason: |\n  text\nreach: *r\n",
            "items:\n  - note: |\n      text\n    name: &n hello\n",
            "items: [\n  first, &n hello\n]\n",
            "items: [\n  first,\n  *n\n]\n",
        ] {
            let error = text(anchored).unwrap_err().to_string();
            assert!(
                error.contains("anchors and aliases"),
                "{anchored:?}: {error}"
            );
        }
    }

    #[test]
    fn claims_are_read_and_only_the_cr_line_changes() {
        let claims = claims(CLAIMS, "cr.md").unwrap();
        assert_eq!(claims.cr, None);
        assert_eq!(claims.head, "abc");
        assert_eq!(claims.title.as_deref(), Some("Add login"));
        let updated = set_cr(CLAIMS, 42, "cr.md").unwrap();
        assert_eq!(updated, CLAIMS.replacen("cr: null\r\n", "cr: 42\r\n", 1));
        assert_eq!(super::claims(&updated, "cr.md").unwrap().cr, Some(42));
        assert_eq!(
            set_cr(&updated, 43, "cr.md").unwrap(),
            CLAIMS.replacen("cr: null", "cr: 43", 1)
        );
    }

    #[test]
    fn claims_problems_name_the_file() {
        let no_front = claims("# Title\n", "cr.md").err().unwrap().to_string();
        assert!(
            no_front.starts_with("cr.md: a claims file must start"),
            "{no_front}"
        );
        let old = claims(
            "---\nformat: change-claims/1\ncr: null\nhead: a\n---\n",
            "x.md",
        );
        assert!(old.err().unwrap().to_string().contains("change-claims/2"));
        let alias = "---\nformat: change-claims/2\ncr: null\nhead: &h abc\nreach: *h\n---\n";
        assert!(claims(alias, "x.md")
            .err()
            .unwrap()
            .to_string()
            .contains("anchors"));
        // An ampersand inside a value is not an anchor.
        let text = "---\nformat: change-claims/2\ncr: 7\nhead: abc\nreach: \"R&D * all\"\nnote: Q&A\n---\n";
        assert_eq!(claims(text, "x.md").unwrap().cr, Some(7));
        let ids = "---\nformat: change-claims/2\ncr: null\nhead: abc\nquestions:\n  - {id: \"1\", text: q}\n  - {id: 2, text: q}\ndecisions:\n  - {id: 1, path: a}\n---\n";
        let error = claims(ids, "x.md").err().unwrap().to_string();
        assert!(
            error.contains("questions[1].id must be a quoted string"),
            "{error}"
        );
        assert!(
            error.contains("decisions[0].id must be a quoted string"),
            "{error}"
        );
        assert!(!error.contains("questions[0]"), "{error}");
    }

    #[test]
    fn scenario_sets_refuse_anchors_and_aliases() {
        let set = "---\nformat: qa-scenarios/2\nsource: intent\nscenarios:\n  - &one {id: 1, name: \"Q&A\"}\n  - *one\n---\n";
        let error = scenario_set(set, "qa.md").err().unwrap().to_string();
        assert!(
            error.starts_with("qa.md: YAML anchors and aliases are not supported"),
            "{error}"
        );
        // The alias also repeats scenario 1, and both problems are listed.
        assert!(error.contains("scenario id 1 appears twice"), "{error}");
    }

    /// A grouped diff set in the gs-qa-scenarios template, with a numbered
    /// heading outside `## Scenarios` and a group heading that starts with a
    /// digit.
    const SET: &str = "---\nformat: qa-scenarios/2\nsource: diff\nbranch: \"feat/seats\"\nbase: \"main\"\nrevision: \"a1b2c3d\"\nreview: null\nscenarios:\n  - id: 1\n    name: \"Add a seat\"\n    priority: must\n  - id: 2\n    name: \"Last seat\"\n    priority: should\nregressions: []\n---\n\n# QA: feat/seats\n\n## What changed\n\n### 1. Not a scenario\n\n## Scenarios\n\n### Free plan\n\nSign in as free@example.com.\n\n#### 1. Add a seat (Must)\n\n1. Click **Add seat**.\n   - Expect: 2 seats.\n\n```text\n# not a heading\n```\n\n### 2 seats left\n\nSign in as team@example.com.\n\n#### 2. Last seat (Should)\n\nsteps\n\n## Regression checks\n\n- none\n";

    #[test]
    fn sections_run_from_the_heading_to_the_next_heading_at_its_level() {
        let set = scenario_set(SET, "qa.md").unwrap();
        assert_eq!(set.source, "diff");
        assert_eq!(set.ids, ["1", "2"]);
        let sections = sections(SET, &set.ids, "qa.md").unwrap();
        // Scenario 1 ends at the next group heading, scenario 2 at the
        // heading after ## Scenarios.
        let one = &SET[SET.find("#### 1.").unwrap()..SET.find("### 2 seats").unwrap()];
        let two = &SET[SET.find("#### 2.").unwrap()..SET.find("## Regression").unwrap()];
        assert_eq!(
            Value::Array(sections),
            json!([
                {"id": "1", "sha256": sha256_hex(one.as_bytes())},
                {"id": "2", "sha256": sha256_hex(two.as_bytes())},
            ])
        );
    }

    #[test]
    fn sections_need_exactly_one_heading_per_scenario_under_scenarios() {
        let ids = vec!["1".to_owned(), "2".to_owned()];
        let error = |body: &str| sections(body, &ids, "qa.md").err().unwrap().to_string();
        let missing = error("## Scenarios\n### 1. One\n");
        assert!(missing.contains("scenario 2 has no"), "{missing}");
        let twice = error("## Scenarios\n### 1. One\n### 1. Again\n### 2. Two\n");
        assert!(twice.contains("more than one heading"), "{twice}");
        // `### 10.` is not scenario 1, and `### 1 seat` has no dot.
        assert!(error("## Scenarios\n### 10. Ten\n### 1 seat\n### 2. Two\n").contains("scenario 1"));
        // Only headings under ## Scenarios count.
        assert!(error("### 1. One\n## Scenarios\n### 2. Two\n").contains("scenario 1"));
        assert!(error("### 1. One\n### 2. Two\n").contains("no \"## Scenarios\" heading"));
    }
}
