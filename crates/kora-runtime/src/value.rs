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

use crate::label::Label;
use crate::modules::ModuleId;

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
    ///
    /// `home` is the module the function was defined in. A function reads its
    /// own file's top-level names, not the caller's, so importing a module
    /// cannot change what the code inside it means.
    Func {
        def: Rc<FuncDef>,
        home: ModuleId,
    },
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
    /// A stdlib module brought in with `use`.
    Module {
        name: Rc<String>,
    },
    /// Another Kora file, brought in with `use "./lib.ko" as lib`.
    UserModule {
        /// Index into the interpreter's module table.
        id: ModuleId,
        /// The name it was bound to, for error messages.
        alias: Rc<String>,
    },
    /// A reference to a declared `type`, so it can be handed to a parser:
    /// `csv.parse(text, Expense)`.
    TypeRef {
        name: Rc<String>,
    },
    /// A Python module reached through the sidecar worker.
    PyModule {
        module: Rc<String>,
    },
    /// A connected MCP server, from `use mcp github as gh`.
    McpServer {
        alias: Rc<String>,
    },
    /// One tool exposed by an MCP server, ready to hand to `analyze`.
    McpTool {
        server: Rc<String>,
        name: Rc<String>,
    },
    /// A value carrying a confidentiality label.
    ///
    /// Wrapping rather than tagging every variant keeps the label out of the
    /// way of ordinary code: unlabelled values cost nothing, and the wrapper
    /// only appears where sensitive data actually flows.
    Labeled {
        label: Label,
        inner: Rc<Value>,
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
            Value::Func { def, .. } => format!("function {}", def.name),
            Value::Object { type_name, .. } => type_name.as_str().into(),
            Value::Builtin(name) => format!("builtin {name}"),
            Value::Variant { tag, .. } => tag.as_str().into(),
            Value::Module { name } => format!("module {name}"),
            Value::UserModule { alias, .. } => format!("module {alias}"),
            Value::TypeRef { name } => format!("type {name}"),
            Value::PyModule { module } => format!("python module {module}"),
            Value::McpServer { alias } => format!("mcp server {alias}"),
            Value::McpTool { server, name } => format!("tool {server}.{name}"),
            Value::Labeled { inner, .. } => inner.type_name(),
        }
    }

    /// The label this value carries.
    pub fn label(&self) -> Label {
        match self {
            Value::Labeled { label, .. } => label.clone(),
            _ => Label::PUBLIC,
        }
    }

    /// The value with any label stripped. Used at points that have already
    /// checked the label, never to launder one.
    pub fn unlabeled(&self) -> &Value {
        match self {
            Value::Labeled { inner, .. } => inner.unlabeled(),
            other => other,
        }
    }

    /// Attach a label, collapsing nested wrappers.
    pub fn with_label(self, label: Label) -> Value {
        if label.is_plain() {
            return self;
        }
        match self {
            Value::Labeled {
                label: existing,
                inner,
            } => Value::Labeled {
                label: existing.join(label),
                inner,
            },
            other => Value::Labeled {
                label,
                inner: Rc::new(other),
            },
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
            Value::Func { .. } | Value::Object { .. } | Value::Builtin(_) => true,
            Value::Variant { .. }
            | Value::Module { .. }
            | Value::UserModule { .. }
            | Value::TypeRef { .. }
            | Value::McpServer { .. }
            | Value::McpTool { .. }
            | Value::PyModule { .. } => true,
            Value::Labeled { inner, .. } => inner.truthy(),
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
            // Comparison sees through labels; the *result* of a comparison
            // inherits the label at the operator, handled by the interpreter.
            (Labeled { inner, .. }, other) => inner.same(other),
            (other, Labeled { inner, .. }) => other.same(inner),
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
            Value::Func { def, .. } => write!(f, "<function {}>", def.name),
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
            Value::Module { name } => write!(f, "<module {name}>"),
            Value::UserModule { alias, .. } => write!(f, "<module {alias}>"),
            Value::TypeRef { name } => write!(f, "<type {name}>"),
            Value::PyModule { module } => write!(f, "<python module {module}>"),
            Value::McpServer { alias } => write!(f, "<mcp server {alias}>"),
            Value::McpTool { server, name } => write!(f, "<tool {server}.{name}>"),
            // Printing is a local action, not an export, so the value shows
            // normally. Telemetry export is a labeled sink and redacts.
            Value::Labeled { inner, .. } => write!(f, "{inner}"),
        }
    }
}
