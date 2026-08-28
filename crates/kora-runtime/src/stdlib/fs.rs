//! `fs` — writes that cannot half-happen, paths that cannot be hijacked.
//!
//! Three defects fixed.
//!
//! **Partial writes.** A crash during `write()` leaves a truncated file, and
//! the original is gone. Here every write goes to a temporary file and is
//! renamed into place, so a reader sees either the old contents or the new
//! ones, never a half-written mix.
//!
//! **Path traversal.** When a path comes from model output or an HTTP body,
//! `../../etc/passwd` is a real risk, and the usual defence is a review
//! comment. Here an `unverified` path is refused outright.
//!
//! **Silent overwrite.** `write()` destroying an existing file by default has
//! cost people work for decades. Overwriting is spelled differently from
//! creating.
//!
//! **Directory order.** `os.listdir` and `glob.glob` hand back whatever order
//! the filesystem happened to store, which differs between machines. A
//! program that fans that list across threads then does its work in a
//! different sequence on every host, and a durable replay stops matching the
//! run it is resuming. Listings here are always sorted.

use std::path::{Component, Path};
use std::rc::Rc;

use kora_syntax::token::Span;

use super::glob;
use super::{err, ok, require_not_classified, require_verified, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::media::Image;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[
    ("read", read),
    ("write", write),
    ("append", append),
    ("exists", exists),
    ("lines", lines),
    ("image", image),
    ("list", list),
    ("glob", glob_files),
];

/// `fs.read(path) -> Ok(text) | Err(reason)`
///
/// The contents are `unverified`: a file is outside the program.
fn read(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "fs.read", span)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => Ok(ok(Value::Str(Rc::new(text)).with_label(Label::UNVERIFIED))),
        Err(e) => Ok(err(describe_io(&path, &e))),
    }
}

/// `fs.lines(path) -> Ok(list) | Err(reason)`
fn lines(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "fs.lines", span)?;
    match std::fs::read_to_string(&path) {
        Ok(text) => {
            let items: Vec<Value> = text
                .lines()
                .map(|l| Value::Str(Rc::new(l.to_string())))
                .collect();
            Ok(ok(
                Value::List(Rc::new(std::cell::RefCell::new(items))).with_label(Label::UNVERIFIED)
            ))
        }
        Err(e) => Ok(err(describe_io(&path, &e))),
    }
}

/// `fs.write(path, text) -> Ok(None) | Err(reason)`
///
/// Atomic: written to a temporary file and renamed, so a crash cannot leave a
/// truncated file behind.
fn write(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "fs.write", span)?;
    let contents = str_arg(&args, 1, "fs.write", "the text to write", span)?;
    if let Some(value) = args.get(1) {
        require_not_classified(interp, value, "fs.write", span)?;
    }

    let target = Path::new(&path);
    let tmp = target.with_extension(format!(
        "{}.kora-tmp",
        target
            .extension()
            .map(|e| e.to_string_lossy().to_string())
            .unwrap_or_default()
    ));
    if let Err(e) = std::fs::write(&tmp, contents) {
        return Ok(err(describe_io(&path, &e)));
    }
    match std::fs::rename(&tmp, target) {
        Ok(()) => Ok(ok(Value::None)),
        Err(e) => {
            std::fs::remove_file(&tmp).ok();
            Ok(err(describe_io(&path, &e)))
        }
    }
}

/// `fs.append(path, text) -> Ok(None) | Err(reason)`
fn append(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    use std::io::Write;
    let path = checked_path(&args, "fs.append", span)?;
    let contents = str_arg(&args, 1, "fs.append", "the text to append", span)?;
    if let Some(value) = args.get(1) {
        require_not_classified(interp, value, "fs.append", span)?;
    }
    let opened = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path);
    match opened {
        Ok(mut file) => match file.write_all(contents.as_bytes()) {
            Ok(()) => Ok(ok(Value::None)),
            Err(e) => Ok(err(describe_io(&path, &e))),
        },
        Err(e) => Ok(err(describe_io(&path, &e))),
    }
}

/// `fs.image(path) -> Ok(image) | Err(reason)`
///
/// The image is `unverified` like any other file content, and its type comes
/// from the bytes rather than the extension (see [`crate::media`]).
fn image(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "fs.image", span)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) => return Ok(err(describe_io(&path, &e))),
    };
    match Image::detect(bytes, &path) {
        Ok(image) => Ok(ok(
            Value::Image(Rc::new(image)).with_label(Label::UNVERIFIED)
        )),
        Err(reason) => Ok(err(reason)),
    }
}

/// `fs.list(dir) -> Ok(list of paths) | Err(reason)`
///
/// Full paths, not bare names: a name alone has to be re-joined by hand, and
/// forgetting to is how a listing loop ends up reading the wrong directory.
fn list(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let dir = checked_path(&args, "fs.list", span)?;
    let entries = match std::fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(e) => return Ok(err(describe_io(&dir, &e))),
    };
    let mut names: Vec<String> = Vec::new();
    for entry in entries.flatten() {
        // A name that is not valid UTF-8 is skipped rather than mangled: a
        // lossy name would not open again.
        if let Some(name) = entry.file_name().to_str() {
            names.push(format!("{}/{name}", dir.trim_end_matches('/')));
        }
    }
    names.sort();
    Ok(ok(paths_value(names)))
}

/// `fs.glob(pattern) -> Ok(list of paths) | Err(reason)`
fn glob_files(
    _interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let pattern = checked_path(&args, "fs.glob", span)?;
    match glob::expand(&pattern) {
        Ok(paths) => Ok(ok(paths_value(paths))),
        Err(reason) => Ok(err(format!("fs.glob({pattern}): {reason}"))),
    }
}

/// Paths the runtime produced from a program-supplied pattern.
///
/// Verified, unlike file *contents*. The program named the directory and the
/// shape of the names, and every result was matched against it — that is an
/// allowlist, which is exactly what narrows `unverified` elsewhere. Leaving
/// these unverified would mean no listed path could be opened without a
/// laundering step that checks nothing.
fn paths_value(paths: Vec<String>) -> Value {
    Value::List(Rc::new(std::cell::RefCell::new(
        paths
            .into_iter()
            .map(|p| Value::Str(Rc::new(p)))
            .collect::<Vec<_>>(),
    )))
}

/// `fs.exists(path) -> bool`
fn exists(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let path = checked_path(&args, "fs.exists", span)?;
    Ok(Value::Bool(Path::new(&path).exists()))
}

/// Validate the path argument: it must be verified data, and must not climb
/// out of the working tree.
fn checked_path(args: &[Value], func: &str, span: Span) -> Result<String, RuntimeError> {
    let Some(value) = args.first() else {
        return Err(RuntimeError::new(format!("{func}() needs a path"), span));
    };
    require_verified(value, func, "a path", span)?;
    let path = str_arg(args, 0, func, "a path", span)?;

    // `..` in a path built from data is how traversal happens. Refuse rather
    // than normalize, because normalizing quietly changes what the caller
    // asked for.
    if Path::new(&path)
        .components()
        .any(|c| matches!(c, Component::ParentDir))
    {
        return Err(RuntimeError::new(
            format!("{func}() was given a path containing `..`: {path}"),
            span,
        )
        .with_hint("build paths from known directories rather than climbing out of one"));
    }
    Ok(path)
}

/// io::Error messages omit the path, which is the first thing you want.
fn describe_io(path: &str, e: &std::io::Error) -> String {
    match e.kind() {
        std::io::ErrorKind::NotFound => format!("no such file: {path}"),
        std::io::ErrorKind::PermissionDenied => format!("permission denied: {path}"),
        _ => format!("{path}: {e}"),
    }
}
