//! Token definitions for the Kora lexer.

use std::fmt;

/// A half-open byte range into the source, plus line/col for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub line: u32,
    pub col: u32,
}

impl Span {
    pub fn new(start: usize, end: usize, line: u32, col: u32) -> Self {
        Span {
            start,
            end,
            line,
            col,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum TokenKind {
    // Literals
    Int(i64),
    Float(f64),
    /// Plain string literal (escapes already resolved).
    Str(String),
    /// f-string: alternating literal parts and expression source fragments,
    /// joined as `parts[0] + exprs[0] + parts[1] + ... + parts[n]`.
    FStr {
        parts: Vec<String>,
        exprs: Vec<String>,
    },
    True,
    False,
    None,

    // Identifiers & keywords
    Ident(String),
    Def,
    If,
    Elif,
    Else,
    While,
    For,
    In,
    Return,
    Break,
    Continue,
    Pass,
    And,
    Or,
    Not,
    Type,
    Match,
    Case,
    Agent,
    Tool,
    Classified,
    Declassify,
    Use,
    Test,
    Mock,
    Assert,
    Budget,
    Parallel,

    // Operators
    Plus,
    Minus,
    Star,
    Slash,
    Percent,
    DoubleStar,
    DoubleSlash,
    Eq,    // =
    EqEq,  // ==
    NotEq, // !=
    Lt,
    Gt,
    LtEq,
    GtEq,
    PlusEq,
    MinusEq,
    StarEq,
    SlashEq,
    Arrow, // ->

    // Delimiters
    LParen,
    RParen,
    LBracket,
    RBracket,
    LBrace,
    RBrace,
    Comma,
    Colon,
    Dot,
    At,

    // Layout
    Newline,
    Indent,
    Dedent,
    Eof,
}

impl fmt::Display for TokenKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use TokenKind::*;
        match self {
            Int(v) => write!(f, "{v}"),
            Float(v) => write!(f, "{v}"),
            Str(_) => write!(f, "string"),
            FStr { .. } => write!(f, "f-string"),
            True => write!(f, "True"),
            False => write!(f, "False"),
            None => write!(f, "None"),
            Ident(s) => write!(f, "{s}"),
            Def => write!(f, "def"),
            If => write!(f, "if"),
            Elif => write!(f, "elif"),
            Else => write!(f, "else"),
            While => write!(f, "while"),
            For => write!(f, "for"),
            In => write!(f, "in"),
            Return => write!(f, "return"),
            Break => write!(f, "break"),
            Continue => write!(f, "continue"),
            Pass => write!(f, "pass"),
            And => write!(f, "and"),
            Or => write!(f, "or"),
            Not => write!(f, "not"),
            Type => write!(f, "type"),
            Match => write!(f, "match"),
            Case => write!(f, "case"),
            Agent => write!(f, "agent"),
            Tool => write!(f, "tool"),
            Classified => write!(f, "classified"),
            Declassify => write!(f, "declassify"),
            Use => write!(f, "use"),
            Test => write!(f, "test"),
            Mock => write!(f, "mock"),
            Assert => write!(f, "assert"),
            Budget => write!(f, "budget"),
            Parallel => write!(f, "parallel"),
            Plus => write!(f, "+"),
            Minus => write!(f, "-"),
            Star => write!(f, "*"),
            Slash => write!(f, "/"),
            Percent => write!(f, "%"),
            DoubleStar => write!(f, "**"),
            DoubleSlash => write!(f, "//"),
            Eq => write!(f, "="),
            EqEq => write!(f, "=="),
            NotEq => write!(f, "!="),
            Lt => write!(f, "<"),
            Gt => write!(f, ">"),
            LtEq => write!(f, "<="),
            GtEq => write!(f, ">="),
            PlusEq => write!(f, "+="),
            MinusEq => write!(f, "-="),
            StarEq => write!(f, "*="),
            SlashEq => write!(f, "/="),
            Arrow => write!(f, "->"),
            LParen => write!(f, "("),
            RParen => write!(f, ")"),
            LBracket => write!(f, "["),
            RBracket => write!(f, "]"),
            LBrace => write!(f, "{{"),
            RBrace => write!(f, "}}"),
            Comma => write!(f, ","),
            Colon => write!(f, ":"),
            Dot => write!(f, "."),
            At => write!(f, "@"),
            Newline => write!(f, "newline"),
            Indent => write!(f, "indent"),
            Dedent => write!(f, "dedent"),
            Eof => write!(f, "end of file"),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Token {
    pub kind: TokenKind,
    pub span: Span,
}
