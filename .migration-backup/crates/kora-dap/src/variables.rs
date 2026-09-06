//! Turning Kora values into what a debugger's variables pane shows.
//!
//! The editor asks for children by an integer handle, so a stop flattens the
//! values it is showing into an arena and hands out indices. The arena is
//! rebuilt at every stop, which is also what makes stale handles from a
//! previous stop impossible to use by accident.

use kora_runtime::value::Value;

/// How deep to walk a nested value. Deep enough for real data, shallow enough
/// that a value holding itself cannot hang the adapter.
const MAX_DEPTH: usize = 12;

/// How many children of one container to show. A long list is truncated with
/// a marker rather than flooding the pane.
const MAX_CHILDREN: usize = 500;

/// One row in the variables pane.
pub struct Node {
    pub name: String,
    pub value: String,
    pub type_name: String,
    pub children: Vec<usize>,
}

/// Every row of one stop, addressed by index.
#[derive(Default)]
pub struct Arena {
    nodes: Vec<Node>,
}

impl Arena {
    /// Claim a handle for a scope before its contents exist.
    ///
    /// Scopes are reserved first so their handles are the low, stable numbers:
    /// the innermost frame's Locals is always 1. Filling them first would
    /// interleave scopes with the values inside them and make every handle
    /// depend on how deep the data happened to be.
    pub fn reserve_scope(&mut self, name: &str) -> usize {
        self.push(Node {
            name: name.to_string(),
            value: String::new(),
            type_name: "scope".to_string(),
            children: Vec::new(),
        })
    }

    /// Put `entries` inside a reserved scope.
    pub fn fill_scope(&mut self, handle: usize, entries: &[(String, Value)]) {
        let children = entries
            .iter()
            .map(|(name, value)| self.add(name, value, 0))
            .collect();
        if let Some(index) = handle.checked_sub(1) {
            if let Some(node) = self.nodes.get_mut(index) {
                node.children = children;
            }
        }
    }

    /// The rows under `handle`, or nothing if the handle is unknown.
    pub fn children_of(&self, handle: usize) -> &[usize] {
        match self.index(handle) {
            Some(node) => &node.children,
            None => &[],
        }
    }

    pub fn get(&self, handle: usize) -> Option<&Node> {
        self.index(handle)
    }

    /// A handle the client can ask about, or 0 for a leaf. DAP reserves 0 for
    /// "no children", so handles are one-based.
    pub fn reference(&self, handle: usize) -> i64 {
        match self.index(handle) {
            Some(node) if !node.children.is_empty() => handle as i64,
            _ => 0,
        }
    }

    fn index(&self, handle: usize) -> Option<&Node> {
        handle.checked_sub(1).and_then(|i| self.nodes.get(i))
    }

    fn push(&mut self, node: Node) -> usize {
        self.nodes.push(node);
        self.nodes.len()
    }

    fn add(&mut self, name: &str, value: &Value, depth: usize) -> usize {
        // A label is worth seeing in a debugger: it is the difference between
        // a value that may reach a model and one that may not.
        //
        // The two directions are named separately. `classified` is
        // confidentiality — this must not leave; `unverified` is integrity —
        // this came from outside and must not reach a sink. Showing an
        // `fs.read` result as "classified" would name the wrong half, and a
        // value can carry both.
        let label = value.label();
        let inner = value.unlabeled();
        let mut type_name = inner.type_name();
        if label.is_unverified() {
            type_name = format!("unverified {type_name}");
        }
        if label.is_classified() {
            type_name = format!("classified {type_name}");
        }

        let children = if depth >= MAX_DEPTH {
            Vec::new()
        } else {
            self.children(inner, depth + 1)
        };

        self.push(Node {
            name: name.to_string(),
            value: summary(inner),
            type_name,
            children,
        })
    }

    fn children(&mut self, value: &Value, depth: usize) -> Vec<usize> {
        match value {
            Value::List(items) => {
                let items = items.borrow();
                items
                    .iter()
                    .take(MAX_CHILDREN)
                    .enumerate()
                    .map(|(i, v)| self.add(&i.to_string(), v, depth))
                    .collect()
            }
            Value::Dict(map) => {
                let map = map.borrow();
                let mut keys: Vec<&String> = map.keys().take(MAX_CHILDREN).collect();
                keys.sort();
                keys.into_iter()
                    .map(|k| {
                        let v = map[k].clone();
                        self.add(k, &v, depth)
                    })
                    .collect()
            }
            Value::Object { fields, .. } => {
                let fields = fields.borrow();
                let mut keys: Vec<&String> = fields.keys().collect();
                keys.sort();
                keys.into_iter()
                    .map(|k| {
                        let v = fields[k].clone();
                        self.add(k, &v, depth)
                    })
                    .collect()
            }
            Value::Variant { payload, .. } => payload
                .iter()
                .enumerate()
                .map(|(i, v)| self.add(&i.to_string(), v, depth))
                .collect(),
            // An image expands to what a person can act on -- where it came
            // from, what it is, how big it is. Never the bytes.
            Value::Image(image) => {
                let source = Value::Str(std::rc::Rc::new(image.source.clone()));
                let mime = Value::Str(std::rc::Rc::new(image.mime.clone()));
                let size = Value::Int(image.bytes.len() as i64);
                vec![
                    self.add("source", &source, depth),
                    self.add("mime", &mime, depth),
                    self.add("bytes", &size, depth),
                ]
            }
            _ => Vec::new(),
        }
    }
}

/// The one-line form shown next to a name.
///
/// Containers show their size rather than their contents: the contents are
/// one click away, and a thousand-element list should not be one line.
fn summary(value: &Value) -> String {
    match value {
        Value::Str(s) => format!("{s:?}"),
        Value::List(items) => format!("list[{}]", items.borrow().len()),
        Value::Dict(map) => format!("dict[{}]", map.borrow().len()),
        Value::Object { type_name, fields } => {
            // A package qualifier is an identity mechanism, not something to
            // show in a variables pane.
            format!(
                "{}({} fields)",
                kora_runtime::value::short_type_name(type_name),
                fields.borrow().len()
            )
        }
        other => other.to_string(),
    }
}
