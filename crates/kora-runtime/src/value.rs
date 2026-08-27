//! Runtime values.
//!
//! Phase 1 note: values use Rc for cheap sharing *within* one agent. The
//! per-agent-heap isolation from DECISIONS.md arrives in Phase 3 — agents
//! will each own an interpreter instance and exchange deep copies.

use std::cell::RefCell;
use std::collections::HashMap;
use std::fmt;
use std::rc::Rc;

use kora_syntax::ast::FuncDef;

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    Float(f64),
    Str(Rc<String>),
    Bool(bool),
    None,
    List(Rc<RefCell<Vec<Value>>>),
    Dict(Rc<RefCell<HashMap<String, Value>>>),
    /// User-defined function.
    Func(Rc<FuncDef>),
    /// Instance of a user-declared `type` block.
    Object {
        type_name: Rc<String>,
        fields: Rc<RefCell<HashMap<String, Value>>>,
    },
    /// Built-in function (print, len, range, ...).
    Builtin(&'static str),
    /// Tagged variant: Ok(value), Uncertain(reason), Exhausted(meter), ...
    Variant {
        tag: Rc<String>,
        payload: Vec<Value>,
    },
}

impl Value {
    pub fn type_name(&self) -> String {
        match self {
            Value::Int(_) => "int".into(),
            Value::Float(_) => "float".into(),
            Value::Str(_) => "str".into(),
            Value::Bool(_) => "bool".into(),
            Value::None => "None".into(),
            Value::List(_) => "list".into(),
            Value::Dict(_) => "dict".into(),
            Value::Func(f) => format!("function {}", f.name),
            Value::Object { type_name, .. } => type_name.as_str().into(),
            Value::Builtin(name) => format!("builtin {name}"),
            Value::Variant { tag, .. } => tag.as_str().into(),
        }
    }

    pub fn truthy(&self) -> bool {
        match self {
            Value::Bool(b) => *b,
            Value::Int(v) => *v != 0,
            Value::Float(v) => *v != 0.0,
            Value::Str(s) => !s.is_empty(),
            Value::None => false,
            Value::List(l) => !l.borrow().is_empty(),
            Value::Dict(d) => !d.borrow().is_empty(),
            Value::Func(_) | Value::Object { .. } | Value::Builtin(_) => true,
            Value::Variant { .. } => true,
        }
    }

    /// Structural equality, Python-style (1 == 1.0 is true).
    pub fn same(&self, other: &Value) -> bool {
        use Value::*;
        match (self, other) {
            (Int(a), Int(b)) => a == b,
            (Float(a), Float(b)) => a == b,
            (Int(a), Float(b)) | (Float(b), Int(a)) => (*a as f64) == *b,
            (Str(a), Str(b)) => a == b,
            (Bool(a), Bool(b)) => a == b,
            (None, None) => true,
            (List(a), List(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| x.same(y))
            }
            (Dict(a), Dict(b)) => {
                let (a, b) = (a.borrow(), b.borrow());
                a.len() == b.len() && a.iter().all(|(k, v)| b.get(k).is_some_and(|w| v.same(w)))
            }
            (
                Object {
                    type_name: t1,
                    fields: f1,
                },
                Object {
                    type_name: t2,
                    fields: f2,
                },
            ) => {
                if t1 != t2 {
                    return false;
                }
                let (f1, f2) = (f1.borrow(), f2.borrow());
                f1.len() == f2.len() && f1.iter().all(|(k, v)| f2.get(k).is_some_and(|w| v.same(w)))
            }
            (
                Variant {
                    tag: t1,
                    payload: p1,
                },
                Variant {
                    tag: t2,
                    payload: p2,
                },
            ) => {
                t1 == t2 && p1.len() == p2.len() && p1.iter().zip(p2.iter()).all(|(x, y)| x.same(y))
            }
            _ => false,
        }
    }

    /// Display formatting, Python-flavored (`True`, `None`, quoted strings in
    /// containers but bare at top level).
    pub fn repr(&self) -> String {
        match self {
            Value::Str(s) => format!("\"{}\"", s),
            other => other.to_string(),
        }
    }
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => {
                if v.fract() == 0.0 && v.is_finite() {
                    write!(f, "{v:.1}")
                } else {
                    write!(f, "{v}")
                }
            }
            Value::Str(s) => write!(f, "{s}"),
            Value::Bool(true) => write!(f, "True"),
            Value::Bool(false) => write!(f, "False"),
            Value::None => write!(f, "None"),
            Value::List(items) => {
                let inner: Vec<String> = items.borrow().iter().map(|v| v.repr()).collect();
                write!(f, "[{}]", inner.join(", "))
            }
            Value::Dict(map) => {
                let inner: Vec<String> = map
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("\"{k}\": {}", v.repr()))
                    .collect();
                write!(f, "{{{}}}", inner.join(", "))
            }
            Value::Func(fd) => write!(f, "<function {}>", fd.name),
            Value::Object { type_name, fields } => {
                let inner: Vec<String> = fields
                    .borrow()
                    .iter()
                    .map(|(k, v)| format!("{k}={}", v.repr()))
                    .collect();
                write!(f, "{type_name}({})", inner.join(", "))
            }
            Value::Builtin(name) => write!(f, "<builtin {name}>"),
            Value::Variant { tag, payload } => {
                if payload.is_empty() {
                    write!(f, "{tag}")
                } else {
                    let inner: Vec<String> = payload.iter().map(|v| v.repr()).collect();
                    write!(f, "{tag}({})", inner.join(", "))
                }
            }
        }
    }
}
