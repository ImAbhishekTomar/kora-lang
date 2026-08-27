//! `csv` — typed rows, and no silent guessing.
//!
//! Two defects fixed, pulling in opposite directions.
//!
//! **Everything is a string** (Python's `csv`): the caller converts by hand,
//! forgets a column, and finds out in production.
//!
//! **Types are guessed** (pandas): a zip code column of `01234` becomes the
//! integer `1234`, a phone number becomes a float, and an ID column silently
//! changes type when a later chunk contains a letter. This has cost people
//! real money and cannot be fixed downstream, because by the time you see the
//! value the leading zero is gone.
//!
//! Kora does neither. You declare the shape; a row that does not match is an
//! error naming the row and column. Nothing is inferred.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::ast::TypeExpr;
use kora_syntax::token::Span;

use super::{err, ok, require_not_classified, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("parse", parse), ("rows", rows), ("write", write)];

/// `csv.parse(text, RowType) -> Ok(list) | Err(reason)`
///
/// Every field is converted according to the declared type. A mismatch names
/// the row and the column instead of failing somewhere downstream.
fn parse(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "csv.parse", "the text to parse", span)?;
    let label = args.first().map(|v| v.label()).unwrap_or_default();

    let type_name = match args.get(1).map(|v| v.unlabeled()) {
        Some(Value::TypeRef { name }) => name.to_string(),
        Some(other) => {
            return Err(RuntimeError::new(
                format!(
                    "csv.parse() expects a declared type as its second argument, got {}",
                    other.type_name()
                ),
                span,
            )
            .with_hint("declare the row shape with `type Row:` and pass `Row`"))
        }
        None => {
            return Err(RuntimeError::new("csv.parse() needs a row type", span)
                .with_hint("example: `csv.parse(text, Expense)`"))
        }
    };

    let fields = match interp.declared_fields(&type_name) {
        Some(fields) => fields,
        None => {
            return Err(RuntimeError::new(
                format!("`{type_name}` is not a declared type"),
                span,
            ))
        }
    };

    let table = match read_table(&text) {
        Ok(t) => t,
        Err(message) => return Ok(err(message)),
    };
    let Some(header) = table.first() else {
        return Ok(err("the file is empty: no header row".to_string()));
    };

    // Map declared fields onto columns by name, so column order in the file
    // does not have to match the type.
    let mut indices = Vec::new();
    for (name, _) in &fields {
        match header.iter().position(|h| h == name) {
            Some(index) => indices.push(index),
            None => {
                return Ok(err(format!(
                    "column `{name}` is missing (the file has: {})",
                    header.join(", ")
                )))
            }
        }
    }

    let mut out = Vec::new();
    for (row_number, row) in table.iter().enumerate().skip(1) {
        // Ragged rows are an error, not a silent None or a shifted value.
        if row.len() != header.len() {
            return Ok(err(format!(
                "row {}: has {} field(s) but the header has {}",
                row_number + 1,
                row.len(),
                header.len()
            )));
        }
        let mut entries = HashMap::new();
        for ((name, ty), index) in fields.iter().zip(&indices) {
            let raw = &row[*index];
            match convert(raw, ty) {
                Ok(value) => {
                    entries.insert(name.clone(), value);
                }
                Err(expected) => {
                    return Ok(err(format!(
                        "row {}, column `{name}`: expected {expected}, got `{raw}`",
                        row_number + 1
                    )))
                }
            }
        }
        out.push(Value::Object {
            type_name: Rc::new(type_name.clone()),
            fields: Rc::new(RefCell::new(entries)),
        });
    }

    // The data came from outside, so it stays unverified until narrowed.
    Ok(ok(
        Value::List(Rc::new(RefCell::new(out))).with_label(label.join(Label::UNVERIFIED))
    ))
}

/// `csv.rows(text) -> Ok(list of dict) | Err(reason)`
///
/// Untyped access for exploring a file. Every value stays a string: no
/// guessing means no lost leading zeros.
fn rows(_interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let text = str_arg(&args, 0, "csv.rows", "the text to parse", span)?;
    let label = args.first().map(|v| v.label()).unwrap_or_default();
    let table = match read_table(&text) {
        Ok(t) => t,
        Err(message) => return Ok(err(message)),
    };
    let Some(header) = table.first().cloned() else {
        return Ok(err("the file is empty: no header row".to_string()));
    };
    let mut out = Vec::new();
    for (row_number, row) in table.iter().enumerate().skip(1) {
        if row.len() != header.len() {
            return Ok(err(format!(
                "row {}: has {} field(s) but the header has {}",
                row_number + 1,
                row.len(),
                header.len()
            )));
        }
        let entries: HashMap<String, Value> = header
            .iter()
            .zip(row)
            .map(|(h, v)| (h.clone(), Value::Str(Rc::new(v.clone()))))
            .collect();
        out.push(Value::Dict(Rc::new(RefCell::new(entries))));
    }
    Ok(ok(
        Value::List(Rc::new(RefCell::new(out))).with_label(label.join(Label::UNVERIFIED))
    ))
}

/// `csv.write(rows) -> Ok(text) | Err(reason)`
fn write(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let Some(value) = args.first() else {
        return Err(RuntimeError::new("csv.write() needs a list of rows", span));
    };
    require_not_classified(interp, value, "csv.write", span)?;

    let Value::List(items) = value.unlabeled() else {
        return Err(RuntimeError::new(
            format!("csv.write() expects a list, got {}", value.type_name()),
            span,
        ));
    };
    let items = items.borrow();
    if items.is_empty() {
        return Ok(ok(Value::Str(Rc::new(String::new()))));
    }

    // Take the column order from the first row's declared type when there is
    // one, so output columns are stable rather than hash-ordered.
    let (header, get): (Vec<String>, bool) = match items[0].unlabeled() {
        Value::Object { type_name, fields } => match interp.declared_fields(type_name.as_str()) {
            Some(declared) => (declared.into_iter().map(|(n, _)| n).collect(), true),
            None => {
                let mut names: Vec<String> = fields.borrow().keys().cloned().collect();
                names.sort();
                (names, true)
            }
        },
        Value::Dict(map) => {
            let mut names: Vec<String> = map.borrow().keys().cloned().collect();
            names.sort();
            (names, false)
        }
        other => {
            return Ok(err(format!(
                "csv.write() rows must be objects or dicts, got {}",
                other.type_name()
            )))
        }
    };

    let mut out = String::new();
    out.push_str(&header.join(","));
    out.push('\n');
    for item in items.iter() {
        let cells: Vec<String> = header
            .iter()
            .map(|name| {
                let found = match item.unlabeled() {
                    Value::Object { fields, .. } if get => fields.borrow().get(name).cloned(),
                    Value::Dict(map) => map.borrow().get(name).cloned(),
                    _ => None,
                };
                escape(&found.map(|v| v.to_string()).unwrap_or_default())
            })
            .collect();
        out.push_str(&cells.join(","));
        out.push('\n');
    }
    Ok(ok(Value::Str(Rc::new(out))))
}

/// Convert one field, or report what was expected.
fn convert(raw: &str, ty: &TypeExpr) -> Result<Value, String> {
    let name = match ty {
        TypeExpr::Name(n) => n.as_str(),
        TypeExpr::Generic(n, _) => n.as_str(),
    };
    match name {
        // Strings are taken exactly as they appear: this is what preserves a
        // zip code's leading zero.
        "str" => Ok(Value::Str(Rc::new(raw.to_string()))),
        "int" => raw
            .trim()
            .parse::<i64>()
            .map(Value::Int)
            .map_err(|_| "an integer".to_string()),
        "float" => raw
            .trim()
            .parse::<f64>()
            .map(Value::Float)
            .map_err(|_| "a number".to_string()),
        "bool" => match raw.trim().to_ascii_lowercase().as_str() {
            "true" | "yes" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "0" => Ok(Value::Bool(false)),
            _ => Err("a boolean (true/false, yes/no, 1/0)".to_string()),
        },
        other => Err(format!("a {other}, which csv cannot read")),
    }
}

/// Minimal RFC 4180 reader: quoted fields, doubled quotes, embedded newlines.
/// A BOM is stripped, because a leading BOM silently breaks the first column
/// name and the resulting error never mentions it.
fn read_table(text: &str) -> Result<Vec<Vec<String>>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows = Vec::new();
    let mut row = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();

    while let Some(c) = chars.next() {
        if in_quotes {
            match c {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        field.push('"');
                    } else {
                        in_quotes = false;
                    }
                }
                other => field.push(other),
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => row.push(std::mem::take(&mut field)),
            '\n' => {
                row.push(std::mem::take(&mut field));
                rows.push(std::mem::take(&mut row));
            }
            '\r' => {}
            other => field.push(other),
        }
    }
    if in_quotes {
        return Err("unterminated quoted field".to_string());
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        rows.push(row);
    }
    Ok(rows)
}

fn escape(value: &str) -> String {
    if value.contains(',') || value.contains('"') || value.contains('\n') {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_quoted_fields_and_embedded_commas() {
        let table = read_table("a,b\n\"x,1\",\"say \"\"hi\"\"\"\n").unwrap();
        assert_eq!(table[1], vec!["x,1", "say \"hi\""]);
    }

    #[test]
    fn strips_a_byte_order_mark() {
        // A BOM silently corrupts the first column name, and the resulting
        // error never mentions it.
        let table = read_table("\u{feff}name,age\nada,36\n").unwrap();
        assert_eq!(table[0][0], "name");
    }

    #[test]
    fn reports_an_unterminated_quote() {
        assert!(read_table("a,b\n\"oops\n").is_err());
    }

    #[test]
    fn strings_keep_their_leading_zeros() {
        // The pandas bug: a zip code column becomes an integer and 01234
        // turns into 1234.
        let value = convert("01234", &TypeExpr::Name("str".into())).unwrap();
        assert_eq!(value.to_string(), "01234");
    }
}
