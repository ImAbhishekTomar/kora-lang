//! Friendly, source-anchored syntax errors.
//!
//! Error messages are half the product (DECISIONS.md): every error carries a
//! span, a plain-language message, and optionally a hint suggesting the fix.

use crate::token::Span;
use std::fmt;

#[derive(Debug, Clone)]
pub struct SyntaxError {
    pub message: String,
    pub hint: Option<String>,
    pub span: Span,
}

impl SyntaxError {
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        SyntaxError {
            message: message.into(),
            hint: None,
            span,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    /// Render the error with the offending source line and a caret marker.
    pub fn render(&self, source: &str, filename: &str) -> String {
        let line_no = self.span.line as usize;
        let col = self.span.col as usize;
        let src_line = source.lines().nth(line_no.saturating_sub(1)).unwrap_or("");

        let gutter = format!("{line_no}");
        let pad = " ".repeat(gutter.len());
        let caret_pad = " ".repeat(col.saturating_sub(1));
        let caret_len = (self.span.end.saturating_sub(self.span.start)).max(1);
        let carets = "^".repeat(caret_len.min(src_line.len().saturating_sub(col - 1).max(1)));

        let mut out = format!(
            "error: {msg}\n {pad}--> {filename}:{line_no}:{col}\n {pad} |\n {gutter} | {src_line}\n {pad} | {caret_pad}{carets}\n",
            msg = self.message,
        );
        if let Some(hint) = &self.hint {
            out.push_str(&format!(" {pad} = hint: {hint}\n"));
        }
        out
    }
}

impl fmt::Display for SyntaxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} (line {}, col {})",
            self.message, self.span.line, self.span.col
        )
    }
}

impl std::error::Error for SyntaxError {}
