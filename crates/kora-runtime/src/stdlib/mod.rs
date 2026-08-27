//! The native standard library.
//!
//! Every module here is backed by a Rust crate, and every one is meant to fix
//! a specific, well-known defect in its equivalent elsewhere (see the table in
//! DECISIONS.md). Three rules hold across all of them:
//!
//! 1. **Data from outside is `unverified`.** File contents, HTTP bodies, and
//!    parsed text carry the label until something narrows them. A dangerous
//!    sink refuses unverified input, so injection is a type error rather than
//!    a review finding.
//! 2. **Nondeterminism is journaled.** Clocks and randomness are effects; a
//!    durable replay must see what the first attempt saw.
//! 3. **Failure is a value.** `Ok(...)` / `Err(reason)`, matched like any
//!    other result. No silent `None`, no forgotten exception.

use std::collections::HashMap;

use kora_syntax::token::Span;

use crate::interp::{Interpreter, RuntimeError};
use crate::value::Value;

pub mod csv;
pub mod env;
pub mod fs;
pub mod http;
pub mod json;
pub mod re;
pub mod sql;
pub mod time;

/// A native function: name, and the implementation.
pub type NativeFn = fn(&mut Interpreter, Vec<Value>, Span) -> Result<Value, RuntimeError>;

/// What a module exports. The `'static` lifetimes are load-bearing: these
/// names are stored in the module table.
pub type Exports = &'static [(&'static str, NativeFn)];

/// One stdlib module.
pub struct Module {
    pub name: &'static str,
    pub functions: HashMap<&'static str, NativeFn>,
}

impl Module {
    fn new(name: &'static str, entries: Exports) -> Module {
        Module {
            name,
            functions: entries.iter().copied().collect(),
        }
    }
}

/// Look up a module by name, or `None` when no such module exists.
pub fn module(name: &str) -> Option<Module> {
    match name {
        "json" => Some(Module::new("json", json::EXPORTS)),
        "csv" => Some(Module::new("csv", csv::EXPORTS)),
        "http" => Some(Module::new("http", http::EXPORTS)),
        "sql" => Some(Module::new("sql", sql::EXPORTS)),
        "env" => Some(Module::new("env", env::EXPORTS)),
        "fs" => Some(Module::new("fs", fs::EXPORTS)),
        "time" => Some(Module::new("time", time::EXPORTS)),
        "re" => Some(Module::new("re", re::EXPORTS)),
        _ => None,
    }
}

/// Module names, for "did you mean" hints.
pub const MODULE_NAMES: &[&str] = &["json", "csv", "http", "sql", "env", "fs", "time", "re"];

// --- shared helpers for module implementations ---

/// `Ok(value)`, the success half of a stdlib result.
pub(crate) fn ok(value: Value) -> Value {
    Value::Variant {
        tag: std::rc::Rc::new("Ok".to_string()),
        payload: vec![value],
    }
}

/// `Err(reason)`. Failure is a value here, never a silent `None`.
pub(crate) fn err(reason: impl Into<String>) -> Value {
    Value::Variant {
        tag: std::rc::Rc::new("Err".to_string()),
        payload: vec![Value::Str(std::rc::Rc::new(reason.into()))],
    }
}

/// Read one string argument, rejecting the wrong shape with a clear message.
pub(crate) fn str_arg(
    args: &[Value],
    index: usize,
    func: &str,
    what: &str,
    span: Span,
) -> Result<String, RuntimeError> {
    match args.get(index).map(|v| v.unlabeled()) {
        Some(Value::Str(s)) => Ok(s.to_string()),
        Some(other) => Err(RuntimeError::new(
            format!(
                "{func}() expects {what} as a string, got {}",
                other.type_name()
            ),
            span,
        )),
        None => Err(RuntimeError::new(format!("{func}() needs {what}"), span)),
    }
}

/// Reject a value that carries a secret anywhere inside it.
///
/// Uses the *deep* label: a `classified` field marker sits on the type, so a
/// shallow check would be bypassed by handing over the whole object.
pub(crate) fn require_not_classified(
    interp: &Interpreter,
    value: &Value,
    func: &str,
    span: Span,
) -> Result<(), RuntimeError> {
    if interp.deep_label(value).is_classified() {
        return Err(
            RuntimeError::new(format!("{func}() was given classified data"), span)
                .with_hint("declassify it for the sink it is going to, or pass redact(...)"),
        );
    }
    Ok(())
}

/// Reject a call that would act on data the program has not validated.
///
/// This is the integrity half of the label system: the point of `unverified`
/// is that it stops here, at the sink, rather than being noticed in review.
pub(crate) fn require_verified(
    value: &Value,
    func: &str,
    what: &str,
    span: Span,
) -> Result<(), RuntimeError> {
    if value.label().is_unverified() {
        return Err(RuntimeError::new(
            format!("{func}() was given {what} that came from outside the program"),
            span,
        )
        .with_hint(
            "narrow it first: parse it into a type, or match it against a set of allowed values",
        ));
    }
    Ok(())
}
