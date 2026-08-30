//! Token-by-token streaming for `analyze()` on a `str` result.
//!
//! Streaming changes when the program sees characters, never what the call
//! returns: the outcome is still parsed from the complete response by
//! [`crate::validate::parse_response`], so a streamed call and a blocking one
//! answer identically. What the stream adds is a view of the answer while it
//! is still being written.
//!
//! Two rules are load-bearing here:
//!
//! * **A response that has already emitted characters is never retried.**
//!   The transport retries a request that failed before the first delta,
//!   because nothing was observed and a second attempt is free of
//!   consequence. Once a character has reached the program, a retry would
//!   replay the answer from the start on top of output the program has
//!   already acted on. That is the same reason tool calls are not retried:
//!   the point of failure is exactly where "did it happen" stops being
//!   knowable.
//! * **Deltas carry the answer, not the JSON around it.** The wire shape is
//!   an object so the refusal channel survives, so the raw stream is
//!   `{"__uncertain__":"","answer":"Hel` — fragments of syntax, useless to
//!   show anyone. [`TextExtractor`] pulls the string value of `answer` out
//!   of the partial JSON and hands over only that.

use serde_json::Value;

use crate::schema::TEXT_KEY;
use crate::ModelError;

/// What the consumer wants the stream to do next.
///
/// `Stop` exists because the program watching a stream may learn, part way
/// through, that it no longer wants the rest: a budget ran out, an enclosing
/// construct is unwinding. Draining the rest of the response would be paying
/// for tokens nobody will read.
#[derive(Debug, Clone, PartialEq)]
pub enum Flow {
    Continue,
    Stop,
}

/// One HTTP POST that reads a response line by line instead of whole.
///
/// The callback is handed each raw line of the response body with any
/// `data: ` prefix already stripped, and returns whether to keep reading.
/// Isolated behind a function type for the same reason the blocking
/// [`crate::provider::Transport`] is: the provider-specific parsing above it
/// is worth testing without a socket.
pub(crate) type StreamTransport = dyn Fn(
    &str,
    &[(&str, String)],
    &Value,
    &mut dyn FnMut(&str) -> Result<Flow, ModelError>,
) -> Result<(), ModelError>;

/// Pulls the value of the `answer` field out of a JSON object arriving in
/// fragments.
///
/// A deliberately small machine rather than an incremental JSON parser: the
/// shape is fixed and known — one string field, at the top level, whose name
/// is a compile-time constant — so the general problem does not need solving
/// to answer the only question being asked. It reports characters exactly
/// once, in order, decoding escapes as it goes, and never reports a
/// character it might later have to take back.
#[derive(Debug, Default)]
pub struct TextExtractor {
    /// Everything seen so far, kept because the outcome is parsed from the
    /// complete body once the stream ends.
    raw: String,
    state: State,
    /// The last complete string token read outside the answer, so a `:`
    /// arriving next can tell whether it was a key and which one.
    last_token: String,
    /// Set when the `answer` key has been read and its `:` seen, so the next
    /// string that opens is the value rather than another key.
    expecting_value: bool,
    /// Holds a `\uXXXX` escape while its four digits are still arriving.
    pending_escape: String,
    /// Holds the leading half of a `\uXXXX\uXXXX` surrogate pair while the
    /// trailing half is still outstanding. Neither half is a character on
    /// its own, so nothing can be emitted until the other one arrives.
    pending_surrogate: Option<u32>,
}

#[derive(Debug, Default, PartialEq)]
enum State {
    /// Outside any string.
    #[default]
    Between,
    /// Inside a string that is not the answer — a key, or the refusal
    /// reason. Tracked rather than skipped so that a reason containing
    /// `"answer":` cannot be mistaken for the field itself.
    Token { escaped: bool },
    /// Inside the answer, emitting characters as they arrive.
    Value,
    /// Inside the answer, having just read a backslash.
    Escape,
    /// Inside a `\uXXXX` escape in the answer, collecting hex digits.
    Unicode,
    /// The answer ended. Nothing further is emitted.
    Done,
}

impl TextExtractor {
    pub fn new() -> TextExtractor {
        TextExtractor::default()
    }

    /// The complete response body seen so far.
    pub fn raw(&self) -> &str {
        &self.raw
    }

    /// Give up on a surrogate whose other half never came. Substituting is
    /// preferred to dropping so that the loss shows in the output rather
    /// than passing for text the model never wrote.
    fn flush_surrogate(pending: &mut Option<u32>, out: &mut String) {
        if pending.take().is_some() {
            out.push(char::REPLACEMENT_CHARACTER);
        }
    }

    /// Feed one fragment of the response; returns the characters of `answer`
    /// that became known because of it, which is very often none.
    pub fn push(&mut self, fragment: &str) -> String {
        self.raw.push_str(fragment);
        let mut out = String::new();
        for ch in fragment.chars() {
            match self.state {
                State::Between => match ch {
                    '"' if self.expecting_value => {
                        self.expecting_value = false;
                        self.state = State::Value;
                    }
                    '"' => {
                        self.last_token.clear();
                        self.state = State::Token { escaped: false };
                    }
                    ':' => self.expecting_value = self.last_token == TEXT_KEY,
                    _ => {}
                },
                State::Token { escaped } => {
                    if escaped {
                        self.last_token.push(ch);
                        self.state = State::Token { escaped: false };
                    } else {
                        match ch {
                            '\\' => self.state = State::Token { escaped: true },
                            '"' => self.state = State::Between,
                            _ => self.last_token.push(ch),
                        }
                    }
                }
                State::Value => match ch {
                    '"' => {
                        Self::flush_surrogate(&mut self.pending_surrogate, &mut out);
                        self.state = State::Done;
                    }
                    '\\' => self.state = State::Escape,
                    _ => {
                        Self::flush_surrogate(&mut self.pending_surrogate, &mut out);
                        out.push(ch);
                    }
                },
                State::Escape => {
                    self.state = State::Value;
                    // A `\u` may be the trailing half of a pair, so the
                    // pending half is only abandoned once this escape is
                    // known to be something else.
                    if ch != 'u' {
                        Self::flush_surrogate(&mut self.pending_surrogate, &mut out);
                    }
                    match ch {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            self.state = State::Unicode;
                            self.pending_escape.clear();
                        }
                        other => out.push(other),
                    }
                }
                State::Unicode => {
                    self.pending_escape.push(ch);
                    if self.pending_escape.len() == 4 {
                        let value = u32::from_str_radix(&self.pending_escape, 16).ok();
                        self.pending_escape.clear();
                        self.state = State::Value;
                        match (self.pending_surrogate, value) {
                            // The trailing half of a pair: combine into the
                            // one character the two of them stand for.
                            (Some(high), Some(low @ 0xDC00..=0xDFFF)) => {
                                self.pending_surrogate = None;
                                let combined = 0x1_0000 + ((high - 0xD800) << 10) + (low - 0xDC00);
                                if let Some(decoded) = char::from_u32(combined) {
                                    out.push(decoded);
                                }
                            }
                            // A leading half: hold it back, since on its own
                            // it names no character.
                            (None, Some(high @ 0xD800..=0xDBFF)) => {
                                self.pending_surrogate = Some(high)
                            }
                            (pending, value) => {
                                if pending.is_some() {
                                    self.pending_surrogate = None;
                                    out.push(char::REPLACEMENT_CHARACTER);
                                }
                                match value {
                                    // A second leading half in a row: the
                                    // first is unpaired, this one still may
                                    // not be.
                                    Some(high @ 0xD800..=0xDBFF) => {
                                        self.pending_surrogate = Some(high)
                                    }
                                    Some(v) => out.push(
                                        char::from_u32(v).unwrap_or(char::REPLACEMENT_CHARACTER),
                                    ),
                                    None => out.push(char::REPLACEMENT_CHARACTER),
                                }
                            }
                        }
                    }
                }
                State::Done => {}
            }
        }
        out
    }
}

/// Run a streaming analyze call over the given transport.
///
/// Split from the socket for the same reason the blocking path is: the parts
/// worth arguing about — which characters reach the program, what a broken
/// stream returns, what a stopped stream costs — should be checkable without
/// a listening port.
pub(crate) fn analyze_streaming_with(
    config: &crate::ModelConfig,
    req: &crate::AnalyzeRequest,
    transport: &StreamTransport,
    on_text: &mut dyn FnMut(&str) -> Result<Flow, ModelError>,
) -> Result<crate::AnalyzeOutcome, ModelError> {
    let request = crate::provider::stream_request(config, req)?;
    let mut extractor = TextExtractor::new();
    let mut tokens_in = 0u64;
    let mut tokens_out = 0u64;
    let mut stopped = false;

    let result = transport(
        &request.url,
        &request.headers,
        &request.body,
        &mut |payload| {
            let part = crate::provider::parse_delta(&config.provider, payload)?;
            if part.tokens_in > 0 {
                tokens_in = part.tokens_in;
            }
            if part.tokens_out > 0 {
                tokens_out = part.tokens_out;
            }
            if part.text.is_empty() {
                return Ok(Flow::Continue);
            }
            let visible = extractor.push(&part.text);
            if visible.is_empty() {
                return Ok(Flow::Continue);
            }
            match on_text(&visible)? {
                Flow::Continue => Ok(Flow::Continue),
                Flow::Stop => {
                    stopped = true;
                    Ok(Flow::Stop)
                }
            }
        },
    );

    if let Err(e) = result {
        // What was already written is not thrown away: the program has seen
        // it, and a reason that arrives without it reads as though nothing
        // happened at all.
        let seen = extractor.raw().len();
        let detail = if seen == 0 {
            e.message
        } else {
            format!(
                "{} (after {seen} characters had already arrived)",
                e.message
            )
        };
        return Ok(crate::AnalyzeOutcome::Failed {
            reason: detail,
            tokens_in,
            tokens_out,
        });
    }

    if stopped {
        // A stopped stream has no complete JSON object to validate, and the
        // caller stopped it because it had stopped caring about the answer.
        return Ok(crate::AnalyzeOutcome::Failed {
            reason: "the stream was stopped before the answer finished".to_string(),
            tokens_in,
            tokens_out,
        });
    }

    crate::validate::parse_response(extractor.raw(), &req.schema, tokens_in, tokens_out)
}

/// The real streaming transport: one POST whose body is read as it arrives.
///
/// Retries only while nothing has been observed. `emitted` is set by the
/// first line handed to the callback, and once set the loop stops being a
/// retry loop — a stream that dies half way through is a `Failed` outcome
/// carrying what was already written, not a request to run again.
pub(crate) fn stream_transport_for(config: &crate::ModelConfig) -> Box<StreamTransport> {
    let timeout = std::time::Duration::from_secs(config.timeout_secs.max(1));
    let attempts = config.max_retries.saturating_add(1);
    Box::new(
        move |url: &str,
              headers: &[(&str, String)],
              body: &Value,
              on_line: &mut dyn FnMut(&str) -> Result<Flow, ModelError>| {
            let mut attempt = 0;
            loop {
                attempt += 1;
                let mut emitted = false;
                match send_streaming(url, headers, body, timeout, &mut emitted, on_line) {
                    Ok(()) => return Ok(()),
                    Err(e) => {
                        if emitted || !e.retryable || attempt >= attempts {
                            return Err(e);
                        }
                    }
                }
                std::thread::sleep(std::time::Duration::from_millis(
                    crate::provider::retry_base_delay(attempt),
                ));
            }
        },
    )
}

/// One streaming attempt: send, then read the body line by line.
fn send_streaming(
    url: &str,
    headers: &[(&str, String)],
    body: &Value,
    timeout: std::time::Duration,
    emitted: &mut bool,
    on_line: &mut dyn FnMut(&str) -> Result<Flow, ModelError>,
) -> Result<(), ModelError> {
    use std::io::BufRead;

    let agent = ureq::AgentBuilder::new().timeout(timeout).build();
    let mut request = agent.post(url);
    for (name, value) in headers {
        request = request.set(name, value);
    }
    let response = match request.send_json(body.clone()) {
        Ok(response) => response,
        Err(ureq::Error::Status(code, response)) => {
            let text = response.into_string().unwrap_or_default();
            let message = format!(
                "{url} returned HTTP {code}: {}",
                crate::validate::truncate(&text, 300)
            );
            return Err(if crate::provider::retryable_status(code) {
                ModelError::retryable(message)
            } else {
                ModelError::new(message)
            });
        }
        Err(e) => {
            return Err(ModelError::retryable(format!(
                "request to {url} failed: {e}"
            )))
        }
    };

    let reader = std::io::BufReader::new(response.into_reader());
    for line in reader.lines() {
        let line = line.map_err(|e| {
            // Deliberately not retryable: characters have reached the
            // program by now, and a second attempt would write the answer
            // twice.
            ModelError::new(format!("stream from {url} broke mid-response: {e}"))
        })?;
        let payload = line.strip_prefix("data: ").unwrap_or(&line).trim();
        if payload.is_empty() {
            continue;
        }
        *emitted = true;
        if on_line(payload)? == Flow::Stop {
            return Ok(());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(fragments: &[&str]) -> String {
        let mut extractor = TextExtractor::new();
        let mut out = String::new();
        for fragment in fragments {
            out.push_str(&extractor.push(fragment));
        }
        out
    }

    #[test]
    fn extracts_the_answer_from_one_piece() {
        assert_eq!(
            drain(&[r#"{"__uncertain__":"","answer":"hello"}"#]),
            "hello"
        );
    }

    #[test]
    fn extracts_across_fragment_boundaries() {
        // The boundaries land inside the key, inside the value, and inside
        // an escape, because a provider splits wherever it likes.
        assert_eq!(
            drain(&[
                r#"{"__uncerta"#,
                r#"in__":"","ans"#,
                r#"wer":"hel"#,
                r#"lo, wor"#,
                r#"ld"}"#,
            ]),
            "hello, world"
        );
    }

    #[test]
    fn decodes_escapes_split_across_fragments() {
        assert_eq!(drain(&[r#"{"answer":"a\"#, r#"nb"}"#]), "a\nb");
        assert_eq!(drain(&[r#"{"answer":"a\u00"#, r#"e9b"}"#]), "aéb");
    }

    #[test]
    fn a_quote_inside_the_answer_does_not_end_it() {
        assert_eq!(
            drain(&[r#"{"answer":"she said \"hi\" twice"}"#]),
            "she said \"hi\" twice"
        );
    }

    #[test]
    fn nothing_is_emitted_after_the_value_closes() {
        assert_eq!(drain(&[r#"{"answer":"done","__uncertain__":""}"#]), "done");
    }

    #[test]
    fn a_refusal_reason_mentioning_the_key_is_not_mistaken_for_it() {
        // The reason text contains the word, but not as a quoted key.
        assert_eq!(
            drain(&[r#"{"__uncertain__":"no answer available","answer":""}"#]),
            ""
        );
    }

    #[test]
    fn a_refusal_reason_quoting_the_key_is_still_not_mistaken_for_it() {
        // The reason contains an escaped `"answer":` — the exact byte
        // sequence a scanner that ignored string boundaries would trip on.
        let body = r#"{"__uncertain__":"the \"answer\": field is undefined here","answer":"real"}"#;
        assert_eq!(drain(&[body]), "real");
    }

    #[test]
    fn refusal_is_known_before_the_answer_begins() {
        // `__uncertain__` sorts first, so a program watching the stream has
        // already seen the refusal by the time any answer would arrive.
        let mut extractor = TextExtractor::new();
        assert_eq!(extractor.push(r#"{"__uncertain__":"cannot comply","#), "");
        assert!(extractor.raw().contains("cannot comply"));
    }

    #[test]
    fn decodes_a_surrogate_pair_into_one_character() {
        // Providers encode anything outside the basic multilingual plane as
        // a `\uXXXX\uXXXX` pair. Decoding the halves separately yields two
        // values that are not characters at all, so the pair has to be
        // recognised as one unit.
        assert_eq!(drain(&[r#"{"answer":"hi \uD83D\uDE00"}"#]), "hi 😀");
    }

    #[test]
    fn decodes_a_surrogate_pair_split_across_fragments() {
        // The split lands between the two halves, which is where a provider
        // is most likely to put it.
        assert_eq!(drain(&[r#"{"answer":"\uD83D"#, r#"\uDE00"}"#]), "😀");
    }

    #[test]
    fn a_lone_surrogate_becomes_the_replacement_character() {
        // Nothing valid can be built from half a pair. Substituting keeps
        // the loss visible rather than dropping the character silently.
        assert_eq!(drain(&[r#"{"answer":"a\uD83Db"}"#]), "a\u{fffd}b");
    }

    #[test]
    fn a_lone_surrogate_before_a_real_escape_loses_only_itself() {
        assert_eq!(drain(&[r#"{"answer":"\uD83DA"}"#]), "\u{fffd}A");
    }

    #[test]
    fn raw_keeps_the_whole_body_for_the_final_parse() {
        let mut extractor = TextExtractor::new();
        extractor.push(r#"{"answer":"hi"#);
        extractor.push(r#""}"#);
        assert_eq!(extractor.raw(), r#"{"answer":"hi"}"#);
    }
}
