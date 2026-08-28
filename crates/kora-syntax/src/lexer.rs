//! Hand-written lexer with Python-style INDENT/DEDENT synthesis.

use crate::error::SyntaxError;
use crate::token::{Span, Token, TokenKind};

pub struct Lexer<'a> {
    src: &'a str,
    bytes: &'a [u8],
    pos: usize,
    line: u32,
    col: u32,
    /// Stack of active indentation widths; always starts with 0.
    indents: Vec<usize>,
    /// Depth of open (), [], {} — newlines inside brackets are ignored.
    bracket_depth: usize,
    /// True at the start of a logical line (need to measure indentation).
    at_line_start: bool,
    tokens: Vec<Token>,
}

impl<'a> Lexer<'a> {
    pub fn new(src: &'a str) -> Self {
        Lexer {
            src,
            bytes: src.as_bytes(),
            pos: 0,
            line: 1,
            col: 1,
            indents: vec![0],
            bracket_depth: 0,
            at_line_start: true,
            tokens: Vec::new(),
        }
    }

    pub fn tokenize(mut self) -> Result<Vec<Token>, SyntaxError> {
        while self.pos < self.bytes.len() {
            if self.at_line_start && self.bracket_depth == 0 {
                self.handle_indentation()?;
                if self.pos >= self.bytes.len() {
                    break;
                }
            }
            let c = self.bytes[self.pos];
            match c {
                b' ' | b'\t' => {
                    self.advance();
                }
                b'#' => self.skip_comment(),
                b'\n' => self.handle_newline(),
                b'\r' => {
                    self.advance();
                }
                b'0'..=b'9' => self.lex_number()?,
                b'"' | b'\'' => self.lex_string(c)?,
                b'f' if self.peek_at(1) == Some(b'"') || self.peek_at(1) == Some(b'\'') => {
                    self.lex_fstring()?
                }
                c if c == b'_' || c.is_ascii_alphabetic() => self.lex_ident(),
                _ => self.lex_operator()?,
            }
        }

        // Close any dangling logical line, then unwind indentation.
        self.emit_final_newline();
        while self.indents.len() > 1 {
            self.indents.pop();
            self.push(TokenKind::Dedent, self.here(0));
        }
        self.push(TokenKind::Eof, self.here(0));
        Ok(self.tokens)
    }

    // --- indentation ---

    fn handle_indentation(&mut self) -> Result<(), SyntaxError> {
        loop {
            let start = self.pos;
            let mut width = 0usize;
            while let Some(c) = self.peek() {
                match c {
                    b' ' => {
                        width += 1;
                        self.advance();
                    }
                    b'\t' => {
                        return Err(SyntaxError::new(
                            "tab character used for indentation",
                            self.here(1),
                        )
                        .with_hint("Kora uses spaces for indentation — configure your editor to insert spaces"));
                    }
                    _ => break,
                }
            }
            match self.peek() {
                // Blank line or comment-only line: not a logical line, restart.
                Some(b'\n') => {
                    self.advance_newline();
                    continue;
                }
                Some(b'\r') => {
                    self.advance();
                    continue;
                }
                Some(b'#') => {
                    self.skip_comment();
                    continue;
                }
                Option::None => {
                    self.at_line_start = false;
                    return Ok(());
                }
                Some(_) => {
                    self.at_line_start = false;
                    let current = *self.indents.last().unwrap();
                    if width > current {
                        self.indents.push(width);
                        self.push(TokenKind::Indent, self.span_from(start));
                    } else if width < current {
                        while *self.indents.last().unwrap() > width {
                            self.indents.pop();
                            self.push(TokenKind::Dedent, self.span_from(start));
                        }
                        if *self.indents.last().unwrap() != width {
                            return Err(SyntaxError::new(
                                "unindent does not match any outer indentation level",
                                self.span_from(start),
                            )
                            .with_hint("check that this line lines up with an earlier block"));
                        }
                    }
                    return Ok(());
                }
            }
        }
    }

    fn handle_newline(&mut self) {
        if self.bracket_depth == 0 {
            // Collapse repeated newlines: only emit if last token wasn't one.
            let emit = !matches!(
                self.tokens.last().map(|t| &t.kind),
                Some(TokenKind::Newline) | Some(TokenKind::Indent) | Option::None
            );
            if emit {
                self.push(TokenKind::Newline, self.here(1));
            }
            self.at_line_start = true;
        }
        self.advance_newline();
    }

    fn emit_final_newline(&mut self) {
        let needs = !matches!(
            self.tokens.last().map(|t| &t.kind),
            Some(TokenKind::Newline) | Option::None
        );
        if needs {
            self.push(TokenKind::Newline, self.here(0));
        }
    }

    // --- literals ---

    fn lex_number(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        let mut is_float = false;
        while let Some(c) = self.peek() {
            match c {
                b'0'..=b'9' | b'_' => self.advance(),
                b'.' if !is_float && matches!(self.peek_at(1), Some(b'0'..=b'9')) => {
                    is_float = true;
                    self.advance();
                }
                _ => break,
            }
        }
        let text: String = self.src[start..self.pos].replace('_', "");
        let span = self.span_from(start);
        if is_float {
            let v: f64 = text
                .parse()
                .map_err(|_| SyntaxError::new(format!("invalid number `{text}`"), span))?;
            self.push(TokenKind::Float(v), span);
        } else {
            let v: i64 = text
                .parse()
                .map_err(|_| SyntaxError::new(format!("integer `{text}` is too large"), span))?;
            self.push(TokenKind::Int(v), span);
        }
        Ok(())
    }

    fn lex_string(&mut self, quote: u8) -> Result<(), SyntaxError> {
        let start = self.pos;
        self.advance(); // opening quote
        let mut out = String::new();
        loop {
            match self.peek() {
                Option::None | Some(b'\n') => {
                    return Err(
                        SyntaxError::new("unterminated string", self.span_from(start))
                            .with_hint(format!("add a closing {}", quote as char)),
                    );
                }
                Some(b'\\') => {
                    self.advance();
                    out.push(self.escape_char()?);
                }
                Some(c) if c == quote => {
                    self.advance();
                    break;
                }
                Some(_) => {
                    let ch = self.advance_char();
                    out.push(ch);
                }
            }
        }
        self.push(TokenKind::Str(out), self.span_from(start));
        Ok(())
    }

    /// f-strings: literal text plus `{expr}` holes. Expression sources are
    /// captured as raw text and parsed later by the parser.
    fn lex_fstring(&mut self) -> Result<(), SyntaxError> {
        let start = self.pos;
        self.advance(); // 'f'
        let quote = self.bytes[self.pos];
        self.advance(); // opening quote
        let mut parts: Vec<String> = Vec::new();
        let mut exprs: Vec<String> = Vec::new();
        let mut cur = String::new();
        loop {
            match self.peek() {
                Option::None | Some(b'\n') => {
                    return Err(SyntaxError::new(
                        "unterminated f-string",
                        self.span_from(start),
                    ));
                }
                Some(b'\\') => {
                    self.advance();
                    cur.push(self.escape_char()?);
                }
                Some(b'{') if self.peek_at(1) == Some(b'{') => {
                    self.advance();
                    self.advance();
                    cur.push('{');
                }
                Some(b'}') if self.peek_at(1) == Some(b'}') => {
                    self.advance();
                    self.advance();
                    cur.push('}');
                }
                Some(b'{') => {
                    self.advance();
                    let expr_start = self.pos;
                    let mut depth = 1usize;
                    while depth > 0 {
                        match self.peek() {
                            Option::None | Some(b'\n') => {
                                return Err(SyntaxError::new(
                                    "unterminated `{` in f-string",
                                    self.span_from(expr_start),
                                )
                                .with_hint("add a matching `}`"));
                            }
                            Some(b'{') => {
                                depth += 1;
                                self.advance();
                            }
                            Some(b'}') => {
                                depth -= 1;
                                if depth > 0 {
                                    self.advance();
                                }
                            }
                            Some(_) => {
                                self.advance_char();
                            }
                        }
                    }
                    let expr_src = self.src[expr_start..self.pos].to_string();
                    if expr_src.trim().is_empty() {
                        return Err(SyntaxError::new(
                            "empty expression in f-string",
                            self.span_from(expr_start),
                        ));
                    }
                    self.advance(); // closing '}'
                    parts.push(std::mem::take(&mut cur));
                    exprs.push(expr_src);
                }
                Some(c) if c == quote => {
                    self.advance();
                    break;
                }
                Some(_) => {
                    let ch = self.advance_char();
                    cur.push(ch);
                }
            }
        }
        parts.push(cur);
        self.push(TokenKind::FStr { parts, exprs }, self.span_from(start));
        Ok(())
    }

    fn escape_char(&mut self) -> Result<char, SyntaxError> {
        let c = self
            .peek()
            .ok_or_else(|| SyntaxError::new("unfinished escape sequence", self.here(1)))?;
        self.advance();
        Ok(match c {
            b'n' => '\n',
            b't' => '\t',
            b'r' => '\r',
            b'\\' => '\\',
            b'\'' => '\'',
            b'"' => '"',
            b'0' => '\0',
            other => {
                return Err(SyntaxError::new(
                    format!("unknown escape sequence `\\{}`", other as char),
                    self.here(2),
                )
                .with_hint("supported escapes: \\n \\t \\r \\\\ \\' \\\" \\0"));
            }
        })
    }

    fn lex_ident(&mut self) {
        let start = self.pos;
        while let Some(c) = self.peek() {
            if c == b'_' || c.is_ascii_alphanumeric() {
                self.advance();
            } else {
                break;
            }
        }
        let text = &self.src[start..self.pos];
        let kind = match text {
            "def" => TokenKind::Def,
            "if" => TokenKind::If,
            "elif" => TokenKind::Elif,
            "else" => TokenKind::Else,
            "while" => TokenKind::While,
            "for" => TokenKind::For,
            "in" => TokenKind::In,
            "return" => TokenKind::Return,
            "break" => TokenKind::Break,
            "continue" => TokenKind::Continue,
            "pass" => TokenKind::Pass,
            "and" => TokenKind::And,
            "or" => TokenKind::Or,
            "not" => TokenKind::Not,
            "type" => TokenKind::Type,
            "match" => TokenKind::Match,
            "case" => TokenKind::Case,
            "agent" => TokenKind::Agent,
            "tool" => TokenKind::Tool,
            "classified" => TokenKind::Classified,
            "declassify" => TokenKind::Declassify,
            "use" => TokenKind::Use,
            "test" => TokenKind::Test,
            "mock" => TokenKind::Mock,
            "assert" => TokenKind::Assert,
            "budget" => TokenKind::Budget,
            "parallel" => TokenKind::Parallel,
            "True" => TokenKind::True,
            "False" => TokenKind::False,
            "None" => TokenKind::None,
            _ => TokenKind::Ident(text.to_string()),
        };
        self.push(kind, self.span_from(start));
    }

    fn lex_operator(&mut self) -> Result<(), SyntaxError> {
        use TokenKind::*;
        let start = self.pos;
        let c = self.bytes[self.pos];
        let two = self.peek_at(1);
        let (kind, len) = match (c, two) {
            (b'*', Some(b'*')) => (DoubleStar, 2),
            (b'/', Some(b'/')) => (DoubleSlash, 2),
            (b'=', Some(b'=')) => (EqEq, 2),
            (b'!', Some(b'=')) => (NotEq, 2),
            (b'<', Some(b'=')) => (LtEq, 2),
            (b'>', Some(b'=')) => (GtEq, 2),
            (b'+', Some(b'=')) => (PlusEq, 2),
            (b'-', Some(b'=')) => (MinusEq, 2),
            (b'*', Some(b'=')) => (StarEq, 2),
            (b'/', Some(b'=')) => (SlashEq, 2),
            (b'-', Some(b'>')) => (Arrow, 2),
            (b'+', _) => (Plus, 1),
            (b'-', _) => (Minus, 1),
            (b'*', _) => (Star, 1),
            (b'/', _) => (Slash, 1),
            (b'%', _) => (Percent, 1),
            (b'=', _) => (Eq, 1),
            (b'<', _) => (Lt, 1),
            (b'>', _) => (Gt, 1),
            (b'(', _) => {
                self.bracket_depth += 1;
                (LParen, 1)
            }
            (b')', _) => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                (RParen, 1)
            }
            (b'[', _) => {
                self.bracket_depth += 1;
                (LBracket, 1)
            }
            (b']', _) => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                (RBracket, 1)
            }
            (b'{', _) => {
                self.bracket_depth += 1;
                (LBrace, 1)
            }
            (b'}', _) => {
                self.bracket_depth = self.bracket_depth.saturating_sub(1);
                (RBrace, 1)
            }
            (b',', _) => (Comma, 1),
            (b':', _) => (Colon, 1),
            (b'.', _) => (Dot, 1),
            _ => {
                return Err(SyntaxError::new(
                    format!("unexpected character `{}`", c as char),
                    self.here(1),
                ));
            }
        };
        for _ in 0..len {
            self.advance();
        }
        self.push(kind, self.span_from(start));
        Ok(())
    }

    // --- low-level helpers ---

    fn skip_comment(&mut self) {
        while let Some(c) = self.peek() {
            if c == b'\n' {
                break;
            }
            self.advance_char();
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes.get(self.pos + offset).copied()
    }

    /// Advance one byte (ASCII contexts only).
    fn advance(&mut self) {
        self.pos += 1;
        self.col += 1;
    }

    /// Advance one full UTF-8 character, returning it.
    fn advance_char(&mut self) -> char {
        let ch = self.src[self.pos..].chars().next().unwrap_or('\u{FFFD}');
        self.pos += ch.len_utf8();
        self.col += 1;
        ch
    }

    fn advance_newline(&mut self) {
        self.pos += 1;
        self.line += 1;
        self.col = 1;
    }

    fn here(&self, len: usize) -> Span {
        Span::new(self.pos, self.pos + len, self.line, self.col)
    }

    fn span_from(&self, start: usize) -> Span {
        let consumed = self.pos - start;
        Span::new(
            start,
            self.pos,
            self.line,
            self.col.saturating_sub(consumed as u32),
        )
    }

    fn push(&mut self, kind: TokenKind, span: Span) {
        self.tokens.push(Token { kind, span });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use TokenKind::*;

    fn kinds(src: &str) -> Vec<TokenKind> {
        Lexer::new(src)
            .tokenize()
            .unwrap()
            .into_iter()
            .map(|t| t.kind)
            .collect()
    }

    #[test]
    fn simple_assignment() {
        assert_eq!(
            kinds("x = 1 + 2\n"),
            vec![Ident("x".into()), Eq, Int(1), Plus, Int(2), Newline, Eof]
        );
    }

    #[test]
    fn indentation_blocks() {
        let toks = kinds("if x:\n    y = 1\nz = 2\n");
        assert!(toks.contains(&Indent));
        assert!(toks.contains(&Dedent));
    }

    #[test]
    fn nested_dedents_unwind() {
        let toks = kinds("if a:\n    if b:\n        x = 1\n");
        let dedents = toks.iter().filter(|k| **k == Dedent).count();
        assert_eq!(dedents, 2);
    }

    #[test]
    fn fstring_parts() {
        let toks = kinds("f\"hi {name}!\"\n");
        match &toks[0] {
            FStr { parts, exprs } => {
                assert_eq!(parts, &vec!["hi ".to_string(), "!".to_string()]);
                assert_eq!(exprs, &vec!["name".to_string()]);
            }
            other => panic!("expected f-string, got {other:?}"),
        }
    }

    #[test]
    fn newline_inside_brackets_ignored() {
        let toks = kinds("x = [1,\n     2]\n");
        let newlines = toks.iter().filter(|k| **k == Newline).count();
        assert_eq!(newlines, 1);
    }

    #[test]
    fn comments_skipped() {
        let toks = kinds("# a comment\nx = 1  # trailing\n");
        assert_eq!(toks[0], Ident("x".into()));
    }

    #[test]
    fn tab_indent_rejected() {
        let err = Lexer::new("if x:\n\ty = 1\n").tokenize().unwrap_err();
        assert!(err.message.contains("tab"));
    }

    #[test]
    fn bad_dedent_rejected() {
        let err = Lexer::new("if x:\n    y = 1\n  z = 2\n")
            .tokenize()
            .unwrap_err();
        assert!(err.message.contains("unindent"));
    }

    #[test]
    fn float_and_underscores() {
        assert_eq!(kinds("1_000\n")[0], Int(1000));
        assert_eq!(kinds("2.75\n")[0], Float(2.75));
    }

    #[test]
    fn unterminated_string() {
        let err = Lexer::new("x = \"oops\n").tokenize().unwrap_err();
        assert!(err.message.contains("unterminated"));
    }
}
