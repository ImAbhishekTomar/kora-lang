//! `xml` — the four defects it exists to fix, and the walk on top of them.
//!
//! The first four tests would each fail against `xml.etree`, `lxml` in its
//! default configuration, or `xmltodict`, which is why this is a module in
//! the language rather than something left to a package.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;

const CONFIG: &str = r#"
[models]
default = "local:test-model"

[sinks]
local_model = { allow = ["classified"] }
"#;

fn run(src: &str) -> Vec<String> {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    interp
        .run(&program)
        .unwrap_or_else(|e| panic!("runtime error: {}\n{src}", e.message));
    interp.output
}

fn run_err(src: &str) -> String {
    let program = parse(src).unwrap_or_else(|e| panic!("parse error: {e}\n{src}"));
    let mut interp = Interpreter::new();
    let config = Config::parse(CONFIG).unwrap();
    interp.sinks = config.sinks.clone();
    interp.config = config;
    match interp.run(&program) {
        Err(e) => e.message,
        Ok(_) => panic!("expected an error, program succeeded:\n{src}"),
    }
}

/// XML into a Kora string literal.
fn escape(text: &str) -> String {
    text.replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}

/// Parse `xml` and print the value at `path` inside the root element.
fn parse_and_get(xml: &str, path: &str) -> Vec<String> {
    run(&format!(
        r#"use xml
def main():
    match xml.parse("{}"):
        case Ok(root):
            match xml.get(root, "{path}"):
                case Ok(v):
                    print(v)
                case Err(w):
                    print(w)
        case Err(why):
            print(why)
"#,
        escape(xml)
    ))
}

// --- the four defects ---

#[test]
fn a_doctype_is_refused_so_there_is_no_entity_to_expand() {
    // The fix: an entity in a DTD can name a local file or an internal URL,
    // and a parser that resolves one is a file reader and an HTTP client.
    // Python needed a separate library (defusedxml) because the standard
    // one's defaults could not be changed.
    let xxe = "<!DOCTYPE foo [<!ENTITY xxe SYSTEM \"file:///etc/passwd\">]><foo>&xxe;</foo>";
    let out = parse_and_get(xxe, "tag");
    assert!(out[0].contains("DOCTYPE"), "got: {}", out[0]);
    assert!(
        out[0].contains("local file") || out[0].contains("internal URL"),
        "the message must say what the risk is: {}",
        out[0]
    );
}

#[test]
fn a_billion_laughs_document_never_gets_to_expand_either() {
    // It needs a DTD to define the entities, so the same refusal covers it.
    let mut bomb = String::from("<!DOCTYPE lolz [<!ENTITY a \"aaaaaaaaaa\">");
    for (n, prev) in [('b', 'a'), ('c', 'b'), ('d', 'c')] {
        bomb.push_str(&format!(
            "<!ENTITY {n} \"&{prev};&{prev};&{prev};&{prev};&{prev};&{prev};&{prev};&{prev};&{prev};&{prev};\">"
        ));
    }
    bomb.push_str("]><lolz>&d;</lolz>");
    let out = parse_and_get(&bomb, "tag");
    assert!(out[0].contains("DOCTYPE"), "got: {}", out[0]);
}

#[test]
fn a_namespace_is_a_field_not_a_prefix_glued_to_the_tag() {
    // The fix: ElementTree reports `{http://example.com}item`, so code strips
    // the brace prefix and then matches an element from the wrong namespace.
    let doc = "<feed xmlns=\"http://www.w3.org/2005/Atom\"><entry/></feed>";
    assert_eq!(parse_and_get(doc, "tag"), vec!["feed"]);
    assert_eq!(
        parse_and_get(doc, "ns"),
        vec!["http://www.w3.org/2005/Atom"]
    );
    // And the local name is what a lookup matches, so adding a default
    // namespace to a document does not break every query written against it.
    assert_eq!(parse_and_get(doc, "children.0.tag"), vec!["entry"]);
}

#[test]
fn character_data_is_not_split_between_text_and_tail() {
    // The fix: `<p>Hello <b>world</b>!</p>` puts "Hello " on p.text and "!"
    // on b.tail, so the obvious read of p.text loses two thirds of it.
    let out = parse_and_get("<p>Hello <b>world</b>!</p>", "text");
    assert_eq!(out, vec!["Hello world!"]);
}

#[test]
fn one_child_is_a_list_of_one_not_an_object() {
    // The fix: xmltodict gives one child as an object and two as a list, so
    // a program tested against a two-item feed crashes on a one-item feed.
    let one = run(r#"use xml
def main():
    match xml.parse("<feed><item>a</item></feed>"):
        case Ok(root):
            match xml.find_all(root, "item"):
                case Ok(items):
                    print(len(items))
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    assert_eq!(one, vec!["1"]);

    let two = run(r#"use xml
def main():
    match xml.parse("<feed><item>a</item><item>b</item></feed>"):
        case Ok(root):
            match xml.find_all(root, "item"):
                case Ok(items):
                    print(len(items))
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    assert_eq!(two, vec!["2"]);
}

// --- walking ---

#[test]
fn find_returns_the_first_element_on_a_path() {
    let out = run(r#"use xml
def main():
    match xml.parse("<rss><channel><item><title>one</title></item><item><title>two</title></item></channel></rss>"):
        case Ok(root):
            match xml.find(root, "channel.item.title"):
                case Ok(title):
                    match xml.text(title):
                        case Ok(t):
                            print(t)
                        case Err(w):
                            print(w)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["one"]);
}

#[test]
fn find_all_returns_every_element_on_a_path() {
    let out = run(r#"use xml
def main():
    match xml.parse("<rss><channel><item><title>one</title></item><item><title>two</title></item></channel></rss>"):
        case Ok(root):
            match xml.find_all(root, "channel.item.title"):
                case Ok(titles):
                    for title in titles:
                        match xml.text(title):
                            case Ok(t):
                                print(t)
                            case Err(w):
                                print(w)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["one", "two"]);
}

#[test]
fn a_dead_end_partway_through_a_path_says_where_it_stopped() {
    // "not found" with no position is the error everyone has to add a print
    // statement to understand.
    let out = run(r#"use xml
def main():
    match xml.parse("<rss><channel/></rss>"):
        case Ok(root):
            match xml.find(root, "chanel.item"):
                case Ok(_):
                    print("unreachable")
                case Err(why):
                    print(why)
        case Err(w):
            print(w)
"#);
    assert!(out[0].contains("chanel"), "got: {}", out[0]);
    assert!(out[0].contains("no such element"), "got: {}", out[0]);
}

#[test]
fn no_matches_at_the_end_of_a_path_is_an_empty_list_not_an_error() {
    // "no items in this feed" is an answer; "no channel to look inside" is a
    // mistake. Collapsing the two would make an empty feed indistinguishable
    // from a typo.
    let out = run(r#"use xml
def main():
    match xml.parse("<rss><channel/></rss>"):
        case Ok(root):
            match xml.find_all(root, "channel.item"):
                case Ok(items):
                    print(len(items))
                case Err(why):
                    print(why)
        case Err(w):
            print(w)
"#);
    assert_eq!(out, vec!["0"]);
}

// --- attributes ---

#[test]
fn attributes_are_keyed_by_local_name() {
    let out = parse_and_get("<item id='7' kind='book'/>", "attrs.id");
    assert_eq!(out, vec!["7"]);
}

#[test]
fn two_attributes_with_one_local_name_are_refused_rather_than_merged() {
    // The same rule `yaml` applies to a duplicate mapping key: silently
    // keeping the last is an edit nobody can see in the parsed result.
    let doc = "<e xmlns:n='http://example.com' a='one' n:a='two'/>";
    let out = parse_and_get(doc, "attrs.a");
    assert!(out[0].contains("two `a` attributes"), "got: {}", out[0]);
}

// --- the rules every stdlib module follows ---

#[test]
fn a_parsed_document_is_unverified() {
    let err = run_err(
        r#"use fs
use xml
def main():
    match xml.parse("<config><path>/etc/passwd</path></config>"):
        case Ok(root):
            match xml.find(root, "path"):
                case Ok(node):
                    match xml.text(node):
                        case Ok(target):
                            fs.read(target)
                        case Err(w):
                            print(w)
                case Err(w):
                    print(w)
        case Err(w):
            print(w)
"#,
    );
    assert!(
        err.contains("came from outside"),
        "the label must survive parse -> find -> text, got: {err}"
    );
}

#[test]
fn malformed_xml_names_a_line_and_column() {
    let out = parse_and_get("<a><b></a>", "tag");
    assert!(out[0].contains("invalid XML at line"), "got: {}", out[0]);
}

#[test]
fn passing_something_that_is_not_an_element_says_so() {
    let err = run_err(
        r#"use xml
def main():
    match xml.text("not an element"):
        case Ok(t):
            print(t)
        case Err(w):
            print(w)
"#,
    );
    assert!(err.contains("expects an element, got str"), "got: {err}");
}
