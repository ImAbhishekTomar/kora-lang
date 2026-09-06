//! `env` — secrets that cannot be printed by accident.
//!
//! The defect fixed: `os.environ["API_KEY"]` returns an ordinary string, so it
//! flows into a log line, an error message, or a crash report without anyone
//! noticing. Leaked credentials in logs are a routine incident, and the
//! ecosystem cannot fix it because environment variables are plain strings
//! everywhere.
//!
//! Here a variable whose name looks like a credential comes back `classified`.
//! It works normally for its real purpose, but reaching a model, a file, or a
//! serializer requires an explicit release.

use std::rc::Rc;

use kora_syntax::token::Span;

use super::{err, ok, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("get", get), ("has", has)];

/// Name fragments that mean "this is a credential".
const SECRET_HINTS: &[&str] = &[
    "key",
    "token",
    "secret",
    "password",
    "passwd",
    "credential",
    "auth",
    "session",
    "cookie",
    "private",
];

/// `env.get(name) -> Ok(value) | Err(reason)`
///
/// Missing is a value, not an empty string that breaks something later.
fn get(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let name = str_arg(&args, 0, "env.get", "a variable name", span)?;
    match std::env::var(&name) {
        Ok(value) => {
            let label = if looks_secret(&name) {
                Label::CLASSIFIED
            } else {
                Label::PUBLIC
            };
            Ok(ok(Value::Str(Rc::new(value)).with_label(label)))
        }
        Err(_) => Ok(err(format!("`{name}` is not set"))),
    }
}

/// `env.has(name) -> bool`
fn has(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let name = str_arg(&args, 0, "env.has", "a variable name", span)?;
    Ok(Value::Bool(std::env::var(&name).is_ok()))
}

/// Whether a variable name suggests it holds a credential.
pub(crate) fn looks_secret(name: &str) -> bool {
    let lowered = name.to_ascii_lowercase();
    SECRET_HINTS.iter().any(|hint| lowered.contains(hint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn credential_names_are_recognised() {
        assert!(looks_secret("OPENAI_API_KEY"));
        assert!(looks_secret("DATABASE_PASSWORD"));
        assert!(looks_secret("github_token"));
        assert!(looks_secret("AWS_SECRET_ACCESS_KEY"));
        assert!(looks_secret("SESSION_COOKIE"));
    }

    #[test]
    fn ordinary_names_are_not_treated_as_secrets() {
        assert!(!looks_secret("HOME"));
        assert!(!looks_secret("LANG"));
        assert!(!looks_secret("KORA_ENV"));
    }
}
