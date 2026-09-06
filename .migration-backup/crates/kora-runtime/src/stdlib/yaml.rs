//! `yaml` — the config format, without the three ways it lies to you.
//!
//! YAML is where deployment configuration, CI pipelines, and manifests live,
//! so a program that reads it is usually one step away from doing something
//! consequential. Every mainstream YAML library has three defects that cannot
//! be fixed there, because fixing them would break files already in the world:
//!
//! 1. **A duplicate key wins silently.** PyYAML, js-yaml, and Go's `yaml.v3`
//!    all keep the last one. Appending `admin: true` to the end of a file
//!    someone else wrote is therefore an edit that no diff of the parsed
//!    result would show. Here it is `Err`, naming the key and both lines.
//! 2. **The Norway problem.** YAML 1.1 reads `NO` as `false`, so a country
//!    list loses Norway and a `region: NO` becomes a boolean. Kora parses the
//!    YAML 1.2 core schema, where only `true`/`false` are booleans — `no`,
//!    `yes`, `on`, and `off` stay the strings they were written as.
//! 3. **An anchor can expand to more data than the file contains.** The
//!    "billion laughs" bomb is nine lines of YAML that expands to gigabytes;
//!    every library that resolves aliases eagerly will try. Expansion is
//!    metered here against a node budget, so a hostile file is an `Err` rather
//!    than an out-of-memory kill.
//!
//! Past that, `yaml` behaves exactly like [`json`](super::json): with a
//! declared type the shape is checked and a mismatch names its path
//! (`$.services.web.port`), without one the result is an untyped document, and
//! either way it is `unverified` because it came from outside the program.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use kora_syntax::token::Span;
use serde_json::{Map, Value as J};
use yaml_rust2::parser::{Event, MarkedEventReceiver, Parser, Tag};
use yaml_rust2::scanner::{Marker, TScalarStyle};
use yaml_rust2::{Yaml, YamlEmitter};

use super::json::{coerce_to_type, get as json_get, json_to_value, value_to_json};
use super::{err, ok, require_not_classified, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[
    ("parse", parse),
    ("documents", documents),
    ("stringify", stringify),
    ("get", json_get),
];

/// How many nodes one `yaml.parse` may build, counting nodes an alias
/// expands into.
///
/// A hand-written config is a few hundred nodes; a generated Kubernetes
/// bundle is a few thousand. A quarter of a million is far above anything
/// legitimate and far below what a bomb needs to exhaust memory, which is the
/// only window a limit like this has to sit in. It is deliberately not
/// configurable: a program that raises the ceiling to admit one hostile file
/// has not learned anything the failure was trying to tell it.
const MAX_NODES: usize = 250_000;

/// How deep nesting may go. The value is built with a stack rather than
/// recursion, so this bounds the *conversion* into Kora values, which is
/// recursive, before it can overflow the host stack.
const MAX_DEPTH: usize = 128;

/// `yaml.parse(text) -> Ok(value) | Err(reason)`
/// `yaml.parse(text, Config) -> Ok(typed) | Err(reason)`
///
/// One document. A file holding several (`---` between them) is an `Err`
/// naming the count, because taking the first one silently is how half a
/// manifest bundle gets applied; `yaml.documents` returns them all.
fn parse(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "yaml.parse", "the text to parse", span)?;
    let mut docs = match load(&text) {
        Ok(docs) => docs,
        Err(reason) => return Ok(err(reason)),
    };
    match docs.len() {
        0 => return Ok(err("the document is empty")),
        1 => {}
        n => {
            return Ok(err(format!(
                "this text holds {n} documents; use `yaml.documents` to read them all"
            )))
        }
    }
    let parsed = docs.remove(0);

    let Some(type_arg) = args.get(1) else {
        return Ok(ok(json_to_value(&parsed).with_label(Label::UNVERIFIED)));
    };
    let type_name = declared_type(type_arg, "yaml.parse", span)?;
    match coerce_to_type(interp, &parsed, &type_name, "$", span)? {
        Ok(value) => Ok(ok(value.with_label(Label::UNVERIFIED))),
        Err(message) => Ok(err(message)),
    }
}

/// `yaml.documents(text) -> Ok(list) | Err(reason)`
/// `yaml.documents(text, Service) -> Ok(list[Service]) | Err(reason)`
///
/// The multi-document form, which is how manifests and bundles are actually
/// written. With a declared type every document is checked against it, and
/// the path in a mismatch names which one failed (`$.2.image`).
fn documents(
    interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "yaml.documents", "the text to parse", span)?;
    let docs = match load(&text) {
        Ok(docs) => docs,
        Err(reason) => return Ok(err(reason)),
    };

    let type_name = match args.get(1) {
        Some(arg) => Some(declared_type(arg, "yaml.documents", span)?),
        None => None,
    };

    let mut out = Vec::with_capacity(docs.len());
    for (index, doc) in docs.iter().enumerate() {
        match &type_name {
            None => out.push(json_to_value(doc)),
            Some(name) => match coerce_to_type(interp, doc, name, &format!("$.{index}"), span)? {
                Ok(value) => out.push(value),
                Err(message) => return Ok(err(message)),
            },
        }
    }
    Ok(ok(
        Value::List(Rc::new(std::cell::RefCell::new(out))).with_label(Label::UNVERIFIED)
    ))
}

/// The shared "second argument must be a declared type" check.
fn declared_type(arg: &Value, called: &str, span: Span) -> Result<String, RuntimeError> {
    match arg.unlabeled() {
        Value::TypeRef { name } => Ok(name.to_string()),
        other => Err(RuntimeError::new(
            format!(
                "{called}() expects a declared type as its second argument, got {}",
                other.type_name()
            ),
            span,
        )
        .with_hint("declare the shape with `type Config:` and pass `Config`")),
    }
}

/// `yaml.stringify(value) -> Ok(text) | Err(reason)`
fn stringify(
    interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let Some(value) = args.first() else {
        return Err(RuntimeError::new("yaml.stringify() needs a value", span));
    };
    // Same rule `json.stringify` follows: writing a config file is usually a
    // prelude to sending it somewhere.
    require_not_classified(interp, value, "yaml.stringify", span)?;
    let Some(json) = value_to_json(value) else {
        return Ok(err(format!(
            "{} cannot be represented as YAML",
            value.type_name()
        )));
    };
    let mut text = String::new();
    let mut emitter = YamlEmitter::new(&mut text);
    match emitter.dump(&json_to_yaml(&json)) {
        // The emitter opens every document with `---`. That is correct YAML
        // and round-trips, but it is noise at the top of a one-document file
        // that nothing else writes, so it comes off.
        Ok(()) => {
            let body = text.strip_prefix("---\n").unwrap_or(&text);
            Ok(ok(Value::Str(Rc::new(body.to_string()))))
        }
        Err(e) => Ok(err(format!("could not encode: {e}"))),
    }
}

// --- loading ---

/// Parse every document in `text`, or the first failure as a message that
/// names a line and column.
fn load(text: &str) -> Result<Vec<J>, String> {
    let mut sink = Load::default();
    let mut parser = Parser::new_from_str(text);
    if let Err(e) = parser.load(&mut sink, true) {
        // A malformed file: the parser's own complaint, given a position.
        let mark = e.marker();
        return Err(format!(
            "invalid YAML at line {}, column {}: {}",
            mark.line(),
            mark.col() + 1,
            e.info()
        ));
    }
    match sink.error {
        Some(reason) => Err(reason),
        None => Ok(sink.docs),
    }
}

/// A container being built.
enum Frame {
    Seq {
        anchor: usize,
        items: Vec<J>,
    },
    Map {
        anchor: usize,
        entries: Map<String, J>,
        key: Option<String>,
        /// Keys that arrived through a `<<` merge. An explicit key may
        /// override one of these -- that is what a merge is for -- while two
        /// explicit spellings of the same key stay an error.
        merged: HashSet<String>,
    },
}

/// Builds documents from the event stream rather than using the crate's own
/// loader, for the three reasons in this module's header: the loader resolves
/// aliases with no budget, and it has nowhere to report a duplicate key or a
/// non-scalar key with the line it appeared on.
#[derive(Default)]
struct Load {
    docs: Vec<J>,
    stack: Vec<Frame>,
    anchors: HashMap<usize, J>,
    nodes: usize,
    /// The first failure. Later events are ignored once this is set: the
    /// parser has no way to be told to stop, and one message about the real
    /// cause beats a cascade of consequences.
    error: Option<String>,
}

impl MarkedEventReceiver for Load {
    fn on_event(&mut self, ev: Event, mark: Marker) {
        if self.error.is_some() {
            return;
        }
        match ev {
            Event::Scalar(value, style, anchor, tag) => {
                match scalar(&value, style, tag.as_ref(), mark) {
                    Ok(node) => self.finish(node, anchor, mark),
                    Err(reason) => self.error = Some(reason),
                }
            }
            Event::SequenceStart(anchor, _) => {
                self.open(
                    Frame::Seq {
                        anchor,
                        items: Vec::new(),
                    },
                    mark,
                );
            }
            Event::MappingStart(anchor, _) => {
                self.open(
                    Frame::Map {
                        anchor,
                        entries: Map::new(),
                        key: None,
                        merged: HashSet::new(),
                    },
                    mark,
                );
            }
            Event::SequenceEnd | Event::MappingEnd => {
                let (node, anchor) = match self.stack.pop() {
                    Some(Frame::Seq { anchor, items }) => (J::Array(items), anchor),
                    Some(Frame::Map {
                        anchor, entries, ..
                    }) => (J::Object(entries), anchor),
                    // The parser never emits an unmatched end event.
                    None => return,
                };
                self.finish(node, anchor, mark);
            }
            Event::Alias(id) => match self.anchors.get(&id).cloned() {
                Some(node) => {
                    // The expansion counts against the budget, which is the
                    // whole point: the bomb is small until it is resolved.
                    let cost = count(&node);
                    if self.spend(cost, mark) {
                        self.push(node, mark);
                    }
                }
                None => {
                    self.error = Some(at(mark, "an alias with no matching anchor"));
                }
            },
            Event::Nothing
            | Event::StreamStart
            | Event::StreamEnd
            | Event::DocumentStart
            | Event::DocumentEnd => {}
        }
    }
}

impl Load {
    fn open(&mut self, frame: Frame, mark: Marker) {
        if !self.spend(1, mark) {
            return;
        }
        if self.stack.len() >= MAX_DEPTH {
            self.error = Some(at(
                mark,
                &format!("nested more than {MAX_DEPTH} levels deep"),
            ));
            return;
        }
        self.stack.push(frame);
    }

    /// A finished node: remember it if it was anchored, then place it.
    fn finish(&mut self, node: J, anchor: usize, mark: Marker) {
        // A valid anchor id starts at 1.
        if anchor > 0 {
            self.anchors.insert(anchor, node.clone());
        }
        self.push(node, mark);
    }

    /// Place a finished node in whatever is being built, or in `docs` when
    /// nothing is.
    fn push(&mut self, node: J, mark: Marker) {
        match self.stack.last_mut() {
            None => self.docs.push(node),
            Some(Frame::Seq { items, .. }) => items.push(node),
            Some(Frame::Map {
                entries,
                key,
                merged,
                ..
            }) => match key.take() {
                None => match map_key(&node) {
                    // A merge key is not a key: it is checked when its value
                    // arrives, and one mapping may carry only one.
                    Some(k) if k == MERGE_KEY => *key = Some(k),
                    Some(k) => {
                        if entries.contains_key(&k) && !merged.contains(&k) {
                            self.error =
                                Some(at(mark, &format!("`{k}` is set twice in this mapping")));
                        } else {
                            *key = Some(k);
                        }
                    }
                    None => {
                        self.error = Some(at(
                            mark,
                            "a mapping key must be a scalar, not a list or another mapping",
                        ));
                    }
                },
                Some(k) if k == MERGE_KEY => {
                    // `<<: *defaults`, or `<<: [*a, *b]` for several. An
                    // explicit key already in the mapping wins, and so does an
                    // earlier merge, which is the one behaviour every YAML
                    // implementation agrees on.
                    let sources = match &node {
                        J::Object(_) => vec![&node],
                        J::Array(items) => items.iter().collect(),
                        _ => {
                            self.error = Some(at(
                                mark,
                                "`<<` merges a mapping, or a list of mappings, and got neither",
                            ));
                            return;
                        }
                    };
                    for source in sources {
                        let J::Object(fields) = source else {
                            self.error = Some(at(
                                mark,
                                "`<<` merges a mapping, or a list of mappings, and got neither",
                            ));
                            return;
                        };
                        for (name, value) in fields {
                            if !entries.contains_key(name) {
                                entries.insert(name.clone(), value.clone());
                                merged.insert(name.clone());
                            }
                        }
                    }
                }
                Some(k) => {
                    merged.remove(&k);
                    entries.insert(k, node);
                }
            },
        }
    }

    /// Charge `n` nodes against the budget. `false` once it is spent.
    fn spend(&mut self, n: usize, mark: Marker) -> bool {
        self.nodes = self.nodes.saturating_add(n);
        if self.nodes > MAX_NODES {
            self.error = Some(at(
                mark,
                &format!(
                    "this document expands past {MAX_NODES} values, which no configuration \
                     needs; an anchor is being used to multiply it"
                ),
            ));
            return false;
        }
        true
    }
}

/// YAML's merge key. Not part of the 1.2 core schema, but every
/// docker-compose and CI file in the world uses it, so reading it as a
/// literal key named `<<` would be a wrong answer rather than a missing
/// feature.
const MERGE_KEY: &str = "<<";

/// A key as it will appear in the parsed value. YAML allows any node as a
/// key; a scalar becomes the text it was written as, and a collection has no
/// representation, which the caller turns into an error.
fn map_key(node: &J) -> Option<String> {
    Some(match node {
        J::String(s) => s.clone(),
        J::Number(n) => n.to_string(),
        J::Bool(b) => b.to_string(),
        J::Null => "null".to_string(),
        J::Array(_) | J::Object(_) => return None,
    })
}

/// How many values a node holds, counting itself.
fn count(node: &J) -> usize {
    match node {
        J::Array(items) => 1 + items.iter().map(count).sum::<usize>(),
        J::Object(map) => 1 + map.values().map(count).sum::<usize>(),
        _ => 1,
    }
}

/// One scalar, resolved under the YAML 1.2 core schema.
///
/// Quoting decides the type, which is the reason quoting exists: `"true"` is
/// a string and `true` is a boolean. Only those two spellings are boolean —
/// see the Norway problem in this module's header.
fn scalar(value: &str, style: TScalarStyle, tag: Option<&Tag>, mark: Marker) -> Result<J, String> {
    if let Some(Tag { handle, suffix }) = tag {
        if handle == "tag:yaml.org,2002:" {
            return match suffix.as_str() {
                "str" => Ok(J::String(value.to_string())),
                "bool" => match value {
                    "true" | "True" | "TRUE" => Ok(J::Bool(true)),
                    "false" | "False" | "FALSE" => Ok(J::Bool(false)),
                    _ => Err(at(
                        mark,
                        &format!("`{value}` is tagged !!bool but is not one"),
                    )),
                },
                "int" => integer(value)
                    .ok_or_else(|| at(mark, &format!("`{value}` is tagged !!int but is not one"))),
                "float" => float(value, mark).and_then(|v| {
                    v.ok_or_else(|| {
                        at(mark, &format!("`{value}` is tagged !!float but is not one"))
                    })
                }),
                "null" => Ok(J::Null),
                _ => Ok(J::String(value.to_string())),
            };
        }
    }
    if style != TScalarStyle::Plain {
        return Ok(J::String(value.to_string()));
    }
    Ok(match value {
        "" | "~" | "null" | "Null" | "NULL" => J::Null,
        "true" | "True" | "TRUE" => J::Bool(true),
        "false" | "False" | "FALSE" => J::Bool(false),
        _ => {
            if let Some(n) = integer(value) {
                n
            } else if let Some(n) = float(value, mark)? {
                n
            } else {
                J::String(value.to_string())
            }
        }
    })
}

fn integer(value: &str) -> Option<J> {
    let (sign, digits) = match value.strip_prefix('-') {
        Some(rest) => (-1i64, rest),
        None => (1i64, value.strip_prefix('+').unwrap_or(value)),
    };
    let magnitude = if let Some(hex) = digits.strip_prefix("0x") {
        i64::from_str_radix(hex, 16).ok()?
    } else if let Some(octal) = digits.strip_prefix("0o") {
        i64::from_str_radix(octal, 8).ok()?
    } else if digits.bytes().all(|b| b.is_ascii_digit()) && !digits.is_empty() {
        digits.parse::<i64>().ok()?
    } else {
        return None;
    };
    Some(J::from(sign * magnitude))
}

/// `Ok(None)` when the text is not a number at all. The error case is the one
/// that *is* a number and has no Kora value: `.inf` and `.nan` are real YAML
/// floats with no representation here, and turning them into the string
/// `".inf"` would be a wrong answer rather than a missing one.
fn float(value: &str, mark: Marker) -> Result<Option<J>, String> {
    match value {
        ".inf" | ".Inf" | ".INF" | "+.inf" | "+.Inf" | "+.INF" | "-.inf" | "-.Inf" | "-.INF"
        | ".nan" | ".NaN" | ".NAN" => {
            return Err(at(
                mark,
                &format!("`{value}` has no value in Kora; write it as a quoted string if the text is what you want"),
            ))
        }
        _ => {}
    }
    // Rust parses `inf`, `NaN`, and `1_0` in ways YAML does not, so the shape
    // is checked before the parse rather than after.
    let looks_numeric = value
        .starts_with(|c: char| c == '-' || c == '+' || c == '.' || c.is_ascii_digit())
        && value.bytes().any(|b| b.is_ascii_digit())
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'));
    if !looks_numeric {
        return Ok(None);
    }
    match value.parse::<f64>() {
        Ok(f) => Ok(serde_json::Number::from_f64(f).map(J::Number)),
        Err(_) => Ok(None),
    }
}

fn at(mark: Marker, message: &str) -> String {
    format!("line {}, column {}: {message}", mark.line(), mark.col() + 1)
}

/// The way back out, for `stringify`. Every case is reachable: a Kora value
/// became JSON first, and that conversion already refused everything with no
/// data representation.
fn json_to_yaml(json: &J) -> Yaml {
    match json {
        J::Null => Yaml::Null,
        J::Bool(b) => Yaml::Boolean(*b),
        J::Number(n) => match n.as_i64() {
            Some(i) => Yaml::Integer(i),
            None => Yaml::Real(n.to_string()),
        },
        J::String(s) => Yaml::String(s.clone()),
        J::Array(items) => Yaml::Array(items.iter().map(json_to_yaml).collect()),
        J::Object(map) => Yaml::Hash(
            map.iter()
                .map(|(k, v)| (Yaml::String(k.clone()), json_to_yaml(v)))
                .collect(),
        ),
    }
}
