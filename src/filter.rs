use crate::fields::{Catalog, Row};
use crate::util::Result;
use glob::{MatchOptions, Pattern};
use regex::Regex;
use serde::{Deserialize, Deserializer};
use std::collections::BTreeSet;

#[derive(Clone, Debug)]
pub struct Expression {
    root: Node,
    fields: Vec<(String, usize)>,
}

#[derive(Clone, Debug)]
enum Node {
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
    Compare { field: String, matcher: Matcher },
}

#[derive(Clone, Debug)]
enum Matcher {
    Equal(String),
    Unequal(String),
    Regex { pattern: Regex, negate: bool },
    Glob(Pattern),
}

impl Matcher {
    fn matches(&self, value: &str) -> bool {
        match self {
            Self::Equal(expected) => value == expected,
            Self::Unequal(expected) => value != expected,
            Self::Regex { pattern, negate } => pattern.is_match(value) != *negate,
            Self::Glob(pattern) => pattern.matches_with(
                value,
                MatchOptions {
                    case_sensitive: true,
                    require_literal_separator: false,
                    require_literal_leading_dot: false,
                },
            ),
        }
    }
}
impl Node {
    fn matches<'a>(&self, value: &mut impl FnMut(&str) -> &'a str) -> bool {
        match self {
            Self::And(nodes) => nodes.iter().all(|node| node.matches(value)),
            Self::Or(nodes) => nodes.iter().any(|node| node.matches(value)),
            Self::Not(node) => !node.matches(value),
            Self::Compare { field, matcher } => matcher.matches(value(field)),
        }
    }
}
impl Expression {
    pub fn parse(source: &str) -> Result<Self> {
        if source.len() > 65536 {
            return Err("filter expression exceeds 64 KiB".into());
        }
        let mut parser = Parser {
            source,
            tokens: lex(source)?,
            cursor: 0,
            fields: Vec::new(),
        };
        let root = parser.or(0)?;
        if !matches!(parser.peek().kind, Kind::End) {
            return Err(parser.error("unexpected token; expected and, or, or end of expression"));
        }
        Ok(Self {
            root,
            fields: parser.fields,
        })
    }
    pub fn validate(&self, catalog: &Catalog) -> Result<BTreeSet<String>> {
        self.fields
            .iter()
            .map(|(field, column)| {
                catalog
                    .canonical(field)
                    .map_err(|e| format!("at character {column}: {e}"))
            })
            .collect()
    }
    pub fn matches(&self, row: &Row) -> bool {
        self.root
            .matches(&mut |field| row.get(field).map_or("", String::as_str))
    }
}
impl<'de> Deserialize<'de> for Expression {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let source = String::deserialize(deserializer)?;
        Self::parse(&source).map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug)]
enum Operator {
    Equal,
    Unequal,
    Regex,
    NotRegex,
    Glob,
}
#[derive(Debug)]
enum Kind {
    Field(String),
    Text(String),
    Operator(Operator),
    And,
    Or,
    Not,
    Open,
    Close,
    End,
}
#[derive(Debug)]
struct Token {
    kind: Kind,
    offset: usize,
}
fn diagnostic(source: &str, offset: usize, message: &str) -> String {
    format!(
        "at character {}: {message}",
        source[..offset].chars().count() + 1
    )
}
fn lex(source: &str) -> Result<Vec<Token>> {
    let mut tokens = Vec::new();
    let mut pos = 0;
    while pos < source.len() {
        let c = source[pos..].chars().next().unwrap();
        if c.is_whitespace() {
            pos += c.len_utf8();
            continue;
        }
        let offset = pos;
        let kind = if c == '\'' || c == '"' {
            let quote = c;
            pos += 1;
            let start = pos;
            loop {
                let next = source[pos..]
                    .chars()
                    .next()
                    .ok_or_else(|| diagnostic(source, offset, "unterminated quoted string"))?;
                if next == quote {
                    break;
                }
                pos += next.len_utf8();
                if next == '\\' && quote == '"' {
                    let next = source[pos..]
                        .chars()
                        .next()
                        .ok_or_else(|| diagnostic(source, offset, "unterminated quoted string"))?;
                    pos += next.len_utf8();
                }
            }
            let end = pos;
            pos += 1;
            Kind::Text(if quote == '\'' {
                source[start..end].into()
            } else {
                serde_json::from_str(&source[offset..pos]).map_err(|e| {
                    diagnostic(source, offset, &format!("invalid quoted string: {e}"))
                })?
            })
        } else if c.is_ascii_alphanumeric() || c == '_' {
            pos += 1;
            while pos < source.len()
                && matches!(source.as_bytes()[pos], b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'_' | b'-')
            {
                pos += 1;
            }
            let word = &source[offset..pos];
            match word.to_ascii_lowercase().as_str() {
                "and" => Kind::And,
                "or" => Kind::Or,
                "not" => Kind::Not,
                "glob" => Kind::Operator(Operator::Glob),
                _ => Kind::Field(word.into()),
            }
        } else {
            let (length, kind) = if source[pos..].starts_with("==") {
                (2, Kind::Operator(Operator::Equal))
            } else if source[pos..].starts_with("!=") {
                (2, Kind::Operator(Operator::Unequal))
            } else if source[pos..].starts_with("=~") {
                (2, Kind::Operator(Operator::Regex))
            } else if source[pos..].starts_with("!~") {
                (2, Kind::Operator(Operator::NotRegex))
            } else if source[pos..].starts_with("&&") {
                (2, Kind::And)
            } else if source[pos..].starts_with("||") {
                (2, Kind::Or)
            } else {
                match c {
                    '(' => (1, Kind::Open),
                    ')' => (1, Kind::Close),
                    '!' => (1, Kind::Not),
                    _ => {
                        return Err(diagnostic(
                            source,
                            offset,
                            "unexpected character; quote values and use ==, !=, =~, !~, or glob",
                        ));
                    }
                }
            };
            pos += length;
            kind
        };
        tokens.push(Token { kind, offset });
    }
    tokens.push(Token {
        kind: Kind::End,
        offset: source.len(),
    });
    Ok(tokens)
}

struct Parser<'a> {
    source: &'a str,
    tokens: Vec<Token>,
    cursor: usize,
    fields: Vec<(String, usize)>,
}
impl Parser<'_> {
    fn peek(&self) -> &Token {
        &self.tokens[self.cursor]
    }
    fn error(&self, message: &str) -> String {
        diagnostic(self.source, self.peek().offset, message)
    }
    fn or(&mut self, depth: usize) -> Result<Node> {
        let mut nodes = vec![self.and(depth)?];
        while matches!(self.peek().kind, Kind::Or) {
            self.cursor += 1;
            nodes.push(self.and(depth)?);
        }
        Ok(if nodes.len() == 1 {
            nodes.remove(0)
        } else {
            Node::Or(nodes)
        })
    }
    fn and(&mut self, depth: usize) -> Result<Node> {
        let mut nodes = vec![self.unary(depth)?];
        while matches!(self.peek().kind, Kind::And) {
            self.cursor += 1;
            nodes.push(self.unary(depth)?);
        }
        Ok(if nodes.len() == 1 {
            nodes.remove(0)
        } else {
            Node::And(nodes)
        })
    }
    fn unary(&mut self, depth: usize) -> Result<Node> {
        if depth >= 64 {
            return Err(self.error("filter nesting exceeds 64 levels"));
        }
        if matches!(self.peek().kind, Kind::Not) {
            self.cursor += 1;
            return Ok(Node::Not(Box::new(self.unary(depth + 1)?)));
        }
        if matches!(self.peek().kind, Kind::Open) {
            self.cursor += 1;
            let node = self.or(depth + 1)?;
            if !matches!(self.peek().kind, Kind::Close) {
                return Err(self.error("expected closing parenthesis"));
            }
            self.cursor += 1;
            return Ok(node);
        }
        let Kind::Field(field) = &self.peek().kind else {
            return Err(self.error("expected a Slurm field name, not, or parenthesis"));
        };
        let field = field.clone();
        self.fields.push((
            field.clone(),
            self.source[..self.peek().offset].chars().count() + 1,
        ));
        self.cursor += 1;
        let Kind::Operator(operator) = self.peek().kind else {
            return Err(self.error("expected ==, !=, =~, !~, or glob after field"));
        };
        self.cursor += 1;
        let Kind::Text(value) = &self.peek().kind else {
            return Err(self.error("expected a single- or double-quoted value"));
        };
        let matcher = match operator {
            Operator::Equal => Matcher::Equal(value.clone()),
            Operator::Unequal => Matcher::Unequal(value.clone()),
            Operator::Regex | Operator::NotRegex => Matcher::Regex {
                pattern: Regex::new(value)
                    .map_err(|e| self.error(&format!("invalid regular expression: {e}")))?,
                negate: matches!(operator, Operator::NotRegex),
            },
            Operator::Glob => Matcher::Glob(
                Pattern::new(value).map_err(|e| self.error(&format!("invalid glob: {e}")))?,
            ),
        };
        self.cursor += 1;
        Ok(Node::Compare {
            field: field.to_ascii_lowercase(),
            matcher,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn row() -> Row {
        [
            ("name", "训练-42"),
            ("state", "RUNNING"),
            ("partition", "debug-a"),
            ("reason", "None"),
            ("cpus-per-task", "8"),
        ]
        .into_iter()
        .map(|(k, v)| (k.into(), v.into()))
        .collect()
    }
    #[test]
    fn precedence_grouping_and_negation() {
        for (expression, expected) in [
            (
                r#"Name == "missing" or State == "RUNNING" and Partition == "other""#,
                false,
            ),
            (
                r#"Name == "训练-42" or State == "PENDING" and Partition == "other""#,
                true,
            ),
            (
                r#"(Name == "训练-42" or State == "PENDING") and Partition == "other""#,
                false,
            ),
            (r#"not (Name glob "test-*" or State != "RUNNING")"#, true),
            (
                r#"!(NAME !~ "训练" || state != "RUNNING") && cpus-per-task == "8""#,
                true,
            ),
            (r#"NOT NOT Name == "训练-42" AND State == "RUNNING""#, true),
        ] {
            assert_eq!(
                Expression::parse(expression).unwrap().matches(&row()),
                expected,
                "{expression}"
            );
        }
    }
    #[test]
    fn matchers_quotes_and_raw_values() {
        for expression in [
            r#"Name =~ "42""#,
            r#"Name =~ '^训练-\d+$'"#,
            r#"Name =~ "^训练-\\d+$""#,
            r#"Name glob "??-4[0-9]""#,
            r#"State =~ "(?i)^running$""#,
            r#"Reason == "None""#,
        ] {
            assert!(
                Expression::parse(expression).unwrap().matches(&row()),
                "{expression}"
            );
        }
        assert!(
            !Expression::parse(r#"Name glob "42""#)
                .unwrap()
                .matches(&row())
        );
        let row = [("name".into(), " a\nb\"c\\d ".into())]
            .into_iter()
            .collect();
        assert!(
            Expression::parse(r#"Name == " a\nb\"c\\d ""#)
                .unwrap()
                .matches(&row)
        );
        assert!(
            Expression::parse(r#"Name =~ '(?s)^ a.b"c\\d $'"#)
                .unwrap()
                .matches(&row)
        );
        assert!(
            Expression::parse("Name == ''")
                .unwrap()
                .matches(&[("name".into(), String::new())].into_iter().collect())
        );
    }
    #[test]
    fn catalog_checks_all_branches_and_retains_field_positions() {
        let catalog = Catalog::parse("JobId Name State", false).unwrap();
        let expression = Expression::parse(r#"Name == "训练-42" or Typo == "ignored""#).unwrap();
        assert!(expression.matches(&row()));
        let error = expression.validate(&catalog).unwrap_err();
        assert!(error.contains("at character 20"), "{error}");
        assert!(error.contains("unknown job field"));
        assert_eq!(
            Expression::parse("name == 'a' or NAME == 'b'")
                .unwrap()
                .validate(&catalog)
                .unwrap()
                .len(),
            1
        );
    }
    #[test]
    fn boolean_operations_short_circuit() {
        for expression in [
            "Name == 'ok' or State == 'forbidden'",
            "Name != 'ok' and State == 'forbidden'",
        ] {
            let expr = Expression::parse(expression).unwrap();
            let mut count = 0;
            expr.root.matches(&mut |field| {
                count += 1;
                assert_eq!(field, "name");
                "ok"
            });
            assert_eq!(count, 1);
        }
    }
    #[test]
    fn rejects_malformed_or_oversized_expressions() {
        for source in [
            "",
            " ",
            "Name",
            "Name == value",
            "Name = 'x'",
            "Name =~ '['",
            "Name glob '['",
            "Name == 'x' and",
            "Name == 'x' junk",
            "(Name == 'x'",
            "Name == 'x')",
            "Name == 'unterminated",
            r#"Name == "bad\q""#,
            "Name > '5'",
        ] {
            assert!(
                Expression::parse(source).unwrap_err().contains("character"),
                "{source}"
            );
        }
        assert!(
            Expression::parse(&format!("{}Name == 'x'{}", "(".repeat(65), ")".repeat(65)))
                .unwrap_err()
                .contains("nesting")
        );
        assert!(Expression::parse(&" ".repeat(65537)).is_err());
    }
}
