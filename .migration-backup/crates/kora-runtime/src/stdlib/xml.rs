//! `xml` — the format most enterprise data still arrives in, read safely.
//!
//! XML is what a bank statement, a SOAP response, an RSS feed, and half the
//! government data in the world still look like. Four defects follow every
//! mainstream reader, and none of them can be fixed there without breaking
//! documents already in the world:
//!
//! 1. **The parser will read your files and make your requests.** A `DOCTYPE`
//!    may define an entity pointing at `/etc/passwd` or at an internal URL,
//!    and a parser that resolves it hands the attacker both. Python needed a
//!    separate library (`defusedxml`) because `xml.etree`'s defaults could
//!    not be changed. Here a DTD is refused outright, so there is no entity
//!    to expand and no request to make — and the "billion laughs" expansion
//!    goes with it, since that needs a DTD too.
//! 2. **Namespaces get mangled.** `ElementTree` reports a tag as
//!    `{http://example.com}item`, so code strips the brace prefix and then
//!    matches an element from the wrong namespace. Here an element carries
//!    its namespace in its own field, and a lookup matches the local name, so
//!    a document that gains a default namespace does not break every query.
//! 3. **Character data is split in two.** `<p>Hello <b>world</b>!</p>` puts
//!    `"Hello "` on `p.text` and `"!"` on `b.tail`, so the obvious read of
//!    `p.text` silently loses two thirds of the sentence. Here `text` is all
//!    of an element's character data, in document order, and there is no
//!    second place for the rest of it to hide.
//! 4. **The shape depends on the data.** `xmltodict` and friends give one
//!    child as an object and two as a list, so a program tested against a
//!    two-item feed crashes the day a feed has one item. Here `children` is
//!    always a list, of any length, including zero.
//!
//! An element is an ordinary Kora value, so `xml.get` and the `json.get` path
//! walk both work on it:
//!
//! ```text
//! {"tag": "item", "ns": None, "attrs": {"id": "1"},
//!  "text": "Hello world!", "children": [...]}
//! ```

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::token::Span;
use roxmltree::{Document, Node, ParsingOptions};

use super::json::get as json_get;
use super::{err, ok, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[
    ("parse", parse),
    ("find", find),
    ("find_all", find_all),
    ("text", text),
    ("get", json_get),
];

/// How many nodes one document may hold.
///
/// A DTD is already refused, so this is not the entity-expansion defense --
/// that one is structural. This is the plain "somebody sent a 900 MB feed"
/// ceiling, and it is the same number `yaml` uses, because a program reading
/// both should not have to learn two limits.
const MAX_NODES: u32 = 250_000;

/// `xml.parse(text) -> Ok(element) | Err(reason)`
///
/// The document's root element. There is deliberately no `xml.parse(text,
/// Type)` to match `json` and `yaml`: in XML a value can live in an attribute
/// or in a child element, and a parser that guessed which one a field meant
/// would be making exactly the shape-depends-on-the-data mistake this module
/// exists to refuse. Walk to what you need with `xml.find` and build the
/// declared type yourself.
fn parse(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "xml.parse", "the text to parse", span)?;
    let options = ParsingOptions {
        // The whole of the XXE and billion-laughs attack class, refused by
        // the parser rather than by a setting someone can turn off.
        allow_dtd: false,
        nodes_limit: MAX_NODES,
        ..ParsingOptions::default()
    };
    let document = match Document::parse_with_options(&text, options) {
        Ok(document) => document,
        Err(e) => return Ok(err(describe(&e))),
    };
    match element_to_value(document.root_element()) {
        Ok(value) => Ok(ok(value.with_label(Label::UNVERIFIED))),
        Err(reason) => Ok(err(reason)),
    }
}

/// `xml.find(element, "channel.item") -> Ok(element) | Err(reason)`
///
/// The first element on that path of local names, searched among children at
/// each step. `Err` when nothing matches, naming how far the walk got, the
/// same way `json.get` does -- "not found" with no position is the error
/// everyone has to add a print statement to understand.
fn find(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (root, label) = element_arg(&args, "xml.find", span)?;
    let path = str_arg(&args, 1, "xml.find", "a path of tag names", span)?;
    match walk(&root, &path)? {
        Ok(mut found) => match found.is_empty() {
            true => unreachable!("walk returns Err when nothing matched"),
            false => Ok(ok(found.remove(0).with_label(label))),
        },
        Err(reason) => Ok(err(reason)),
    }
}

/// `xml.find_all(element, "channel.item") -> Ok(list) | Err(reason)`
///
/// Every element on that path. An empty list is a success, not an error: "no
/// items in this feed" is an answer, while "no `channel` to look inside" is a
/// mistake, and the two are told apart here rather than by the caller.
fn find_all(
    _interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let (root, label) = element_arg(&args, "xml.find_all", span)?;
    let path = str_arg(&args, 1, "xml.find_all", "a path of tag names", span)?;
    match walk_all(&root, &path)? {
        Ok(found) => Ok(ok(
            Value::List(Rc::new(RefCell::new(found))).with_label(label)
        )),
        Err(reason) => Ok(err(reason)),
    }
}

/// `xml.text(element) -> Ok(text) | Err(reason)`
///
/// The convenience form of reading the `text` field, so the common case
/// reads as one call rather than a dictionary lookup that can miss.
fn text(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (element, label) = element_arg(&args, "xml.text", span)?;
    match element.get("text") {
        Some(value) => Ok(ok(value.clone().with_label(label))),
        None => Ok(err("this value is not an element: it has no `text`")),
    }
}

/// The first argument, which every function here takes: an element, as
/// produced by `xml.parse`. Its label travels with whatever comes out.
fn element_arg(
    args: &[Value],
    called: &str,
    span: Span,
) -> Result<(HashMap<String, Value>, Label), RuntimeError> {
    let Some(value) = args.first() else {
        return Err(RuntimeError::new(
            format!("{called}() needs an element"),
            span,
        ));
    };
    let label = value.label();
    match value.unlabeled() {
        Value::Dict(map) => Ok((map.borrow().clone(), label)),
        other => Err(RuntimeError::new(
            format!("{called}() expects an element, got {}", other.type_name()),
            span,
        )
        .with_hint("pass what `xml.parse` returned, or an element out of `xml.find`")),
    }
}

/// Walk a dotted path of local names, returning every element at the end of
/// it. The inner `Err` names the segment that had nowhere to go.
fn walk_all(
    root: &HashMap<String, Value>,
    path: &str,
) -> Result<Result<Vec<Value>, String>, RuntimeError> {
    let mut current = vec![root.clone()];
    let mut walked = String::new();
    for segment in path.split('.').filter(|s| !s.is_empty()) {
        if !walked.is_empty() {
            walked.push('.');
        }
        walked.push_str(segment);
        let mut next = Vec::new();
        for element in &current {
            for child in children_of(element) {
                if tag_of(&child).as_deref() == Some(segment) {
                    next.push(child);
                }
            }
        }
        if next.is_empty() {
            // Distinguish "the path is wrong" from "there are none here": a
            // dead end partway through is a mistake, and only the last
            // segment can legitimately match nothing.
            let is_last = walked.len() == path.trim_matches('.').len();
            if !is_last {
                return Ok(Err(format!("{walked}: no such element")));
            }
        }
        current = next;
    }
    Ok(Ok(current
        .into_iter()
        .map(|e| Value::Dict(Rc::new(RefCell::new(e))))
        .collect()))
}

/// `walk_all`, but an empty result is an error: `xml.find` promises one
/// element, so "there were none" has to be a value the caller matches.
fn walk(
    root: &HashMap<String, Value>,
    path: &str,
) -> Result<Result<Vec<Value>, String>, RuntimeError> {
    match walk_all(root, path)? {
        Ok(found) if found.is_empty() => Ok(Err(format!("{path}: no such element"))),
        other => Ok(other),
    }
}

fn children_of(element: &HashMap<String, Value>) -> Vec<HashMap<String, Value>> {
    let Some(Value::List(items)) = element.get("children").map(Value::unlabeled) else {
        return Vec::new();
    };
    items
        .borrow()
        .iter()
        .filter_map(|child| match child.unlabeled() {
            Value::Dict(map) => Some(map.borrow().clone()),
            _ => None,
        })
        .collect()
}

fn tag_of(element: &HashMap<String, Value>) -> Option<String> {
    match element.get("tag").map(Value::unlabeled) {
        Some(Value::Str(s)) => Some(s.to_string()),
        _ => None,
    }
}

/// One element, as the value a Kora program sees.
fn element_to_value(node: Node) -> Result<Value, String> {
    let mut fields: HashMap<String, Value> = HashMap::new();
    fields.insert(
        "tag".to_string(),
        Value::Str(Rc::new(node.tag_name().name().to_string())),
    );
    fields.insert(
        "ns".to_string(),
        match node.tag_name().namespace() {
            Some(uri) => Value::Str(Rc::new(uri.to_string())),
            None => Value::None,
        },
    );

    // Attributes are keyed by local name. Two attributes may share one local
    // name if they are in different namespaces, and silently keeping the last
    // is the same defect `yaml` refuses in a duplicate mapping key.
    let mut attrs: HashMap<String, Value> = HashMap::new();
    for attribute in node.attributes() {
        let name = attribute.name().to_string();
        if attrs.contains_key(&name) {
            return Err(format!(
                "<{}> has two `{name}` attributes, in different namespaces",
                node.tag_name().name()
            ));
        }
        attrs.insert(name, Value::Str(Rc::new(attribute.value().to_string())));
    }
    fields.insert(
        "attrs".to_string(),
        Value::Dict(Rc::new(RefCell::new(attrs))),
    );

    // All character data under this element, in document order: the defect in
    // point 3 of this module's header is that everyone else splits it.
    let mut text = String::new();
    for descendant in node.descendants() {
        if descendant.is_text() {
            text.push_str(descendant.text().unwrap_or_default());
        }
    }
    fields.insert("text".to_string(), Value::Str(Rc::new(text)));

    let mut children = Vec::new();
    for child in node.children() {
        if child.is_element() {
            children.push(element_to_value(child)?);
        }
    }
    fields.insert(
        "children".to_string(),
        Value::List(Rc::new(RefCell::new(children))),
    );

    Ok(Value::Dict(Rc::new(RefCell::new(fields))))
}

/// A parse failure with the position it happened at, and -- for the one error
/// a security default causes -- what the default is and why.
fn describe(e: &roxmltree::Error) -> String {
    if matches!(e, roxmltree::Error::DtdDetected) {
        return "this document has a DOCTYPE, which Kora does not process: an entity in a DTD \
                can name a local file or an internal URL, and expanding one is how a parser is \
                turned into a file reader. Remove the DOCTYPE, or extract the data with a tool \
                that is meant to trust it."
            .to_string();
    }
    let pos = e.pos();
    format!("invalid XML at line {}, column {}: {e}", pos.row, pos.col)
}
