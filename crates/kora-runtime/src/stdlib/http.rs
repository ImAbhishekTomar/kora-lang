//! `http` — timeouts you cannot forget, failures you cannot ignore.
//!
//! Four defects fixed.
//!
//! **No default timeout.** `requests.get(url)` waits forever. It is the single
//! most common way a Python service hangs, and the fix — passing `timeout=`
//! every time — is something everyone knows and nobody does consistently.
//! Here the timeout always exists; you can change it, not omit it.
//!
//! **Failure that looks like success.** `requests.get()` returns a response
//! object for a 500. Unless you remember `raise_for_status()`, a failed call
//! flows onward as data. Here a non-2xx status is `Err`.
//!
//! **Retries as an afterthought.** Everyone reaches for a second library.
//! Retry with backoff is built in, and only for methods where retrying is
//! safe.
//!
//! **SSRF.** When a URL can come from model output or a fetched document,
//! `http.get(url)` reaches internal services. Here a URL must be verified
//! data, and private address ranges are refused unless the project opts in.
//!
//! Responses are `unverified`: a response body is the definition of data from
//! outside the program.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use kora_syntax::token::Span;

use super::{err, ok, require_verified, str_arg};
use crate::interp::{Interpreter, RuntimeError};
use crate::label::Label;
use crate::value::Value;

pub const EXPORTS: super::Exports = &[("get", get), ("post", post)];

const DEFAULT_RETRIES: u32 = 2;

/// `http.get(url) -> Ok(response) | Err(reason)`
fn get(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    request(interp, args, "GET", span)
}

/// `http.post(url, body) -> Ok(response) | Err(reason)`
fn post(interp: &mut Interpreter, args: Vec<Value>, span: Span) -> Result<Value, RuntimeError> {
    request(interp, args, "POST", span)
}

fn request(
    interp: &mut Interpreter,
    args: Vec<Value>,
    method: &str,
    span: Span,
) -> Result<Value, RuntimeError> {
    let Some(url_value) = args.first() else {
        return Err(RuntimeError::new(
            format!("http.{}() needs a url", method.to_lowercase()),
            span,
        ));
    };
    let func = format!("http.{}", method.to_lowercase());
    // A URL assembled from a fetched document or a model answer is how
    // requests end up pointed at internal services.
    require_verified(url_value, &func, "a url", span)?;
    let url = str_arg(&args, 0, &func, "a url", span)?;

    if let Err(reason) = check_destination(&url, interp.allow_private_hosts) {
        return Ok(err(reason));
    }

    // Sending is an export: a secret in the body or the URL must have been
    // released for the host it is going to.
    let host = host_of(&url).unwrap_or_default();
    for value in &args {
        if interp.deep_label(value).is_classified() && !interp.declassified_for_sink(&host) {
            return Err(RuntimeError::new(
                format!("{func}() was given classified data for `{host}`"),
                span,
            )
            .with_hint(format!(
                "wrap it in `declassify <value> for {host}:` and allow that sink in kora.toml"
            )));
        }
    }

    let body = match args.get(1).map(|v| v.unlabeled()) {
        Some(Value::Str(s)) => Some(s.to_string()),
        Some(Value::None) | None => None,
        Some(other) => {
            return Err(RuntimeError::new(
                format!("{func}() body must be a string, got {}", other.type_name()),
                span,
            ))
        }
    };

    // A network call is nondeterministic, so a durable run must replay the
    // answer it already got rather than asking again.
    let site = format!("{}:{}#http", interp.program_name, span.line);
    if let Some(recorded) = interp.journal_lookup(&site, span)? {
        return Ok(response_from_json(&recorded));
    }

    let outcome = perform(method, &url, body.as_deref(), interp.http_timeout_secs);
    let encoded = match &outcome {
        Ok(response) => serde_json::json!({
            "status": response.status,
            "body": response.body,
        })
        .to_string(),
        Err(message) => serde_json::json!({ "error": message }).to_string(),
    };
    interp.journal_record(&site, "http", &encoded, span)?;

    Ok(match outcome {
        Ok(response) => ok(response.into_value()),
        Err(message) => err(message),
    })
}

struct Response {
    status: u16,
    body: String,
}

impl Response {
    fn into_value(self) -> Value {
        let mut fields = HashMap::new();
        fields.insert("status".to_string(), Value::Int(self.status as i64));
        fields.insert("body".to_string(), Value::Str(Rc::new(self.body)));
        // The body came from outside: it cannot become a path, a query, or a
        // command until something narrows it.
        Value::Dict(Rc::new(RefCell::new(fields))).with_label(Label::UNVERIFIED)
    }
}

fn response_from_json(encoded: &str) -> Value {
    let parsed: serde_json::Value =
        serde_json::from_str(encoded).unwrap_or(serde_json::Value::Null);
    if let Some(message) = parsed.get("error").and_then(|v| v.as_str()) {
        return err(message.to_string());
    }
    ok(Response {
        status: parsed.get("status").and_then(|v| v.as_u64()).unwrap_or(0) as u16,
        body: parsed
            .get("body")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
    }
    .into_value())
}

/// Perform the call, retrying only where a retry is safe.
fn perform(
    method: &str,
    url: &str,
    body: Option<&str>,
    timeout_secs: u64,
) -> Result<Response, String> {
    let agent = ureq::AgentBuilder::new()
        .timeout(std::time::Duration::from_secs(timeout_secs))
        .build();

    // GET is idempotent, so retrying is safe. POST is not: a retried payment
    // is worse than a failed one, so it is attempted once.
    let attempts = if method == "GET" {
        DEFAULT_RETRIES + 1
    } else {
        1
    };
    let mut last = String::new();

    for attempt in 0..attempts {
        if attempt > 0 {
            // Exponential backoff, so a struggling server is not hammered.
            std::thread::sleep(std::time::Duration::from_millis(200 << attempt));
        }
        let request = match method {
            "GET" => agent.get(url),
            _ => agent.post(url),
        };
        let sent = match body {
            Some(text) => request
                .set("Content-Type", "application/json")
                .send_string(text),
            None => request.call(),
        };
        match sent {
            Ok(response) => {
                let status = response.status();
                let text = response.into_string().unwrap_or_default();
                return Ok(Response { status, body: text });
            }
            // A non-2xx is a failure, not a response object that flows onward
            // until someone remembers to check it.
            Err(ureq::Error::Status(code, response)) => {
                let text = response.into_string().unwrap_or_default();
                let snippet = text.chars().take(200).collect::<String>();
                last = format!("{url} returned HTTP {code}: {snippet}");
                // Retrying a 4xx will not help; a 5xx might.
                if code < 500 {
                    break;
                }
            }
            Err(e) => last = format!("could not reach {url}: {e}"),
        }
    }
    Err(last)
}

/// Refuse loopback and private ranges by default, so a URL that slipped
/// through cannot reach a metadata service or an internal admin page.
fn check_destination(url: &str, allow_private: bool) -> Result<(), String> {
    if !(url.starts_with("http://") || url.starts_with("https://")) {
        return Err(format!("`{url}` is not an http or https url"));
    }
    if allow_private {
        return Ok(());
    }
    let Some(host) = host_of(url) else {
        return Err(format!("`{url}` has no host"));
    };
    if is_private_host(&host) {
        return Err(format!(
            "`{host}` is a private address; set `[http] allow_private = true` in kora.toml to permit it"
        ));
    }
    Ok(())
}

fn host_of(url: &str) -> Option<String> {
    let rest = url.split_once("://")?.1;
    let authority = rest.split(['/', '?', '#']).next()?;
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    // Strip a port, but leave IPv6 brackets alone.
    let host = if host.starts_with('[') {
        host.split(']')
            .next()
            .unwrap_or(host)
            .trim_start_matches('[')
    } else {
        host.split(':').next().unwrap_or(host)
    };
    Some(host.to_ascii_lowercase())
}

fn is_private_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".localhost") || host.ends_with(".internal") {
        return true;
    }
    if let Ok(addr) = host.parse::<std::net::Ipv4Addr>() {
        let [a, b, ..] = addr.octets();
        return addr.is_loopback()
            || addr.is_link_local()
            || a == 10
            || (a == 172 && (16..=31).contains(&b))
            || (a == 192 && b == 168)
            // The cloud metadata endpoint, the classic SSRF target.
            || (a == 169 && b == 254);
    }
    if let Ok(addr) = host.parse::<std::net::Ipv6Addr>() {
        return addr.is_loopback() || addr.segments()[0] & 0xfe00 == 0xfc00;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_hosts_from_urls() {
        assert_eq!(
            host_of("https://api.example.com/v1/x"),
            Some("api.example.com".into())
        );
        assert_eq!(
            host_of("http://example.com:8080/"),
            Some("example.com".into())
        );
        assert_eq!(
            host_of("https://user:pw@example.com/x"),
            Some("example.com".into())
        );
        assert_eq!(host_of("https://[::1]:80/x"), Some("::1".into()));
    }

    #[test]
    fn private_ranges_are_recognised() {
        assert!(is_private_host("localhost"));
        assert!(is_private_host("127.0.0.1"));
        assert!(is_private_host("10.0.0.1"));
        assert!(is_private_host("172.16.5.4"));
        assert!(is_private_host("192.168.1.1"));
        // The cloud metadata service, the SSRF target that keeps appearing in
        // real incidents.
        assert!(is_private_host("169.254.169.254"));
        assert!(is_private_host("::1"));

        assert!(!is_private_host("example.com"));
        assert!(!is_private_host("8.8.8.8"));
        assert!(!is_private_host("172.32.0.1"), "172.32 is public");
    }

    #[test]
    fn non_http_schemes_are_refused() {
        assert!(check_destination("file:///etc/passwd", false).is_err());
        assert!(check_destination("gopher://x/", false).is_err());
    }

    #[test]
    fn private_hosts_can_be_allowed_explicitly() {
        assert!(check_destination("http://localhost:8080/", false).is_err());
        assert!(check_destination("http://localhost:8080/", true).is_ok());
    }
}
