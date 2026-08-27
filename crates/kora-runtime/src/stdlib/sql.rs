//! `sql` — a query you cannot build by concatenation.
//!
//! The defect fixed is the oldest one on the list. SQL injection persists
//! because building a query with string interpolation is *easier* than the
//! safe path: `f"select * from users where id = {user_id}"` is shorter than
//! binding a parameter, and it works right up until the input contains a
//! quote.
//!
//! Here the query text must be data the program itself produced. A value that
//! came from outside — an HTTP body, a file, a model answer — cannot become
//! query text at all. It can only be *bound* as a parameter, where the driver
//! keeps it separate from the statement. The safe path is the only path.
//!
//! Backed by SQLite, so a program has a working database with no server.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::token::Span;

use super::{err, ok, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("query", query), ("execute", execute)];

/// `sql.query(db_path, "select ... where id = ?", [id]) -> Ok(rows) | Err(reason)`
fn query(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (path, statement, params) = prepare(interp, &args, "sql.query", span)?;
    let connection = match rusqlite::Connection::open(&path) {
        Ok(c) => c,
        Err(e) => return Ok(err(format!("could not open {path}: {e}"))),
    };
    let mut prepared = match connection.prepare(&statement) {
        Ok(p) => p,
        Err(e) => return Ok(err(describe(&statement, &e))),
    };
    let column_names: Vec<String> = prepared
        .column_names()
        .into_iter()
        .map(|s| s.to_string())
        .collect();

    let bound = rusqlite::params_from_iter(params.iter());
    let mut rows = match prepared.query(bound) {
        Ok(r) => r,
        Err(e) => return Ok(err(describe(&statement, &e))),
    };

    let mut out = Vec::new();
    loop {
        match rows.next() {
            Ok(Some(row)) => {
                let mut entries = HashMap::new();
                for (index, name) in column_names.iter().enumerate() {
                    entries.insert(name.clone(), column_value(row, index));
                }
                out.push(Value::Dict(Rc::new(RefCell::new(entries))));
            }
            Ok(None) => break,
            Err(e) => return Ok(err(describe(&statement, &e))),
        }
    }
    // Rows are data from outside the program.
    Ok(ok(
        Value::List(Rc::new(RefCell::new(out))).with_label(Label::UNVERIFIED)
    ))
}

/// `sql.execute(db_path, "insert ...", [params]) -> Ok(count) | Err(reason)`
fn execute(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let (path, statement, params) = prepare(interp, &args, "sql.execute", span)?;
    let connection = match rusqlite::Connection::open(&path) {
        Ok(c) => c,
        Err(e) => return Ok(err(format!("could not open {path}: {e}"))),
    };
    let bound = rusqlite::params_from_iter(params.iter());
    match connection.execute(&statement, bound) {
        Ok(count) => Ok(ok(Value::Int(count as i64))),
        Err(e) => Ok(err(describe(&statement, &e))),
    }
}

/// Validate the arguments common to both entry points.
fn prepare(
    interp: &Interpreter,
    args: &[Value],
    func: &str,
    span: Span,
) -> Result<(String, String, Vec<SqlParam>), RuntimeError> {
    let path = str_arg(args, 0, func, "a database path", span)?;

    let Some(statement_value) = args.get(1) else {
        return Err(RuntimeError::new(
            format!("{func}() needs a statement"),
            span,
        ));
    };
    // The heart of it: query text must be something the program wrote. Data
    // from outside can be bound as a parameter, never spliced into the query.
    if statement_value.label().is_unverified() {
        return Err(RuntimeError::new(
            format!("{func}() was given a statement built from outside data"),
            span,
        )
        .with_hint(
            "pass the value as a parameter instead: sql.query(db, \"... where id = ?\", [id])",
        ));
    }
    let statement = str_arg(args, 1, func, "a statement", span)?;

    let mut params = Vec::new();
    if let Some(list) = args.get(2) {
        // Parameters may be unverified: binding is exactly what makes them
        // safe, so this is the path the language wants people to take.
        let Value::List(items) = list.unlabeled() else {
            return Err(RuntimeError::new(
                format!(
                    "{func}() parameters must be a list, got {}",
                    list.type_name()
                ),
                span,
            ));
        };
        for item in items.borrow().iter() {
            // A secret still must not leave without a release, even bound.
            if interp.deep_label(item).is_classified() && !interp.declassified_for_sink("database")
            {
                return Err(
                    RuntimeError::new(format!("{func}() was given classified data"), span)
                        .with_hint("wrap it in `declassify <value> for database:`"),
                );
            }
            params.push(match item.unlabeled() {
                Value::Int(v) => SqlParam::Int(*v),
                Value::Float(v) => SqlParam::Real(*v),
                Value::Bool(b) => SqlParam::Int(*b as i64),
                Value::None => SqlParam::Null,
                other => SqlParam::Text(other.to_string()),
            });
        }
    }
    Ok((path, statement, params))
}

/// An owned parameter value, so it outlives the borrow of the argument list.
enum SqlParam {
    Int(i64),
    Real(f64),
    Text(String),
    Null,
}

impl rusqlite::ToSql for SqlParam {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        use rusqlite::types::{ToSqlOutput, Value as SqlValue};
        Ok(match self {
            SqlParam::Int(v) => ToSqlOutput::Owned(SqlValue::Integer(*v)),
            SqlParam::Real(v) => ToSqlOutput::Owned(SqlValue::Real(*v)),
            SqlParam::Text(v) => ToSqlOutput::Owned(SqlValue::Text(v.clone())),
            SqlParam::Null => ToSqlOutput::Owned(SqlValue::Null),
        })
    }
}

fn column_value(row: &rusqlite::Row<'_>, index: usize) -> Value {
    use rusqlite::types::ValueRef;
    match row.get_ref(index) {
        Ok(ValueRef::Null) | Err(_) => Value::None,
        Ok(ValueRef::Integer(v)) => Value::Int(v),
        Ok(ValueRef::Real(v)) => Value::Float(v),
        Ok(ValueRef::Text(bytes)) => {
            Value::Str(Rc::new(String::from_utf8_lossy(bytes).to_string()))
        }
        Ok(ValueRef::Blob(bytes)) => Value::Str(Rc::new(format!("<{} bytes>", bytes.len()))),
    }
}

/// SQLite errors omit the statement, which is the first thing you want.
fn describe(statement: &str, e: &rusqlite::Error) -> String {
    let short: String = statement.chars().take(120).collect();
    format!("{e} (in: {short})")
}
