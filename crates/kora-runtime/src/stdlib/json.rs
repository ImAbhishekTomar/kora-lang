//! `json` — parsing that tells you *where* it went wrong.
//!
//! What everyone else gets wrong: `json.loads` hands back an untyped blob, so
//! the mistake surfaces later as an attribute error three functions away. And
//! when parsing itself fails, the message is a byte offset — "line 1 column
//! 4318" — which is useless on one-line JSON.
//!
//! Here, parse failures name the path (`$.users[2].email`), and a parsed value
//! is `unverified` until it has been narrowed into a declared type.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::token::Span;
use serde_json::Value as J;

use super::{err, ok, require_not_classified, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("parse", parse), ("stringify", stringify), ("get", get)];

/// `json.parse(text) -> Ok(value) | Err(reason)`
///
/// The result is `unverified`: it came from outside the program, so it cannot
/// reach a dangerous sink until something narrows it.
fn parse(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "json.parse", "the text to parse", span)?;
    match serde_json::from_str::<J>(&text) {
        Ok(value) => Ok(ok(json_to_value(&value).with_label(Label::UNVERIFIED))),
        Err(e) => Ok(err(describe_parse_error(&text, &e))),
    }
}

/// `json.stringify(value) -> Ok(text) | Err(reason)`
fn stringify(
    interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let Some(value) = args.first() else {
        return Err(RuntimeError::new("json.stringify() needs a value", span));
    };
    // Refuse to serialize secrets by accident: writing JSON is usually a
    // prelude to sending it somewhere.
    require_not_classified(interp, value, "json.stringify", span)?;
    match value_to_json(value) {
        Some(json) => match serde_json::to_string(&json) {
            Ok(text) => Ok(ok(Value::Str(Rc::new(text)))),
            Err(e) => Ok(err(format!("could not encode: {e}"))),
        },
        None => Ok(err(format!(
            "{} cannot be represented as JSON",
            value.type_name()
        ))),
    }
}

/// `json.get(value, "users.0.email") -> Ok(value) | Err(reason)`
///
/// A path walk that says exactly where it stopped, instead of raising a
/// `KeyError` naming only the last segment.
fn get(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let Some(root) = args.first() else {
        return Err(RuntimeError::new("json.get() needs a value", span));
    };
    let path = str_arg(&args, 1, "json.get", "a path", span)?;
    let label = root.label();

    let mut current = root.unlabeled().clone();
    let mut walked = String::from("$");
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        walked.push('.');
        walked.push_str(segment);
        let next = match (&current, segment.parse::<usize>()) {
            (Value::List(items), Ok(index)) => items.borrow().get(index).cloned(),
            (Value::List(_), Err(_)) => {
                return Ok(err(format!("{walked}: expected a number to index a list")))
            }
            (Value::Dict(map), _) => map.borrow().get(segment).cloned(),
            (Value::Object { fields, .. }, _) => fields.borrow().get(segment).cloned(),
            (other, _) => {
                return Ok(err(format!(
                    "{walked}: cannot look inside {}",
                    other.type_name()
                )))
            }
        };
        match next {
            Some(v) => current = v,
            None => return Ok(err(format!("{walked}: not found"))),
        }
    }
    Ok(ok(current.with_label(label)))
}

/// Turn a serde error into a message that points at the problem.
fn describe_parse_error(text: &str, e: &serde_json::Error) -> String {
    let line = e.line();
    let column = e.column();
    // On single-line JSON a line/column pair says nothing useful, so show the
    // surrounding text instead.
    let excerpt = excerpt_at(text, line, column);
    if excerpt.is_empty() {
        format!("invalid JSON at line {line}, column {column}: {e}")
    } else {
        format!("invalid JSON near `{excerpt}` (line {line}, column {column}): {e}")
    }
}

fn excerpt_at(text: &str, line: usize, column: usize) -> String {
    let Some(src_line) = text.lines().nth(line.saturating_sub(1)) else {
        return String::new();
    };
    let start = column.saturating_sub(12);
    let chars: Vec<char> = src_line.chars().collect();
    let end = (column + 12).min(chars.len());
    if start >= end {
        return String::new();
    }
    chars[start..end]
        .iter()
        .collect::<String>()
        .trim()
        .to_string()
}

pub(crate) fn json_to_value(json: &J) -> Value {
    match json {
        J::Null => Value::None,
        J::Bool(b) => Value::Bool(*b),
        J::Number(n) => {
            if let Some(i) = n.as_i64() {
                Value::Int(i)
            } else {
                Value::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        J::String(s) => Value::Str(Rc::new(s.clone())),
        J::Array(items) => Value::List(Rc::new(RefCell::new(
            items.iter().map(json_to_value).collect(),
        ))),
        J::Object(map) => {
            let entries: HashMap<String, Value> = map
                .iter()
                .map(|(k, v)| (k.clone(), json_to_value(v)))
                .collect();
            Value::Dict(Rc::new(RefCell::new(entries)))
        }
    }
}

/// `None` when the value has no JSON representation, so the caller can say so
/// rather than emitting `"<function foo>"` and calling it success.
pub(crate) fn value_to_json(value: &Value) -> Option<J> {
    Some(match value.unlabeled() {
        Value::Int(v) => J::from(*v),
        Value::Float(v) => serde_json::Number::from_f64(*v).map(J::Number)?,
        Value::Str(s) => J::String(s.to_string()),
        Value::Bool(b) => J::Bool(*b),
        Value::None => J::Null,
        Value::List(items) => J::Array(
            items
                .borrow()
                .iter()
                .map(value_to_json)
                .collect::<Option<Vec<_>>>()?,
        ),
        Value::Dict(map) => {
            let mut out = serde_json::Map::new();
            for (k, v) in map.borrow().iter() {
                out.insert(k.clone(), value_to_json(v)?);
            }
            J::Object(out)
        }
        Value::Object { fields, .. } => {
            let mut out = serde_json::Map::new();
            for (k, v) in fields.borrow().iter() {
                out.insert(k.clone(), value_to_json(v)?);
            }
            J::Object(out)
        }
        Value::Func(_) | Value::Builtin(_) | Value::Module { .. } | Value::TypeRef { .. } => {
            return None
        }
        Value::Variant { tag, payload } => {
            if payload.is_empty() {
                J::String(tag.to_string())
            } else {
                let mut out = serde_json::Map::new();
                out.insert(
                    tag.to_string(),
                    J::Array(
                        payload
                            .iter()
                            .map(value_to_json)
                            .collect::<Option<Vec<_>>>()?,
                    ),
                );
                J::Object(out)
            }
        }
        Value::Labeled { .. } => unreachable!("unlabeled() strips this"),
    })
}
