//! `time` — no naive timestamps, and clocks that survive replay.
//!
//! Two defects fixed here.
//!
//! The famous one: `datetime.now()` in Python returns a *naive* value with no
//! zone, and everything downstream then guesses. Here an instant is always an
//! absolute point in time (UTC seconds), and formatting is the only place a
//! zone is applied. There is no naive type to misuse.
//!
//! The one specific to this language: a clock is nondeterministic, so in a
//! durable run it is an **effect**. If `now()` returned the real time during
//! a replay, a resumed program would take different branches than the run it
//! is supposedly continuing. So the first call records its answer, and every
//! replay sees that same answer.

use std::rc::Rc;

use kora_syntax::token::Span;

use super::{err, ok};
use crate::interp::{Interpreter, RuntimeError};
use crate::value::Value;

pub const EXPORTS: super::Exports = &[
    ("now", now),
    ("format", format_instant),
    ("elapsed", elapsed),
];

/// `time.now() -> int` — seconds since the Unix epoch, UTC.
///
/// Journaled: a replayed run sees the instant the original run saw.
fn now(interp: &mut Interpreter, _args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let live = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    let seconds = interp.journal_scalar("time.now", span, live)?;
    Ok(Value::Int(seconds))
}

/// `time.format(seconds, "iso") -> Ok(text) | Err(reason)`
///
/// Only absolute instants can be formatted, so there is no way to print a
/// timestamp whose zone nobody knows.
fn format_instant(
    _interp: &mut Interpreter,
    args: Vec<Value>,
    span: Span,
) -> Result<Value, RuntimeError> {
    let seconds = match args.first().map(|v| v.unlabeled()) {
        Some(Value::Int(v)) => *v,
        Some(other) => {
            return Err(RuntimeError::new(
                format!(
                    "time.format() expects seconds as an int, got {}",
                    other.type_name()
                ),
                span,
            ))
        }
        None => return Err(RuntimeError::new("time.format() needs an instant", span)),
    };
    let style = match args.get(1).map(|v| v.unlabeled()) {
        Some(Value::Str(s)) => s.to_string(),
        _ => "iso".to_string(),
    };
    match style.as_str() {
        "iso" => Ok(ok(Value::Str(Rc::new(iso8601(seconds))))),
        "date" => Ok(ok(Value::Str(Rc::new(
            iso8601(seconds).split('T').next().unwrap_or("").to_string(),
        )))),
        "unix" => Ok(ok(Value::Str(Rc::new(seconds.to_string())))),
        other => Ok(err(format!(
            "unknown time format `{other}` (use \"iso\", \"date\", or \"unix\")"
        ))),
    }
}

/// `time.elapsed(since) -> int` — whole seconds between two instants.
fn elapsed(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    let since = match args.first().map(|v| v.unlabeled()) {
        Some(Value::Int(v)) => *v,
        _ => {
            return Err(RuntimeError::new(
                "time.elapsed() needs a starting instant",
                span,
            ))
        }
    };
    let live = || {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    };
    let current = interp.journal_scalar("time.elapsed", span, live)?;
    Ok(Value::Int((current - since).max(0)))
}

/// Civil date/time from a Unix timestamp, without pulling in a date library.
///
/// Uses the standard days-from-civil algorithm, valid across the proleptic
/// Gregorian calendar.
fn iso8601(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let secs_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        secs_of_day / 3600,
        (secs_of_day % 3600) / 60,
        secs_of_day % 60
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn iso_formatting_matches_known_instants() {
        assert_eq!(iso8601(0), "1970-01-01T00:00:00Z");
        assert_eq!(iso8601(1_000_000_000), "2001-09-09T01:46:40Z");
        // A leap day, which is where naive date maths usually breaks.
        assert_eq!(iso8601(1_709_164_800), "2024-02-29T00:00:00Z");
    }

    #[test]
    fn civil_conversion_handles_leap_years() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(59), (1970, 3, 1));
        // 2000 was a leap year; 1900 was not.
        let y2k = 10_957; // 2000-01-01
        assert_eq!(civil_from_days(y2k), (2000, 1, 1));
        assert_eq!(civil_from_days(y2k + 59), (2000, 2, 29));
    }
}
