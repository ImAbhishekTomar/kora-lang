//! `re` — pattern matching that cannot be turned into a denial of service.
//!
//! The defect fixed: backtracking engines (PCRE, Python's `re`, JavaScript's)
//! can take exponential time on inputs an attacker chooses. `(a+)+$` against a
//! long string of `a`s will hang a server. Every few years this takes down a
//! large site, and nobody can fix it because the ecosystems depend on
//! backtracking features.
//!
//! Kora uses a finite-automaton engine with a linear-time guarantee. The cost
//! is no backreferences and no lookaround; the benefit is that a pattern
//! applied to hostile input cannot become a hang. For an agent language, where
//! patterns may come from a model and text may come from a web page, that is
//! the right trade.

use std::cell::RefCell;
use std::rc::Rc;

use kora_syntax::token::Span;

use super::{err, ok, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::value::Value;

pub const EXPORTS: super::Exports = &[
    ("matches", matches_fn),
    ("find", find),
    ("find_all", find_all),
    ("replace", replace),
    ("split", split),
];

/// `re.matches(pattern, text) -> Ok(bool) | Err(reason)`
fn matches_fn(
    _interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let (regex, text) = match compile(&args, "re.matches", span)? {
        Compiled::Ready(r, t) => (r, t),
        Compiled::Failed(message) => return Ok(err(message)),
    };
    Ok(ok(Value::Bool(regex.is_match(&text))))
}

/// `re.find(pattern, text) -> Ok(text) | Err(reason)`
///
/// Returns the first match, or `Err` when there is none — a miss is a value,
/// not a `None` that flows onward and breaks something later.
fn find(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (regex, text) = match compile(&args, "re.find", span)? {
        Compiled::Ready(r, t) => (r, t),
        Compiled::Failed(message) => return Ok(err(message)),
    };
    let label = args.get(1).map(|v| v.label()).unwrap_or_default();
    match regex.find(&text) {
        Some(m) => Ok(ok(
            Value::Str(Rc::new(m.as_str().to_string())).with_label(label)
        )),
        None => Ok(err("no match".to_string())),
    }
}

/// `re.find_all(pattern, text) -> Ok(list) | Err(reason)`
fn find_all(
    _interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let (regex, text) = match compile(&args, "re.find_all", span)? {
        Compiled::Ready(r, t) => (r, t),
        Compiled::Failed(message) => return Ok(err(message)),
    };
    let label = args.get(1).map(|v| v.label()).unwrap_or_default();
    let items: Vec<Value> = regex
        .find_iter(&text)
        .map(|m| Value::Str(Rc::new(m.as_str().to_string())))
        .collect();
    Ok(ok(
        Value::List(Rc::new(RefCell::new(items))).with_label(label)
    ))
}

/// `re.replace(pattern, text, replacement) -> Ok(text) | Err(reason)`
fn replace(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (regex, text) = match compile(&args, "re.replace", span)? {
        Compiled::Ready(r, t) => (r, t),
        Compiled::Failed(message) => return Ok(err(message)),
    };
    let replacement = str_arg(&args, 2, "re.replace", "a replacement", span)?;
    let label = args.get(1).map(|v| v.label()).unwrap_or_default();
    let out = regex.replace_all(&text, replacement.as_str()).into_owned();
    Ok(ok(Value::Str(Rc::new(out)).with_label(label)))
}

/// `re.split(pattern, text) -> Ok(list) | Err(reason)`
fn split(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (regex, text) = match compile(&args, "re.split", span)? {
        Compiled::Ready(r, t) => (r, t),
        Compiled::Failed(message) => return Ok(err(message)),
    };
    let label = args.get(1).map(|v| v.label()).unwrap_or_default();
    let items: Vec<Value> = regex
        .split(&text)
        .map(|part| Value::Str(Rc::new(part.to_string())))
        .collect();
    Ok(ok(
        Value::List(Rc::new(RefCell::new(items))).with_label(label)
    ))
}

enum Compiled {
    Ready(regex::Regex, String),
    Failed(String),
}

/// Compile the pattern, turning a bad pattern into a value rather than a
/// crash — patterns often come from configuration or from a model.
fn compile(args: &[Value], func: &str, span: Span) -> Result<Compiled, RuntimeError> {
    let pattern = str_arg(args, 0, func, "a pattern", span)?;
    let text = str_arg(args, 1, func, "the text to search", span)?;
    match regex::Regex::new(&pattern) {
        Ok(regex) => Ok(Compiled::Ready(regex, text)),
        Err(e) => Ok(Compiled::Failed(format!(
            "invalid pattern `{pattern}`: {e}"
        ))),
    }
}
