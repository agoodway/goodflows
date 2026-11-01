//! A deliberately small, scalar-only expression language. Parsing never performs lookups.
//!
//! Precedence (high to low): `!`, ordering, equality, `&&`, `||`, `??`.
//! Binary operators associate left to right. Only numbers and strings support ordering;
//! strings use case-sensitive lexicographic order. Booleans support equality only, and
//! null may be compared for equality with any scalar. No implicit conversions occur.
//!
//! Integer literals must fit i64 (negative) or u64 (nonnegative); overflow is rejected
//! rather than rounded. Decimal/exponent literals use serde_json's finite f64 numbers.
//! Source size, parser recursion, and expression tree depth (including operator chains)
//! are bounded to keep parsing, evaluation, cloning, and destruction safe.

use anyhow::{bail, Context, Result};
use serde_json::{Number, Value};
use std::cmp::Ordering;

const MAX_SOURCE_BYTES: usize = 16 * 1024;
const MAX_DEPTH: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reference {
    Env(String),
    Needs { job: String, key: String },
    Step { step: String, key: String },
}

#[derive(Debug, Clone)]
pub struct Expression {
    root: Node,
    references: Vec<Reference>,
}

impl Expression {
    /// Validate the entire source, including namespaces in branches evaluation may skip.
    pub fn parse(source: &str) -> Result<Self> {
        if source.len() > MAX_SOURCE_BYTES {
            bail!("expression exceeds {MAX_SOURCE_BYTES} bytes");
        }
        let mut parser = Parser {
            source,
            position: 0,
            references: Vec::new(),
        };
        let root = parser.expression(1, 0)?;
        parser.whitespace();
        if parser.position != source.len() {
            bail!("unexpected input at byte {}", parser.position);
        }
        Ok(Self {
            root,
            references: parser.references,
        })
    }

    /// All syntactic references in source order, including duplicates and lazy branches.
    pub fn references(&self) -> Vec<&Reference> {
        self.references.iter().collect()
    }

    /// Resolve only evaluated references. The caller supplies null for unset environment
    /// variables and errors for missing outputs. Arrays and objects are always rejected.
    pub fn evaluate(&self, lookup: &impl Fn(&Reference) -> Result<Value>) -> Result<Value> {
        self.root.evaluate(&self.references, lookup)
    }

    /// Evaluate and require an actual JSON boolean, without truthiness or coercion.
    pub fn evaluate_bool(&self, lookup: &impl Fn(&Reference) -> Result<Value>) -> Result<bool> {
        boolean(self.evaluate(lookup)?)
    }
}

#[derive(Debug, Clone)]
struct Node {
    kind: NodeKind,
    depth: usize,
}

#[derive(Debug, Clone)]
enum NodeKind {
    Literal(Value),
    Reference(usize),
    Not(Box<Node>),
    Binary(Operator, Box<Node>, Box<Node>),
}

#[derive(Debug, Clone, Copy)]
enum Operator {
    Coalesce,
    Or,
    And,
    Equal,
    NotEqual,
    Less,
    LessEqual,
    Greater,
    GreaterEqual,
}

impl Operator {
    fn precedence(self) -> u8 {
        match self {
            Self::Coalesce => 1,
            Self::Or => 2,
            Self::And => 3,
            Self::Equal | Self::NotEqual => 4,
            Self::Less | Self::LessEqual | Self::Greater | Self::GreaterEqual => 5,
        }
    }
}

impl Node {
    fn new(kind: NodeKind) -> Result<Self> {
        let depth = match &kind {
            NodeKind::Literal(_) | NodeKind::Reference(_) => 1,
            NodeKind::Not(child) => child.depth + 1,
            NodeKind::Binary(_, left, right) => left.depth.max(right.depth) + 1,
        };
        if depth > MAX_DEPTH {
            bail!("expression tree exceeds depth {MAX_DEPTH} (including operator chains)");
        }
        Ok(Self { kind, depth })
    }

    fn evaluate(
        &self,
        references: &[Reference],
        lookup: &impl Fn(&Reference) -> Result<Value>,
    ) -> Result<Value> {
        match &self.kind {
            NodeKind::Literal(value) => Ok(value.clone()),
            NodeKind::Reference(index) => {
                let reference = &references[*index];
                let value = lookup(reference)
                    .with_context(|| format!("looking up expression reference {reference:?}"))?;
                if value.is_array() || value.is_object() {
                    bail!("expression reference {reference:?} must resolve to a scalar JSON value");
                }
                Ok(value)
            }
            NodeKind::Not(child) => Ok(Value::Bool(!boolean(child.evaluate(references, lookup)?)?)),
            NodeKind::Binary(operator, left, right) => {
                let left = left.evaluate(references, lookup)?;
                // Do not evaluate even the type of a skipped branch.
                match operator {
                    Operator::Coalesce if !left.is_null() => return Ok(left),
                    Operator::Coalesce => return right.evaluate(references, lookup),
                    Operator::And if !boolean(left.clone())? => return Ok(Value::Bool(false)),
                    Operator::Or if boolean(left.clone())? => return Ok(Value::Bool(true)),
                    Operator::And | Operator::Or => {
                        return Ok(Value::Bool(boolean(right.evaluate(references, lookup)?)?));
                    }
                    _ => {}
                }
                let right = right.evaluate(references, lookup)?;
                Ok(Value::Bool(compare(*operator, &left, &right)?))
            }
        }
    }
}

fn boolean(value: Value) -> Result<bool> {
    value
        .as_bool()
        .context("expression requires a boolean value (no implicit truthiness)")
}

fn compare(operator: Operator, left: &Value, right: &Value) -> Result<bool> {
    let equality = matches!(operator, Operator::Equal | Operator::NotEqual);
    if equality && (left.is_null() || right.is_null()) {
        let equal = left.is_null() && right.is_null();
        return Ok(if matches!(operator, Operator::Equal) {
            equal
        } else {
            !equal
        });
    }
    let ordering = match (left, right) {
        (Value::Number(left), Value::Number(right)) => compare_numbers(left, right),
        (Value::String(left), Value::String(right)) => left.cmp(right),
        (Value::Bool(left), Value::Bool(right)) if equality => left.cmp(right),
        // Operand values may contain secrets from the lookup callback.
        _ => bail!("incompatible expression operand types for {operator:?}"),
    };
    Ok(match operator {
        Operator::Equal => ordering == Ordering::Equal,
        Operator::NotEqual => ordering != Ordering::Equal,
        Operator::Less => ordering == Ordering::Less,
        Operator::LessEqual => ordering != Ordering::Greater,
        Operator::Greater => ordering == Ordering::Greater,
        Operator::GreaterEqual => ordering != Ordering::Less,
        _ => unreachable!("logical operators are evaluated lazily"),
    })
}

// Never cast an integer to f64: doing so would collapse adjacent integers above 2^53.
fn compare_numbers(left: &Number, right: &Number) -> Ordering {
    fn integer(number: &Number) -> Option<(bool, u64)> {
        if let Some(value) = number.as_i64() {
            Some((value < 0, value.unsigned_abs()))
        } else {
            number.as_u64().map(|value| (false, value))
        }
    }

    fn integer_float(negative: bool, magnitude: u64, float: f64) -> Ordering {
        let float_negative = float < 0.0;
        if negative != float_negative {
            return if negative {
                Ordering::Less
            } else {
                Ordering::Greater
            };
        }
        let absolute = float.abs();
        let ordering = if absolute >= 18_446_744_073_709_551_616.0 {
            // 2^64 is exactly representable; no u64 can reach it.
            Ordering::Less
        } else {
            let whole = absolute as u64;
            match magnitude.cmp(&whole) {
                Ordering::Equal if absolute.fract() != 0.0 => Ordering::Less,
                ordering => ordering,
            }
        };
        if negative {
            ordering.reverse()
        } else {
            ordering
        }
    }

    match (integer(left), integer(right)) {
        (Some((left_negative, left)), Some((right_negative, right))) => {
            match (left_negative, right_negative) {
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (true, true) => right.cmp(&left),
                (false, false) => left.cmp(&right),
            }
        }
        (Some((negative, magnitude)), None) => {
            integer_float(negative, magnitude, right.as_f64().expect("JSON number"))
        }
        (None, Some((negative, magnitude))) => {
            integer_float(negative, magnitude, left.as_f64().expect("JSON number")).reverse()
        }
        (None, None) => left
            .as_f64()
            .unwrap()
            .partial_cmp(&right.as_f64().unwrap())
            .expect("JSON numbers are finite"),
    }
}

struct Parser<'a> {
    source: &'a str,
    position: usize,
    references: Vec<Reference>,
}

impl Parser<'_> {
    fn whitespace(&mut self) {
        while self.peek().is_some_and(|byte| byte.is_ascii_whitespace()) {
            self.position += 1;
        }
    }

    fn peek(&self) -> Option<u8> {
        self.source.as_bytes().get(self.position).copied()
    }

    fn take(&mut self, text: &str) -> bool {
        self.whitespace();
        if self.source[self.position..].starts_with(text) {
            self.position += text.len();
            true
        } else {
            false
        }
    }

    fn expression(&mut self, minimum: u8, depth: usize) -> Result<Node> {
        if depth > MAX_DEPTH {
            bail!("expression nesting exceeds {MAX_DEPTH}");
        }
        self.whitespace();
        let mut left = if self.take("!") {
            Node::new(NodeKind::Not(Box::new(self.expression(6, depth + 1)?)))?
        } else if self.take("(") {
            let node = self.expression(1, depth + 1)?;
            if !self.take(")") {
                bail!("expected ')' at byte {}", self.position);
            }
            node
        } else {
            self.atom()?
        };
        loop {
            self.whitespace();
            let Some((operator, length)) = self.operator() else {
                break;
            };
            if operator.precedence() < minimum {
                break;
            }
            self.position += length;
            let right = self.expression(operator.precedence() + 1, depth + 1)?;
            left = Node::new(NodeKind::Binary(operator, Box::new(left), Box::new(right)))?;
        }
        Ok(left)
    }

    fn operator(&self) -> Option<(Operator, usize)> {
        let rest = &self.source[self.position..];
        [
            ("??", Operator::Coalesce),
            ("||", Operator::Or),
            ("&&", Operator::And),
            ("==", Operator::Equal),
            ("!=", Operator::NotEqual),
            ("<=", Operator::LessEqual),
            (">=", Operator::GreaterEqual),
            ("<", Operator::Less),
            (">", Operator::Greater),
        ]
        .into_iter()
        .find_map(|(text, operator)| rest.starts_with(text).then_some((operator, text.len())))
    }

    fn atom(&mut self) -> Result<Node> {
        let value = match self.peek() {
            Some(b'"' | b'\'') => Value::String(self.string()?),
            Some(b'-' | b'0'..=b'9') => self.number()?,
            Some(b'a'..=b'z' | b'A'..=b'Z' | b'_') => {
                let root = self.name()?;
                match root.as_str() {
                    "true" => Value::Bool(true),
                    "false" => Value::Bool(false),
                    "null" => Value::Null,
                    "env" | "needs" | "steps" => return self.reference(&root),
                    _ => bail!("unknown expression name or namespace {root:?}"),
                }
            }
            _ => bail!(
                "expected scalar literal, reference, '!', or '(' at byte {}",
                self.position
            ),
        };
        Node::new(NodeKind::Literal(value))
    }

    fn name(&mut self) -> Result<String> {
        self.whitespace();
        let start = self.position;
        while self
            .peek()
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
        {
            self.position += 1;
        }
        if start == self.position {
            bail!("expected a reference name at byte {}", self.position);
        }
        Ok(self.source[start..self.position].to_owned())
    }

    fn reference(&mut self, root: &str) -> Result<Node> {
        let mut parts = Vec::new();
        loop {
            if self.take(".") {
                parts.push(self.name()?);
            } else if self.take("[") {
                self.whitespace();
                if !matches!(self.peek(), Some(b'"' | b'\'')) {
                    bail!(
                        "reference brackets require a quoted name at byte {}",
                        self.position
                    );
                }
                parts.push(self.string()?);
                if !self.take("]") {
                    bail!("expected ']' at byte {}", self.position);
                }
            } else {
                break;
            }
            if parts.len() > 3 {
                bail!("too many components in {root} reference");
            }
        }
        let reference = match (root, parts.as_slice()) {
            ("env", [name]) => Reference::Env(name.clone()),
            ("needs", [job, outputs, key]) if outputs == "outputs" => {
                Reference::Needs { job: job.clone(), key: key.clone() }
            }
            ("steps", [step, outputs, key]) if outputs == "outputs" => {
                Reference::Step { step: step.clone(), key: key.clone() }
            }
            _ => bail!("invalid {root} reference: expected env.NAME, needs.JOB.outputs.KEY, or steps.ID.outputs.KEY"),
        };
        let index = self.references.len();
        self.references.push(reference);
        Node::new(NodeKind::Reference(index))
    }

    fn number(&mut self) -> Result<Value> {
        let start = self.position;
        while self.peek().is_some_and(|byte| {
            byte.is_ascii_digit() || matches!(byte, b'-' | b'+' | b'.' | b'e' | b'E')
        }) {
            self.position += 1;
        }
        let text = &self.source[start..self.position];
        let value: Value = serde_json::from_str(text)
            .with_context(|| format!("invalid JSON number at byte {start}"))?;
        // serde_json otherwise silently converts overflowing integer literals to lossy f64.
        if !text.contains(['.', 'e', 'E']) {
            let number = if text.starts_with('-') {
                Number::from(
                    text.parse::<i64>()
                        .context("integer literal is outside the signed 64-bit range")?,
                )
            } else {
                Number::from(
                    text.parse::<u64>()
                        .context("integer literal is outside the unsigned 64-bit range")?,
                )
            };
            return Ok(Value::Number(number));
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String> {
        let start = self.position;
        let quote = self.peek().context("expected quoted string")?;
        self.position += 1;
        let mut json = String::from("\"");
        loop {
            let byte = self.peek().context("unterminated expression string")?;
            if byte == quote {
                self.position += 1;
                json.push('"');
                return serde_json::from_str(&json)
                    .with_context(|| format!("invalid string at byte {start}"));
            }
            if byte == b'\\' {
                self.position += 1;
                let escaped = self.peek().context("unterminated string escape")?;
                if quote == b'\'' && escaped == b'\'' {
                    json.push('\'');
                    self.position += 1;
                    continue;
                }
                json.push('\\');
                // Keep JSON escapes intact; serde_json validates and decodes them below.
                let character = self.source[self.position..].chars().next().unwrap();
                json.push(character);
                self.position += character.len_utf8();
            } else {
                let character = self.source[self.position..].chars().next().unwrap();
                if character == '"' {
                    json.push('\\');
                }
                json.push(character);
                self.position += character.len_utf8();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::cell::RefCell;

    fn evaluate(source: &str) -> Result<Value> {
        Expression::parse(source)?.evaluate(&|reference| bail!("unexpected lookup {reference:?}"))
    }

    #[test]
    fn scalar_literals_and_strings() {
        for (source, expected) in [
            ("true", json!(true)),
            ("false", json!(false)),
            ("null", Value::Null),
            ("-12.5e2", json!(-1250.0)),
            ("42", json!(42)),
            (r#""hi\n\u263a\"""#, json!("hi\n☺\"")),
            (r#"'it\'s "fine"'"#, json!("it's \"fine\"")),
            (r#""\ud83d\ude00""#, json!("😀")),
        ] {
            assert_eq!(evaluate(source).unwrap(), expected, "{source}");
        }
    }

    #[test]
    fn precedence_and_parentheses() {
        for source in [
            "!false && 1 < 2 == true || false",
            "false || true && true",
            "(false || true) && !(3 >= 4)",
            "null ?? false || true",
            "false == false != false",
            "null ?? null ?? true",
            "1 <= 1 && 2 > 1 && 2 >= 2 && 'a' < 'b'",
        ] {
            assert_eq!(evaluate(source).unwrap(), json!(true), "{source}");
        }
        assert_eq!(evaluate("false ?? true || true").unwrap(), json!(false));
    }

    #[test]
    fn references_include_lazy_branches_in_source_order() {
        let expression = Expression::parse(
            r#"env.MY-VAR ?? needs["build job"].outputs['a.b'] ?? steps.step-1.outputs.result ?? env.MY-VAR"#,
        ).unwrap();
        assert_eq!(
            expression.references(),
            vec![
                &Reference::Env("MY-VAR".into()),
                &Reference::Needs {
                    job: "build job".into(),
                    key: "a.b".into()
                },
                &Reference::Step {
                    step: "step-1".into(),
                    key: "result".into()
                },
                &Reference::Env("MY-VAR".into()),
            ]
        );
        let quoted = Expression::parse(r#"needs["🦀"]["outputs"]["any/key"]"#).unwrap();
        assert_eq!(
            quoted.references(),
            vec![&Reference::Needs {
                job: "🦀".into(),
                key: "any/key".into()
            }]
        );
        assert!(Expression::parse("env.123-name").is_ok());
    }

    #[test]
    fn logical_operators_and_coalescing_are_lazy() {
        for (source, expected) in [
            ("false && needs.missing.outputs.x", json!(false)),
            ("true || steps.missing.outputs.x", json!(true)),
            ("'present' ?? env.MISSING", json!("present")),
            ("false ?? env.MISSING", json!(false)),
            ("0 ?? env.MISSING", json!(0)),
            ("'' ?? env.MISSING", json!("")),
            ("true || (1 < 'bad type')", json!(true)),
        ] {
            assert_eq!(evaluate(source).unwrap(), expected, "{source}");
        }
        let calls = RefCell::new(Vec::new());
        let expression = Expression::parse("env.A ?? env.B ?? env.C").unwrap();
        let result = expression
            .evaluate(&|reference| {
                calls.borrow_mut().push(reference.clone());
                Ok(if reference == &Reference::Env("B".into()) {
                    json!(7)
                } else {
                    Value::Null
                })
            })
            .unwrap();
        assert_eq!(result, json!(7));
        assert_eq!(
            *calls.borrow(),
            vec![Reference::Env("A".into()), Reference::Env("B".into())]
        );
    }

    #[test]
    fn strict_types_and_null_equality() {
        for source in [
            "'x' == 1",
            "false != 0",
            "1 && true",
            "false || null",
            "!0",
            "true < false",
            "null <= null",
            "null < 1",
        ] {
            assert!(evaluate(source).is_err(), "{source}");
        }
        for (source, expected) in [
            ("null == null", true),
            ("null != null", false),
            ("null == 1", false),
            ("'x' != null", true),
            ("false != null", true),
        ] {
            assert_eq!(evaluate(source).unwrap(), json!(expected), "{source}");
        }
        assert!(Expression::parse("1")
            .unwrap()
            .evaluate_bool(&|_| unreachable!())
            .is_err());
        assert!(Expression::parse("true")
            .unwrap()
            .evaluate_bool(&|_| unreachable!())
            .unwrap());
        let unset = Expression::parse("env.UNSET != null").unwrap();
        assert!(!unset.evaluate_bool(&|_| Ok(Value::Null)).unwrap());
    }

    #[test]
    fn lookup_errors_and_non_scalar_values_are_not_hidden() {
        for source in ["env.X", "true && env.X", "false || env.X", "null ?? env.X"] {
            let expression = Expression::parse(source).unwrap();
            let error = expression
                .evaluate(&|_| bail!("missing output"))
                .unwrap_err();
            assert!(format!("{error:#}").contains("missing output"));
            for value in [json!([]), json!({"a": 1})] {
                assert!(expression.evaluate(&|_| Ok(value.clone())).is_err());
            }
        }
    }

    #[test]
    fn exact_large_integer_and_mixed_float_comparisons() {
        for source in [
            "9007199254740992 != 9007199254740993",
            "9007199254740993 > 9007199254740992.0",
            "9007199254740992.0 < 9007199254740993",
            "18446744073709551615 > 18446744073709551614",
            "18446744073709551615 < 18446744073709551616.0",
            "-9223372036854775808 < -9223372036854775807",
            "-9223372036854775807 > -9223372036854775808.0",
            "9223372036854775808 > 9223372036854775807",
            "-1 < 0 && -1 < 0.0 && 0 > -0.5",
            "-2 < -1.5 && -1 > -1.5 && 1 < 1.5 && 2 > 1.5",
            "0 == -0.0 && 1 == 1.0 && -1 == -1.0",
            "1e100 > 18446744073709551615 && -1e100 < -9223372036854775808",
            "0.25 < 0.5 && 0.5 == 0.5",
        ] {
            assert_eq!(evaluate(source).unwrap(), json!(true), "{source}");
        }
    }

    #[test]
    fn rejects_invalid_or_unsupported_syntax_during_parse() {
        for source in [
            "",
            " ",
            "true false",
            "true!",
            "true & false",
            "false | true",
            "null ? 1",
            "1 = 1",
            "[]",
            "{}",
            "[1]",
            "1 + 2",
            "1-2",
            "1 / 2",
            "1e",
            "01",
            "+1",
            "1.",
            "NaN",
            "Infinity",
            "1e999",
            "18446744073709551616",
            "-9223372036854775809",
            "env",
            "env.",
            "env.A.B",
            "env[0]",
            "env['x'",
            "env..x",
            "needs.job.x.key",
            "needs.job.outputs",
            "steps.job.outputs.x.y",
            "step.x.outputs.y",
            "unknown.name",
            "env.X()",
            "${env.X}",
            "true || unknown.X",
            "true || needs.bad",
            "(true",
            "true)",
            "!",
            "true &&",
            "true === true",
            "null ??",
            r#""unterminated"#,
            r#""\q""#,
            r#""\ud800""#,
            "\"line\nbreak\"",
            "'unterminated",
        ] {
            assert!(
                Expression::parse(source).is_err(),
                "unexpectedly accepted {source:?}"
            );
        }
    }

    #[test]
    fn malformed_unicode_input_never_panics() {
        let alphabet = [
            "é", "🦀", "\\", "'", "\"", "\0", "\n", "[", "]", "!", "(", ")", "?", "=", "0", "e",
            ".", "env", "true", " ",
        ];
        let mut state = 0x6a09_e667_f3bc_c909_u64;
        for length in 0..80 {
            for _ in 0..50 {
                let mut source = String::new();
                for _ in 0..length {
                    state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
                    source.push_str(alphabet[(state >> 32) as usize % alphabet.len()]);
                }
                if let Ok(expression) = Expression::parse(&source) {
                    let _ = expression.evaluate(&|_| Ok(Value::Null));
                }
            }
        }
    }

    #[test]
    fn bounded_source_nesting_and_operator_chains() {
        assert!(Expression::parse(&" ".repeat(MAX_SOURCE_BYTES + 1)).is_err());
        assert!(Expression::parse(&format!("{}true", " ".repeat(MAX_SOURCE_BYTES - 4))).is_ok());
        let nested = format!("{}true{}", "(".repeat(MAX_DEPTH), ")".repeat(MAX_DEPTH));
        assert_eq!(evaluate(&nested).unwrap(), json!(true));
        assert!(Expression::parse(&format!("({nested})")).is_err());
        assert!(Expression::parse(&format!("{}true", "!".repeat(MAX_DEPTH))).is_err());
        let chain = std::iter::repeat_n("true", MAX_DEPTH)
            .collect::<Vec<_>>()
            .join(" && ");
        assert_eq!(evaluate(&chain).unwrap(), json!(true));
        assert!(Expression::parse(&format!("{chain} && true")).is_err());
        let long_chain = std::iter::repeat_n("0", 4000)
            .collect::<Vec<_>>()
            .join("??");
        assert!(Expression::parse(&long_chain).is_err());
    }
}
