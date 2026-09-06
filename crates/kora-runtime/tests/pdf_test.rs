//! `pdf` — the failures that usually look like success.
//!
//! Every test here is a case where the ordinary way of reading a PDF returns
//! something the program will happily use: an empty string for a scan, a
//! truncated document for an unreadable page, a crash for a malformed file.
//! The fixtures are built here rather than committed, so what each one
//! contains is readable in the test that uses it.

use kora_runtime::{Config, Interpreter};
use kora_syntax::parse;
use lopdf::content::{Content, Operation};
use lopdf::{dictionary, Document, Object, Stream};

const CONFIG: &str = "[models]\ndefault = \"local:test-model\"\n";

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

struct Scratch(std::path::PathBuf);

impl Scratch {
    fn new(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!(
            "kora-pdf-{name}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::remove_dir_all(&dir).ok();
        std::fs::create_dir_all(&dir).unwrap();
        Scratch(dir)
    }

    /// A path safe to paste into Kora source: a Windows backslash would read
    /// as an escape sequence in a string literal.
    fn path(&self, name: &str) -> String {
        self.0.join(name).to_string_lossy().replace('\\', "\\\\")
    }

    /// A PDF with one page per entry. An empty entry is a page with no text
    /// on it, which is what a scanned page looks like to a text extractor.
    fn pdf(&self, name: &str, pages: &[&str]) -> String {
        write_pdf(&self.0.join(name), pages);
        self.path(name)
    }

    fn write(&self, name: &str, bytes: &[u8]) -> String {
        std::fs::write(self.0.join(name), bytes).unwrap();
        self.path(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).ok();
    }
}

/// Build a PDF whose pages carry exactly the given text.
fn write_pdf(path: &std::path::Path, pages: &[&str]) {
    let mut doc = Document::with_version("1.5");
    let pages_id = doc.new_object_id();
    let font_id = doc.add_object(dictionary! {
        "Type" => "Font",
        "Subtype" => "Type1",
        "BaseFont" => "Helvetica",
    });
    let resources_id = doc.add_object(dictionary! {
        "Font" => dictionary! { "F1" => font_id },
    });

    let mut page_ids = Vec::new();
    for text in pages {
        let operations = if text.is_empty() {
            // A page with no text operations at all: the extractor finds
            // nothing, exactly as it would on a scan.
            Vec::new()
        } else {
            vec![
                Operation::new("BT", vec![]),
                Operation::new("Tf", vec!["F1".into(), 24.into()]),
                Operation::new("Td", vec![72.into(), 720.into()]),
                Operation::new("Tj", vec![Object::string_literal(*text)]),
                Operation::new("ET", vec![]),
            ]
        };
        let content = Content { operations };
        let content_id = doc.add_object(Stream::new(dictionary! {}, content.encode().unwrap()));
        page_ids.push(
            doc.add_object(dictionary! {
                "Type" => "Page",
                "Parent" => pages_id,
                "Contents" => content_id,
            })
            .into(),
        );
    }

    let count = page_ids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(dictionary! {
            "Type" => "Pages",
            "Kids" => page_ids,
            "Count" => count,
            "Resources" => resources_id,
            "MediaBox" => vec![0.into(), 0.into(), 595.into(), 842.into()],
        }),
    );
    let catalog_id = doc.add_object(dictionary! {
        "Type" => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);
    doc.save(path).unwrap();
}

#[test]
fn text_reads_the_document() {
    let scratch = Scratch::new("text");
    let path = scratch.pdf("invoice.pdf", &["Invoice 41", "Total 90"]);
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.text("{path}"):
        case Ok(text):
            print(text)
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    let text = out.join("\n");
    assert!(text.contains("Invoice 41"), "{text}");
    assert!(text.contains("Total 90"), "{text}");
}

/// The boundary every other extractor throws away.
#[test]
fn pages_keep_their_boundaries() {
    let scratch = Scratch::new("pages");
    let path = scratch.pdf("contract.pdf", &["Page one", "Page two", "Page three"]);
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.pages("{path}"):
        case Ok(pages):
            print(len(pages))
            print(pages[1])
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    assert_eq!(out[0], "3", "{out:?}");
    assert!(out[1].contains("Page two"), "{out:?}");
}

/// The headline defect: `pypdf` returns `""` here and the pipeline writes
/// empty records for the rest of the afternoon.
#[test]
fn a_document_without_text_is_an_error_not_an_empty_string() {
    let scratch = Scratch::new("scan");
    let path = scratch.pdf("scan.pdf", &["", ""]);
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.text("{path}"):
        case Ok(text):
            print(f"ok: {{text}}")
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    assert!(out[0].starts_with("failed:"), "{out:?}");
    assert!(out[0].contains("no text layer"), "{out:?}");
}

/// And the same document answers the question up front, so a program can
/// take the vision path on purpose.
#[test]
fn info_says_whether_there_is_text_to_read() {
    let scratch = Scratch::new("info");
    let scanned = scratch.pdf("scan.pdf", &["", "", ""]);
    let digital = scratch.pdf("digital.pdf", &["Readable"]);
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.info("{scanned}"):
        case Ok(facts):
            print(f"{{facts["page_count"]}} {{facts["has_text_layer"]}} {{facts["encrypted"]}}")
        case Err(why):
            print(f"failed: {{why}}")
    match pdf.info("{digital}"):
        case Ok(facts):
            print(f"{{facts["page_count"]}} {{facts["has_text_layer"]}}")
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    assert_eq!(out[0], "3 False False", "{out:?}");
    assert_eq!(out[1], "1 True", "{out:?}");
}

/// A parser fed a file it did not write must not end the run.
#[test]
fn a_malformed_pdf_is_an_error_not_a_crash() {
    let scratch = Scratch::new("malformed");
    let path = scratch.write("broken.pdf", b"%PDF-1.7\nthis is not a pdf body at all\n");
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.text("{path}"):
        case Ok(text):
            print(f"ok: {{text}}")
        case Err(why):
            print("failed")
"#
    ));
    assert_eq!(out[0], "failed", "{out:?}");
}

#[test]
fn a_file_that_is_not_a_pdf_says_so() {
    let scratch = Scratch::new("nonpdf");
    let path = scratch.write("notes.txt", b"plain text, no header");
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.pages("{path}"):
        case Ok(pages):
            print("ok")
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    assert!(out[0].contains("is not a PDF"), "{out:?}");
}

#[test]
fn a_missing_file_names_itself() {
    let scratch = Scratch::new("missing");
    let path = scratch.path("nowhere.pdf");
    let out = run(&format!(
        r#"
use pdf

def main():
    match pdf.text("{path}"):
        case Ok(text):
            print("ok")
        case Err(why):
            print(f"failed: {{why}}")
"#
    ));
    assert!(out[0].contains("no such file"), "{out:?}");
}

/// The same rule `fs.read` follows: a path the program did not validate is
/// refused at the call, not audited later.
#[test]
fn an_unverified_path_is_refused() {
    let scratch = Scratch::new("unverified");
    scratch.write("target.txt", b"invoice.pdf");
    let listing = scratch.path("target.txt");
    let source = format!(
        r#"
use pdf
use fs

def main():
    match fs.read("{listing}"):
        case Ok(untrusted):
            match pdf.text(untrusted):
                case Ok(text):
                    print("ok")
                case Err(why):
                    print(why)
        case Err(why):
            print(why)
"#
    );
    let message = run_err(&source);
    assert!(message.contains("pdf.text"), "{message}");
    assert!(message.contains("came from outside"), "{message}");
}

/// Text out of a document is `unverified`, so it cannot reach a sink without
/// being narrowed first — a PDF body is a standard injection carrier.
#[test]
fn extracted_text_is_unverified() {
    let scratch = Scratch::new("label");
    let path = scratch.pdf("invoice.pdf", &["Invoice 41"]);
    let source = format!(
        r#"
use pdf
use fs

def main():
    match pdf.text("{path}"):
        case Ok(text):
            match fs.read(text):
                case Ok(_):
                    print("read")
                case Err(why):
                    print(why)
        case Err(why):
            print(why)
"#
    );
    let message = run_err(&source);
    assert!(message.contains("fs.read"), "{message}");
    assert!(message.contains("came from outside"), "{message}");
}
