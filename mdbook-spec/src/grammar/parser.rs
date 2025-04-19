//! A parser of the ENBF-like grammar.

use super::{Characters, Expression, ExpressionKind, Grammar, Production};
use regex::{Captures, Regex};
use std::fmt;
use std::fmt::Display;
use std::path::Path;
use std::sync::LazyLock;

struct Parser<'a> {
    input: &'a str,
    index: usize,
}

pub struct Error {
    message: String,
    line: String,
    lineno: usize,
    col: usize,
}

impl Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::result::Result<(), std::fmt::Error> {
        let lineno = format!("{}", self.lineno);
        let space = " ".repeat(lineno.len() + 1);
        let col = " ".repeat(self.col);
        let line = &self.line;
        let message = &self.message;
        write!(f, "\n{space}|\n{lineno} | {line}\n{space}|{col}^ {message}")
    }
}

macro_rules! bail {
    ($parser:expr, $($arg:tt)*) => {{
        let mut msg = String::new();
        fmt::write(&mut msg, format_args!($($arg)*)).unwrap();
        return Err($parser.error(msg));
    }};
}

type Result<T> = std::result::Result<T, Error>;

pub fn parse_grammar(
    input: &str,
    grammar: &mut Grammar,
    category: &str,
    path: &Path,
) -> Result<()> {
    let mut parser = Parser { input, index: 0 };
    loop {
        let p = parser.parse_production(category, path)?;
        grammar.name_order.push(p.name.clone());
        if let Some(dupe) = grammar.productions.insert(p.name.clone(), p) {
            bail!(parser, "duplicate production {} in grammar", dupe.name);
        }
        parser.take_while(&|ch| ch == '\n');
        if parser.eof() {
            break;
        }
    }
    Ok(())
}

impl Parser<'_> {
    fn take_while(&mut self, f: &dyn Fn(char) -> bool) -> &str {
        let mut upper = 0;
        let i = self.index;
        let mut ci = self.input[i..].chars();
        while let Some(ch) = ci.next() {
            if !f(ch) {
                break;
            }
            upper += ch.len_utf8();
        }
        self.index += upper;
        &self.input[i..i + upper]
    }

    /// If the input matches the given regex, it is returned and the head is moved forward.
    ///
    /// Note that regexes must start with `^`.
    fn take_re(&mut self, re: &Regex) -> Option<Captures<'_>> {
        if let Some(cap) = re.captures(&self.input[self.index..]) {
            self.index += cap[0].len();
            Some(cap)
        } else {
            None
        }
    }

    /// Returns whether or not the given string is next, and advances the head if it is.
    fn take_str(&mut self, s: &str) -> bool {
        if self.input[self.index..].starts_with(s) {
            self.index += s.len();
            true
        } else {
            false
        }
    }

    /// Returns the next byte, or None if eof.
    fn peek(&mut self) -> Option<u8> {
        if self.index >= self.input.len() {
            None
        } else {
            Some(self.input.as_bytes()[self.index])
        }
    }

    fn eof(&mut self) -> bool {
        self.index >= self.input.len()
    }

    /// Expects the next input to be the given string, and advances the head.
    fn expect(&mut self, s: &str, err: &str) -> Result<()> {
        if !self.input[self.index..].starts_with(s) {
            bail!(self, "{err}");
        };
        self.index += s.len();
        Ok(())
    }

    fn error(&mut self, message: String) -> Error {
        let (line, lineno, col) = translate_position(self.input, self.index);
        Error {
            message,
            line: line.to_string(),
            lineno,
            col,
        }
    }

    /// Advances zero or more spaces.
    fn space0(&mut self) -> &str {
        self.take_while(&|ch| ch == ' ')
    }

    fn parse_production(&mut self, category: &str, path: &Path) -> Result<Production> {
        let is_root = self.parse_is_root();
        self.space0();
        let name = self
            .parse_name()
            .ok_or_else(|| self.error("expected production name".to_string()))?;
        self.expect(" ->", "expected -> arrow")?;
        let Some(expression) = self.parse_expression()? else {
            bail!(self, "expected an expression");
        };
        Ok(Production {
            name,
            category: category.to_string(),
            expression,
            path: path.to_owned(),
            is_root,
        })
    }

    fn parse_is_root(&mut self) -> bool {
        self.take_str("@root")
    }

    fn parse_name(&mut self) -> Option<String> {
        let name = self.take_while(&|c: char| c.is_alphanumeric() || c == '_');
        if name.is_empty() {
            None
        } else {
            Some(name.to_string())
        }
    }

    fn parse_expression(&mut self) -> Result<Option<Expression>> {
        static ALT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^ *\| *").unwrap());

        let mut es = Vec::new();
        loop {
            let Some(e) = self.parse_seq()? else { break };
            es.push(e);
            if self.take_re(&ALT_RE).is_none() {
                break;
            }
        }
        match es.len() {
            0 => Ok(None),
            1 => Ok(Some(es.pop().unwrap())),
            _ => Ok(Some(Expression {
                kind: ExpressionKind::Alt(es),
                suffix: None,
                footnote: None,
            })),
        }
    }

    fn parse_seq(&mut self) -> Result<Option<Expression>> {
        let mut es = Vec::new();
        loop {
            self.space0();
            let Some(e) = self.parse_expr1()? else {
                break;
            };
            es.push(e);
        }
        match es.len() {
            0 => Ok(None),
            1 => Ok(Some(es.pop().unwrap())),
            _ => Ok(Some(Expression {
                kind: ExpressionKind::Sequence(es),
                suffix: None,
                footnote: None,
            })),
        }
    }

    fn parse_expr1(&mut self) -> Result<Option<Expression>> {
        let Some(next) = self.peek() else {
            return Ok(None);
        };

        let mut kind = if self.take_str("U+") {
            self.parse_unicode()?
        } else if self.input[self.index..]
            .chars()
            .next()
            .map(|ch| ch.is_alphanumeric())
            .unwrap_or(false)
        {
            self.parse_nonterminal()
                .expect("first char already checked")
        } else if self.take_str("\n") {
            if self.eof() || self.take_str("\n") {
                return Ok(None);
            }
            let space = self.take_while(&|ch| ch == ' ');
            if space.len() == 0 {
                bail!(self, "expected indentation on next line");
            }
            ExpressionKind::Break(space.len())
        } else if next == b'`' {
            self.parse_terminal()?
        } else if next == b'[' {
            self.parse_charset()?
        } else if next == b'<' {
            self.parse_prose()?
        } else if next == b'(' {
            self.parse_grouped()?
        } else if next == b'~' {
            self.parse_neg_expression()?
        } else {
            return Ok(None);
        };

        static REPEAT_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^ ?(\*\?|\+\?|\?|\*|\+)").unwrap());
        static RANGE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^\{([0-9]+)?\.\.([0-9]+)?\}").unwrap());
        if let Some(cap) = self.take_re(&REPEAT_RE) {
            kind = match &cap[1] {
                "?" => ExpressionKind::Optional(box_kind(kind)),
                "*" => ExpressionKind::Repeat(box_kind(kind)),
                "*?" => ExpressionKind::RepeatNonGreedy(box_kind(kind)),
                "+" => ExpressionKind::RepeatPlus(box_kind(kind)),
                "+?" => ExpressionKind::RepeatPlusNonGreedy(box_kind(kind)),
                s => panic!("unexpected `{s}`"),
            };
        } else if let Some(cap) = self.take_re(&RANGE_RE) {
            let a = cap.get(1).map(|m| m.as_str().parse::<u32>().unwrap());
            let b = cap.get(2).map(|m| m.as_str().parse::<u32>().unwrap());
            match (a, b) {
                (Some(a), Some(b)) if b < a => bail!(self, "range {a}..{b} is malformed"),
                _ => {}
            }
            kind = ExpressionKind::RepeatRange(box_kind(kind), a, b);
        }

        let suffix = self.parse_suffix()?;
        let footnote = self.parse_footnote()?;

        Ok(Some(Expression {
            kind,
            suffix,
            footnote,
        }))
    }

    fn parse_nonterminal(&mut self) -> Option<ExpressionKind> {
        let nt = self.parse_name()?;
        Some(ExpressionKind::Nt(nt))
    }

    fn parse_terminal(&mut self) -> Result<ExpressionKind> {
        static TERMINAL_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^`([^`\n]+)`").unwrap());
        match self.take_re(&TERMINAL_RE) {
            Some(cap) => Ok(ExpressionKind::Terminal(cap[1].to_string())),
            None => bail!(self, "unterminated terminal, expected closing backtick"),
        }
    }

    fn parse_charset(&mut self) -> Result<ExpressionKind> {
        self.expect("[", "expected opening [")?;
        let mut characters = Vec::new();
        loop {
            self.space0();
            let Some(ch) = self.parse_characters() else {
                break;
            };
            characters.push(ch);
        }
        if characters.is_empty() {
            bail!(self, "expected at least one character in character group");
        }
        self.space0();
        self.expect("]", "expected closing ]")?;
        Ok(ExpressionKind::Charset(characters))
    }

    fn parse_characters(&mut self) -> Option<Characters> {
        static RANGE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^`(.)`-`(.)`").unwrap());
        static TERMINAL_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new("^`([^`\n]+)`").unwrap());
        if let Some(cap) = self.take_re(&RANGE_RE) {
            let a = cap[1].chars().next().unwrap();
            let b = cap[2].chars().next().unwrap();
            Some(Characters::Range(a, b))
        } else if let Some(cap) = self.take_re(&TERMINAL_RE) {
            Some(Characters::Terminal(cap[1].to_string()))
        } else {
            let name = self.parse_name()?;
            Some(Characters::Named(name))
        }
    }

    fn parse_prose(&mut self) -> Result<ExpressionKind> {
        static PROSE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^<([^>\n]+)>").unwrap());
        match self.take_re(&PROSE_RE) {
            Some(cap) => Ok(ExpressionKind::Prose(cap[1].to_string())),
            None => bail!(self, "unterminated prose, expected closing `>`"),
        }
    }

    fn parse_grouped(&mut self) -> Result<ExpressionKind> {
        self.expect("(", "expected opening `(`")?;
        self.space0();
        let Some(e) = self.parse_expression()? else {
            bail!(self, "expected expression in parenthesized group");
        };
        self.space0();
        self.expect(")", "expected closing `)`")?;
        Ok(ExpressionKind::Grouped(Box::new(e)))
    }

    fn parse_neg_expression(&mut self) -> Result<ExpressionKind> {
        self.expect("~", "expected ~")?;
        let Some(next) = self.peek() else {
            bail!(self, "expected expression after ~");
        };
        let kind = match next {
            b'[' => self.parse_charset()?,
            b'`' => self.parse_terminal()?,
            _ => self.parse_nonterminal().ok_or_else(|| {
                self.error("expected a charset, terminal, or name after ~ negation".to_string())
            })?,
        };
        Ok(ExpressionKind::NegExpression(box_kind(kind)))
    }

    fn parse_unicode(&mut self) -> Result<ExpressionKind> {
        static UNICODE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Z0-9]{4}").unwrap());

        match self.take_re(&UNICODE_RE) {
            Some(s) => Ok(ExpressionKind::Unicode(s[0].to_string())),
            None => bail!(self, "expected 4 hexadecimal uppercase digits after U+"),
        }
    }

    fn parse_suffix(&mut self) -> Result<Option<String>> {
        if !self.take_str(" _") {
            return Ok(None);
        }
        let mut in_backtick = false;
        let start = self.index;
        loop {
            let Some(next) = self.peek() else {
                bail!(self, "failed to find end of _ suffixed text");
            };
            self.index += 1;
            match next {
                b'\n' => bail!(self, "failed to find end of _ suffixed text"),
                b'`' => in_backtick = !in_backtick,
                b'_' if !in_backtick => {
                    if self
                        .peek()
                        .map(|b| matches!(b, b'\n' | b' '))
                        .unwrap_or(true)
                    {
                        break;
                    }
                }
                _ => {}
            }
        }
        Ok(Some(self.input[start..self.index - 1].to_string()))
    }

    fn parse_footnote(&mut self) -> Result<Option<String>> {
        static FOOTNOTE_RE: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^([^\]\n]+)]").unwrap());
        if !self.take_str("[^") {
            return Ok(None);
        }
        match self.take_re(&FOOTNOTE_RE) {
            Some(cap) => Ok(Some(cap[1].to_string())),
            None => bail!(self, "unterminated footnote, expected closing `]`"),
        }
    }
}

fn box_kind(kind: ExpressionKind) -> Box<Expression> {
    Box::new(Expression {
        kind,
        suffix: None,
        footnote: None,
    })
}

/// Helper to translate a byte index to a `(line, line_no, col_no)` (1-based).
fn translate_position(input: &str, index: usize) -> (&str, usize, usize) {
    if input.is_empty() {
        return ("", 0, 0);
    }
    let index = index.min(input.len());

    let mut line_start = 0;
    let mut line_number = 0;
    for line in input.lines() {
        let line_end = line_start + line.len();
        if index >= line_start && index <= line_end {
            let column_number = index - line_start + 1;
            return (line, line_number + 1, column_number);
        }
        line_start = line_end + 1;
        line_number += 1;
    }
    ("", line_number + 1, 0)
}

#[test]
fn translate_tests() {
    assert_eq!(translate_position("", 0), ("", 0, 0));
    assert_eq!(translate_position("test", 0), ("test", 1, 1));
    assert_eq!(translate_position("test", 3), ("test", 1, 4));
    assert_eq!(translate_position("test", 4), ("test", 1, 5));
    assert_eq!(translate_position("test\ntest2", 4), ("test", 1, 5));
    assert_eq!(translate_position("test\ntest2", 5), ("test2", 2, 1));
    assert_eq!(translate_position("test\ntest2\n", 11), ("", 3, 0));
}
