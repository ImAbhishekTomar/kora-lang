//! `pdf` — a document that says what it is, instead of quietly returning
//! nothing.
//!
//! Most of the documents people hand an agent are PDFs, and the usual way to
//! read one has three failure modes that all look like success. Every one of
//! them is fixed here by making the outcome a value the program has to match.
//!
//! **A scanned PDF extracts as `""`.** `pypdf` and friends return an empty
//! string for a page that holds a picture of text rather than text, with no
//! error, so a pipeline runs to completion and writes empty records. The
//! failure is discovered in the output, days later. Here a document with no
//! text layer is `Err`, and `pdf.info` says `has_text_layer` up front so a
//! program can take the vision path deliberately rather than by accident.
//!
//! **Page boundaries are lost.** `pdftotext` and most wrappers concatenate
//! everything, so "which page is this total on" is unanswerable and a
//! page-indexed prompt is impossible. `pdf.pages` keeps the boundaries; the
//! joined form is `pdf.text` and is the derived one, not the other way round.
//!
//! **A page that fails to parse truncates the document.** The convenience
//! helpers in this space stop at the first page they cannot read and return
//! what they got, so a 40-page contract silently becomes 12 pages. Here the
//! page count is read from the document's own catalogue first, and a page
//! that will not parse is an `Err` naming the page number.
//!
//! Two more things this module owes the rest of the language.
//!
//! **Text is `unverified`.** A PDF is outside the program, and its text is
//! the most attacker-shaped input an agent ever reads: a document whose body
//! says "ignore your instructions" is the standard prompt-injection carrier.
//! It cannot reach a sink until something narrows it.
//!
//! **A malformed file cannot take the process down.** This is a parser for
//! untrusted input, so every call into it runs inside `catch_unwind` and a
//! panic comes back as `Err`. Without that, one corrupt PDF in a folder ends
//! a run that has already spent its budget.
//!
//! Rendering a page to pixels is not here: that needs a full renderer with
//! fonts and shading behind it, which is a separate decision with a C++
//! library on the other side of it. `fs.image` still reads images only.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::panic::AssertUnwindSafe;
use std::rc::Rc;
use std::sync::OnceLock;

use kora_syntax::token::Span;

use super::fs::{checked_path, describe_io};
use super::{err, ok};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("text", text), ("pages", pages), ("info", info)];

/// Beyond this the file is refused by name and size rather than by running out
/// of memory somewhere inside the parser. A PDF this large is a scan dump, and
/// the text path is not what it wants anyway.
pub const MAX_BYTES: usize = 64 * 1024 * 1024;

/// `pdf.text(path) -> Ok(text) | Err(reason)`
///
/// Every page, joined by a blank line. `unverified`, like any file content.
fn text(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "pdf.text", span)?;
    super::journaled_read(interp, "pdf.text", span, move |_| {
        match read_pages(&path, "pdf.text") {
            Ok(pages) => ok(Value::Str(Rc::new(pages.join("\n\n"))).with_label(Label::UNVERIFIED)),
            Err(reason) => err(reason),
        }
    })
}

/// `pdf.pages(path) -> Ok(list of text) | Err(reason)`
///
/// One entry per page, in document order, including pages that are blank.
/// The index is the page number, so a prompt can name where something was.
fn pages(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "pdf.pages", span)?;
    super::journaled_read(interp, "pdf.pages", span, move |_| {
        match read_pages(&path, "pdf.pages") {
            Ok(pages) => {
                let items: Vec<Value> = pages
                    .into_iter()
                    .map(|page| Value::Str(Rc::new(page)))
                    .collect();
                ok(Value::List(Rc::new(RefCell::new(items))).with_label(Label::UNVERIFIED))
            }
            Err(reason) => err(reason),
        }
    })
}

/// `pdf.info(path) -> Ok({page_count, encrypted, has_text_layer}) | Err(reason)`
///
/// What a program needs to decide how to read the document, without having to
/// extract it first and guess from an empty string. `has_text_layer` is the
/// one that matters: false means the pages are pictures, and the answer is a
/// vision model rather than a longer regex.
fn info(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "pdf.info", span)?;
    super::journaled_read(interp, "pdf.info", span, move |_| {
        let bytes = match load(&path) {
            Ok(bytes) => bytes,
            Err(reason) => return err(reason),
        };
        let (page_count, encrypted) = match without_panicking(|| {
            let mut document = lopdf::Document::load_mem(&bytes)?;
            let encrypted = document.is_encrypted();
            if encrypted {
                // Most "encrypted" PDFs carry an empty user password and are
                // readable; the flag says how the file is built, not whether
                // it can be opened.
                let _ = document.decrypt("");
            }
            Ok::<_, lopdf::Error>((document.get_pages().len(), encrypted))
        }) {
            Ok(Ok(facts)) => facts,
            Ok(Err(e)) => return err(describe_parse(&path, &e)),
            Err(()) => return err(panicked(&path, "pdf.info")),
        };

        // Asked for, not inferred: a document can be one blank page, and the
        // only way to know whether there is text to read is to look.
        let has_text_layer = matches!(read_pages(&path, "pdf.info"), Ok(pages) if pages.iter().any(|p| !p.trim().is_empty()));

        let mut fields = HashMap::new();
        fields.insert("page_count".to_string(), Value::Int(page_count as i64));
        fields.insert("encrypted".to_string(), Value::Bool(encrypted));
        fields.insert("has_text_layer".to_string(), Value::Bool(has_text_layer));
        ok(Value::Dict(Rc::new(RefCell::new(fields))).with_label(Label::UNVERIFIED))
    })
}

/// The text of every page, or the reason there is none.
///
/// The page count comes from the document catalogue rather than from however
/// far extraction happened to get, so a page that will not parse is reported
/// instead of shortening the document.
fn read_pages(path: &str, func: &str) -> Result<Vec<String>, String> {
    let bytes = load(path)?;

    let extracted = without_panicking(|| {
        let mut document = lopdf::Document::load_mem(&bytes)?;
        if document.is_encrypted() {
            document.decrypt("").map_err(|_| {
                lopdf::Error::Decryption(lopdf::encryption::DecryptionError::IncorrectPassword)
            })?;
        }

        let count = document.get_pages().len() as u32;
        let mut pages = Vec::with_capacity(count as usize);
        for number in 1..=count {
            let mut page = String::new();
            {
                let mut output = pdf_extract::PlainTextOutput::new(&mut page);
                if pdf_extract::output_doc_page(&document, &mut output, number).is_err() {
                    return Ok(Err(number));
                }
            }
            pages.push(page);
        }
        Ok::<_, lopdf::Error>(Ok(pages))
    });

    let pages = match extracted {
        Ok(Ok(Ok(pages))) => pages,
        Ok(Ok(Err(number))) => {
            return Err(format!("{path}: page {number} could not be read"));
        }
        Ok(Err(e)) => return Err(describe_parse(path, &e)),
        Err(()) => return Err(panicked(path, func)),
    };

    if pages.is_empty() {
        return Err(format!("{path} has no pages"));
    }
    if pages.iter().all(|page| page.trim().is_empty()) {
        return Err(format!(
            "{path} has no text layer: its pages are images, not text"
        ));
    }
    Ok(pages)
}

/// Read the file, refusing one too large to be worth parsing.
fn load(path: &str) -> Result<Vec<u8>, String> {
    match std::fs::metadata(path) {
        Ok(meta) if meta.len() as usize > MAX_BYTES => {
            return Err(format!(
                "{path} is {} and the limit is {}",
                human_size(meta.len() as usize),
                human_size(MAX_BYTES)
            ));
        }
        Ok(_) => {}
        Err(e) => return Err(describe_io(path, &e)),
    }
    match std::fs::read(path) {
        Ok(bytes) if bytes.starts_with(b"%PDF") => Ok(bytes),
        Ok(bytes) => Err(format!(
            "{path} is not a PDF (its first bytes are {})",
            first_bytes(&bytes)
        )),
        Err(e) => Err(describe_io(path, &e)),
    }
}

/// Run a parse without letting a panic inside it end the run.
///
/// The crates underneath are ordinary Rust parsers with ordinary `unwrap`s in
/// them, and the input is a file the program did not write. A panic is a
/// failed parse, so it is reported as one — but the default hook would still
/// print a backtrace, which for a caught panic is noise on someone's terminal.
/// The hook is replaced once, for the whole process, and only stays quiet on
/// the thread that is currently inside this function: a `parallel for` worker
/// panicking elsewhere still prints.
fn without_panicking<T>(parse: impl FnOnce() -> T) -> Result<T, ()> {
    thread_local! {
        static QUIET: Cell<bool> = const { Cell::new(false) };
    }
    static HOOK: OnceLock<()> = OnceLock::new();

    HOOK.get_or_init(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if QUIET.with(|quiet| quiet.get()) {
                return;
            }
            previous(info);
        }));
    });

    QUIET.with(|quiet| quiet.set(true));
    let outcome = std::panic::catch_unwind(AssertUnwindSafe(parse));
    QUIET.with(|quiet| quiet.set(false));
    outcome.map_err(|_| ())
}

/// A parse failure the caller can act on, rather than a library's own wording.
fn describe_parse(path: &str, e: &lopdf::Error) -> String {
    match e {
        lopdf::Error::Decryption(_) => {
            format!("{path} is encrypted and needs a password to read")
        }
        other => format!("{path} could not be read as a PDF: {other}"),
    }
}

fn panicked(path: &str, func: &str) -> String {
    format!("{path} is malformed: {func}() could not parse it")
}

fn first_bytes(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "empty".to_string();
    }
    let preview: Vec<String> = bytes.iter().take(4).map(|b| format!("{b:02x}")).collect();
    preview.join(" ")
}

fn human_size(bytes: usize) -> String {
    const KB: f64 = 1024.0;
    const MB: f64 = KB * 1024.0;
    let n = bytes as f64;
    if n >= MB {
        format!("{:.1} MB", n / MB)
    } else if n >= KB {
        format!("{:.1} KB", n / KB)
    } else {
        format!("{bytes} bytes")
    }
}
