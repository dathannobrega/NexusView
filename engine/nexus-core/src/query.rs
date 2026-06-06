//! Search query language and evaluation (RF-04).
//!
//! Supported syntax:
//! - **Implicit AND**: `error timeout` matches rows containing both.
//! - **Operators**: `AND`, `OR`, `NOT` (case-insensitive) and parentheses.
//! - **Phrases**: `"disk full"` matches the literal substring including spaces.
//! - **Regex**: `/error\d+/` (delimited by slashes), ARM-optimized via the
//!   byte-level `regex` engine.
//! - **Column scope**: `host:web01` restricts a term to the `host` column;
//!   `host:/web\d+/` scopes a regex. A `name:` prefix is only treated as a scope
//!   when `name` matches a known column, so timestamps like `12:34:56` stay
//!   literal.
//!
//! Plain terms match case-insensitively (analyst-friendly); regex terms respect
//! the pattern's own flags (`(?i)` for case-insensitive).
//!
//! Evaluation runs against raw line bytes with no per-row allocation: global
//! terms scan the whole line, scoped terms inspect a single field slice.

use crate::encoding::ascii_icontains;
use crate::error::{NexusError, Result};
use crate::parser::{self, FieldRanges};

/// A compiled, reusable search predicate.
pub struct Query {
    root: Node,
    /// True when any term is column-scoped, so the evaluator knows it must split
    /// the line into fields first.
    has_scoped: bool,
}

enum Node {
    And(Box<Node>, Box<Node>),
    Or(Box<Node>, Box<Node>),
    Not(Box<Node>),
    /// Case-insensitive substring; `needle` is pre-lowercased ASCII.
    Substr {
        col: Option<usize>,
        needle: Vec<u8>,
    },
    Regex {
        col: Option<usize>,
        re: regex::bytes::Regex,
    },
    /// Empty query — matches everything.
    Always,
}

impl Query {
    /// Compile a query string against the dataset's `columns` (used to resolve
    /// `name:` scopes case-insensitively).
    pub fn compile(input: &str, columns: &[String]) -> Result<Self> {
        let tokens = tokenize(input);
        if tokens.is_empty() {
            return Ok(Query {
                root: Node::Always,
                has_scoped: false,
            });
        }
        let mut parser = Parser {
            tokens: &tokens,
            pos: 0,
            columns,
            has_scoped: false,
        };
        let root = parser.parse_or()?;
        if parser.pos != tokens.len() {
            return Err(NexusError::InvalidQuery(
                "unexpected trailing tokens (check parentheses)".into(),
            ));
        }
        let has_scoped = parser.has_scoped;
        Ok(Query { root, has_scoped })
    }

    /// Does this query touch column-scoped terms? When false the evaluator can
    /// skip field splitting entirely.
    #[inline]
    pub fn has_scoped(&self) -> bool {
        self.has_scoped
    }

    /// If this query is exactly one substring term — global or column-scoped —
    /// return its `(column, ASCII-lowercased needle)`. This is the case the Bloom
    /// index accelerates (RF-04): the per-block trigram filter is a valid
    /// superset test for a scoped field too. Boolean/regex queries return `None`.
    pub fn single_substring(&self) -> Option<(Option<usize>, &[u8])> {
        match &self.root {
            Node::Substr { col, needle } => Some((*col, needle)),
            _ => None,
        }
    }

    /// Evaluate against a single record.
    ///
    /// `line` is the record bytes (line terminators already trimmed). `fields`
    /// must be `Some` when [`Query::has_scoped`] is true.
    #[inline]
    pub fn matches(&self, line: &[u8], fields: Option<&FieldRanges>, quote: u8) -> bool {
        eval(&self.root, line, fields, quote)
    }
}

fn eval(node: &Node, line: &[u8], fields: Option<&FieldRanges>, quote: u8) -> bool {
    match node {
        Node::And(a, b) => eval(a, line, fields, quote) && eval(b, line, fields, quote),
        Node::Or(a, b) => eval(a, line, fields, quote) || eval(b, line, fields, quote),
        Node::Not(a) => !eval(a, line, fields, quote),
        Node::Always => true,
        Node::Substr { col, needle } => match col {
            None => ascii_icontains(line, needle),
            Some(c) => field_slice(line, fields, *c, quote)
                .map(|fb| ascii_icontains(fb, needle))
                .unwrap_or(false),
        },
        Node::Regex { col, re } => match col {
            None => re.is_match(line),
            Some(c) => field_slice(line, fields, *c, quote)
                .map(|fb| re.is_match(fb))
                .unwrap_or(false),
        },
    }
}

/// Resolve a scoped column index to its (unquoted) field byte slice.
#[inline]
fn field_slice<'a>(
    line: &'a [u8],
    fields: Option<&FieldRanges>,
    col: usize,
    quote: u8,
) -> Option<&'a [u8]> {
    let &(s, e) = fields?.get(col)?;
    Some(parser::unquote_borrowed(&line[s..e], quote))
}

// --------------------------------------------------------------------------
// Tokenizer
// --------------------------------------------------------------------------

#[derive(Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    And,
    Or,
    Not,
    Term(String),
}

fn tokenize(input: &str) -> Vec<Tok> {
    let chars: Vec<char> = input.chars().collect();
    let mut toks = Vec::new();
    let mut i = 0;
    let n = chars.len();

    while i < n {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
        } else if c == '(' {
            toks.push(Tok::LParen);
            i += 1;
        } else if c == ')' {
            toks.push(Tok::RParen);
            i += 1;
        } else if c == '"' {
            // Quoted phrase: literal until the next quote (or end of input).
            i += 1;
            let mut term = String::new();
            while i < n && chars[i] != '"' {
                term.push(chars[i]);
                i += 1;
            }
            if i < n {
                i += 1; // consume closing quote
            }
            toks.push(Tok::Term(term));
        } else {
            // Bare word: read until whitespace or a parenthesis. A `/.../` regex
            // literal or a `"..."` quoted value may contain spaces, so a slash or
            // quote toggles a region in which whitespace and parentheses are
            // consumed verbatim. This keeps `/worker 3/`, `col:/web \d+/`, and
            // `col:"web 01"` as single tokens.
            let mut word = String::new();
            let mut in_regex = false;
            let mut in_quote = false;
            while i < n {
                let ch = chars[i];
                if ch == '"' {
                    in_quote = !in_quote;
                    word.push(ch);
                    i += 1;
                } else if ch == '/' && !in_quote {
                    in_regex = !in_regex;
                    word.push(ch);
                    i += 1;
                } else if in_regex || in_quote {
                    word.push(ch);
                    i += 1;
                } else if ch.is_whitespace() || ch == '(' || ch == ')' {
                    break;
                } else {
                    word.push(ch);
                    i += 1;
                }
            }
            match word.to_ascii_uppercase().as_str() {
                "AND" => toks.push(Tok::And),
                "OR" => toks.push(Tok::Or),
                "NOT" => toks.push(Tok::Not),
                _ => toks.push(Tok::Term(word)),
            }
        }
    }
    toks
}

// --------------------------------------------------------------------------
// Recursive-descent parser
//   or    := and ( OR and )*
//   and   := unary ( (AND)? unary )*      (implicit AND between adjacent terms)
//   unary := NOT unary | primary
//   primary := '(' or ')' | term
// --------------------------------------------------------------------------

struct Parser<'a> {
    tokens: &'a [Tok],
    pos: usize,
    columns: &'a [String],
    has_scoped: bool,
}

impl<'a> Parser<'a> {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }

    fn parse_or(&mut self) -> Result<Node> {
        let mut lhs = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.pos += 1;
            let rhs = self.parse_and()?;
            lhs = Node::Or(Box::new(lhs), Box::new(rhs));
        }
        Ok(lhs)
    }

    fn parse_and(&mut self) -> Result<Node> {
        let mut lhs = self.parse_unary()?;
        loop {
            match self.peek() {
                Some(Tok::And) => {
                    self.pos += 1;
                    let rhs = self.parse_unary()?;
                    lhs = Node::And(Box::new(lhs), Box::new(rhs));
                }
                // Implicit AND: another primary begins without an operator.
                Some(Tok::Not | Tok::Term(_) | Tok::LParen) => {
                    let rhs = self.parse_unary()?;
                    lhs = Node::And(Box::new(lhs), Box::new(rhs));
                }
                _ => break,
            }
        }
        Ok(lhs)
    }

    fn parse_unary(&mut self) -> Result<Node> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.pos += 1;
            let inner = self.parse_unary()?;
            Ok(Node::Not(Box::new(inner)))
        } else {
            self.parse_primary()
        }
    }

    fn parse_primary(&mut self) -> Result<Node> {
        match self.peek() {
            Some(Tok::LParen) => {
                self.pos += 1;
                let inner = self.parse_or()?;
                match self.peek() {
                    Some(Tok::RParen) => {
                        self.pos += 1;
                        Ok(inner)
                    }
                    _ => Err(NexusError::InvalidQuery("missing closing ')'".into())),
                }
            }
            Some(Tok::Term(term)) => {
                // Clone the term text so the borrow from `peek()` ends before we
                // advance `pos` — keeps this branch fully infallible.
                let term = term.clone();
                self.pos += 1;
                self.make_term(&term)
            }
            Some(Tok::RParen) => Err(NexusError::InvalidQuery("unexpected ')'".into())),
            Some(Tok::And | Tok::Or | Tok::Not) => {
                Err(NexusError::InvalidQuery("operator without operand".into()))
            }
            None => Err(NexusError::InvalidQuery("unexpected end of query".into())),
        }
    }

    /// Build a leaf node from a term, resolving an optional `column:` scope and
    /// `/regex/` form.
    fn make_term(&mut self, raw: &str) -> Result<Node> {
        let (col, value) = self.split_scope(raw);
        if col.is_some() {
            self.has_scoped = true;
        }
        // A scoped value may be wrapped in quotes to allow spaces (col:"web 01").
        let value = strip_surrounding_quotes(value);

        if let Some(pattern) = as_regex(value) {
            let re = regex::bytes::Regex::new(pattern)
                .map_err(|e| NexusError::InvalidQuery(format!("bad regex: {e}")))?;
            Ok(Node::Regex { col, re })
        } else {
            Ok(Node::Substr {
                col,
                needle: value.to_ascii_lowercase().into_bytes(),
            })
        }
    }

    /// Split `name:value` into a column index + value. `name` may be a column
    /// name (case-insensitive) or `#N` for a column index — the latter lets the
    /// UI scope reliably even when a column name contains spaces. Otherwise the
    /// whole token is an unscoped value.
    fn split_scope<'t>(&self, raw: &'t str) -> (Option<usize>, &'t str) {
        if let Some(idx) = raw.find(':') {
            let (name, rest) = raw.split_at(idx);
            let value = &rest[1..];
            if !value.is_empty() {
                if let Some(num) = name.strip_prefix('#') {
                    if let Ok(col) = num.parse::<usize>() {
                        if col < self.columns.len() {
                            return (Some(col), value);
                        }
                    }
                }
                if let Some(col) = self
                    .columns
                    .iter()
                    .position(|c| c.eq_ignore_ascii_case(name))
                {
                    return (Some(col), value);
                }
            }
        }
        (None, raw)
    }
}

/// Strip one pair of surrounding double quotes, if present.
fn strip_surrounding_quotes(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"' {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

/// Recognize the `/pattern/` regex form and return the inner pattern.
fn as_regex(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 && bytes[0] == b'/' && bytes[bytes.len() - 1] == b'/' {
        Some(&value[1..value.len() - 1])
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cols() -> Vec<String> {
        vec!["host".into(), "message".into()]
    }

    fn run(query: &str, line: &[u8]) -> bool {
        let columns = cols();
        let q = Query::compile(query, &columns).unwrap();
        let fields = if q.has_scoped() {
            Some(parser::field_ranges(line, b',', b'"'))
        } else {
            None
        };
        q.matches(line, fields.as_ref(), b'"')
    }

    #[test]
    fn implicit_and() {
        assert!(run("disk error", b"web01,disk error here"));
        assert!(!run("disk error", b"web01,disk ok"));
    }

    #[test]
    fn explicit_operators() {
        assert!(run("error OR warning", b"x,warning state"));
        assert!(run("error AND NOT timeout", b"x,error fatal"));
        assert!(!run("error AND NOT timeout", b"x,error timeout"));
    }

    #[test]
    fn parentheses() {
        assert!(run("(a OR b) AND c", b"x,a c"));
        assert!(!run("(a OR b) AND c", b"x,a d"));
    }

    #[test]
    fn case_insensitive_substring() {
        assert!(run("ERROR", b"x,an error occurred"));
    }

    #[test]
    fn regex_term() {
        assert!(run(r"/err\d+/", b"x,err42 raised"));
        assert!(!run(r"/err\d+/", b"x,error"));
    }

    #[test]
    fn regex_with_spaces() {
        assert!(run(r"/worker 3/", b"x,done by worker 3 now"));
        assert!(!run(r"/worker 3/", b"x,done by worker 4 now"));
    }

    #[test]
    fn scoped_regex_with_spaces() {
        // scoped to the `message` column (index 1)
        assert!(run(r"message:/disk full/", b"web01,disk full alert"));
        assert!(!run(r"message:/disk full/", b"disk full,ok")); // match only in host, not message
    }

    #[test]
    fn scoped_quoted_value_with_space() {
        // column-filter use case: host:"web 01" must keep the space literal.
        assert!(run("host:\"web 01\"", b"web 01,some message"));
        assert!(!run("host:\"web 01\"", b"web02,web 01 here")); // only in host column
    }

    #[test]
    fn scope_by_column_index() {
        // #0 = host, #1 = message — robust even when names contain spaces.
        assert!(run("#0:web01", b"web01,login"));
        assert!(!run("#1:web01", b"web01,login")); // web01 is in host, not message
        assert!(run("#1:\"disk full\"", b"web01,disk full now"));
    }

    #[test]
    fn column_scope() {
        assert!(run("host:web01", b"web01,some error"));
        assert!(!run("host:web01", b"web02,web01 in message")); // scoped to host col
    }

    #[test]
    fn colon_value_not_a_scope() {
        // "12:34:56" — "12" is not a column, so it stays literal.
        assert!(run("12:34:56", b"x,event at 12:34:56"));
    }

    #[test]
    fn empty_query_matches_all() {
        assert!(run("", b"anything"));
        assert!(run("   ", b"anything"));
    }

    #[test]
    fn invalid_regex_errors() {
        let columns = cols();
        assert!(Query::compile("/(/", &columns).is_err());
    }

    #[test]
    fn unbalanced_paren_errors() {
        let columns = cols();
        assert!(Query::compile("(a OR b", &columns).is_err());
    }
}
