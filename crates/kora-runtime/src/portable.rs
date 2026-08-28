//! Values that can cross a thread boundary.
//!
//! Agents share nothing (DECISIONS.md): each one owns its heap, and values
//! move between them by copying. Runtime values use `Rc` for cheap sharing
//! *within* an agent, which is exactly why they cannot be sent as-is. This
//! module is the copy boundary — the price of isolation, paid explicitly.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::ast::FuncDef;

use crate::label::Label;
use crate::media::Image;
use crate::modules::ModuleId;
use crate::value::Value;

/// A deep, `Send`-safe copy of a [`Value`].
#[derive(Debug, Clone)]
pub enum Portable {
    Int(i64),
    Float(f64),
    Str(String),
    Bool(bool),
    None,
    List(Vec<Portable>),
    Dict(Vec<(String, Portable)>),
    Object {
        type_name: String,
        fields: Vec<(String, Portable)>,
    },
    Variant {
        tag: String,
        payload: Vec<Portable>,
    },
    /// A function plus the module it was defined in, so a worker resolves its
    /// free names the same way the spawning interpreter would.
    Func {
        def: FuncDef,
        home: ModuleId,
    },
    Builtin(&'static str),
    /// Images cross by copy like everything else. A worker classifying its
    /// own receipt needs the bytes, not a handle into another agent's heap.
    Image(Image),
    Module(String),
    UserModule {
        id: ModuleId,
        alias: String,
    },
    TypeRef(String),
    McpServer(String),
    PyModule(String),
    McpTool(String, String),
    /// Labels cross agent boundaries: isolation must not launder them.
    Labeled {
        label: Label,
        inner: Box<Portable>,
    },
}

impl Portable {
    /// Deep-copy a runtime value out of one agent's heap.
    pub fn from_value(value: &Value) -> Portable {
        match value {
            Value::Int(v) => Portable::Int(*v),
            Value::Float(v) => Portable::Float(*v),
            Value::Str(s) => Portable::Str(s.to_string()),
            Value::Bool(b) => Portable::Bool(*b),
            Value::None => Portable::None,
            Value::List(items) => {
                Portable::List(items.borrow().iter().map(Portable::from_value).collect())
            }
            Value::Dict(map) => Portable::Dict(
                map.borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), Portable::from_value(v)))
                    .collect(),
            ),
            Value::Object { type_name, fields } => Portable::Object {
                type_name: type_name.to_string(),
                fields: fields
                    .borrow()
                    .iter()
                    .map(|(k, v)| (k.clone(), Portable::from_value(v)))
                    .collect(),
            },
            Value::Variant { tag, payload } => Portable::Variant {
                tag: tag.to_string(),
                payload: payload.iter().map(Portable::from_value).collect(),
            },
            Value::Func { def, home } => Portable::Func {
                def: (**def).clone(),
                home: *home,
            },
            Value::Builtin(name) => Portable::Builtin(name),
            Value::Image(image) => Portable::Image((**image).clone()),
            Value::Module { name } => Portable::Module(name.to_string()),
            Value::UserModule { id, alias } => Portable::UserModule {
                id: *id,
                alias: alias.to_string(),
            },
            Value::TypeRef { name } => Portable::TypeRef(name.to_string()),
            Value::McpServer { alias } => Portable::McpServer(alias.to_string()),
            Value::PyModule { module } => Portable::PyModule(module.to_string()),
            Value::McpTool { server, name } => {
                Portable::McpTool(server.to_string(), name.to_string())
            }
            Value::Labeled { label, inner } => Portable::Labeled {
                label: label.clone(),
                inner: Box::new(Portable::from_value(inner)),
            },
        }
    }

    /// Rebuild the value inside the receiving agent's heap.
    pub fn into_value(self) -> Value {
        match self {
            Portable::Int(v) => Value::Int(v),
            Portable::Float(v) => Value::Float(v),
            Portable::Str(s) => Value::Str(Rc::new(s)),
            Portable::Bool(b) => Value::Bool(b),
            Portable::None => Value::None,
            Portable::List(items) => Value::List(Rc::new(RefCell::new(
                items.into_iter().map(Portable::into_value).collect(),
            ))),
            Portable::Dict(pairs) => {
                let map: HashMap<String, Value> = pairs
                    .into_iter()
                    .map(|(k, v)| (k, v.into_value()))
                    .collect();
                Value::Dict(Rc::new(RefCell::new(map)))
            }
            Portable::Object { type_name, fields } => {
                let map: HashMap<String, Value> = fields
                    .into_iter()
                    .map(|(k, v)| (k, v.into_value()))
                    .collect();
                Value::Object {
                    type_name: Rc::new(type_name),
                    fields: Rc::new(RefCell::new(map)),
                }
            }
            Portable::Variant { tag, payload } => Value::Variant {
                tag: Rc::new(tag),
                payload: payload.into_iter().map(Portable::into_value).collect(),
            },
            Portable::Func { def, home } => Value::Func {
                def: Rc::new(def),
                home,
            },
            Portable::Builtin(name) => Value::Builtin(name),
            Portable::Image(image) => Value::Image(Rc::new(image)),
            Portable::UserModule { id, alias } => Value::UserModule {
                id,
                alias: Rc::new(alias),
            },
            Portable::Module(name) => Value::Module {
                name: Rc::new(name),
            },
            Portable::TypeRef(name) => Value::TypeRef {
                name: Rc::new(name),
            },
            Portable::McpServer(alias) => Value::McpServer {
                alias: Rc::new(alias),
            },
            Portable::PyModule(module) => Value::PyModule {
                module: Rc::new(module),
            },
            Portable::McpTool(server, name) => Value::McpTool {
                server: Rc::new(server),
                name: Rc::new(name),
            },
            Portable::Labeled { label, inner } => Value::Labeled {
                label,
                inner: Rc::new(inner.into_value()),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(value: Value) -> Value {
        Portable::from_value(&value).into_value()
    }

    #[test]
    fn scalars_survive() {
        assert!(round_trip(Value::Int(7)).same(&Value::Int(7)));
        assert!(round_trip(Value::Bool(true)).same(&Value::Bool(true)));
        assert!(round_trip(Value::None).same(&Value::None));
        assert!(
            round_trip(Value::Str(Rc::new("hi".into()))).same(&Value::Str(Rc::new("hi".into())))
        );
    }

    #[test]
    fn nested_containers_survive() {
        let inner = Value::List(Rc::new(RefCell::new(vec![Value::Int(1), Value::Int(2)])));
        let outer = Value::List(Rc::new(RefCell::new(vec![inner])));
        assert!(round_trip(outer.clone()).same(&outer));
    }

    #[test]
    fn objects_and_variants_survive() {
        let mut fields = HashMap::new();
        fields.insert("name".to_string(), Value::Str(Rc::new("ada".into())));
        let obj = Value::Object {
            type_name: Rc::new("Person".into()),
            fields: Rc::new(RefCell::new(fields)),
        };
        let variant = Value::Variant {
            tag: Rc::new("Ok".into()),
            payload: vec![obj],
        };
        assert!(round_trip(variant.clone()).same(&variant));
    }

    #[test]
    fn copy_is_deep_not_shared() {
        let original = Value::List(Rc::new(RefCell::new(vec![Value::Int(1)])));
        let copy = round_trip(original.clone());
        // Mutating the original must not touch the copy: that is the whole
        // point of the isolation boundary.
        if let Value::List(items) = &original {
            items.borrow_mut().push(Value::Int(2));
        }
        match copy {
            Value::List(items) => assert_eq!(items.borrow().len(), 1),
            other => panic!("expected list, got {other:?}"),
        }
    }

    /// An image must survive the copy boundary intact: a worker classifying
    /// its own receipt needs the bytes, not a truncated summary.
    #[test]
    fn images_survive_the_copy_boundary() {
        let image = Image::detect(b"\x89PNG\r\n\x1a\n\x00\x01\x02".to_vec(), "a.png").unwrap();
        let value = Value::Image(Rc::new(image.clone()));
        match round_trip(value) {
            Value::Image(copied) => {
                assert_eq!(*copied, image);
                assert_eq!(copied.bytes.len(), 11);
            }
            other => panic!("expected an image, got {other:?}"),
        }
    }

    #[test]
    fn portable_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<Portable>();
    }
}
