//! kora-lsp: the language server.
//!
//! Diagnostics, hover, go-to-definition, document symbols, and completion —
//! all driven by the same analysis pass the compiler uses, so the editor's
//! answers can never drift from the compiler's.
//!
//! Synchronous by design: `lsp-server` (rust-analyzer's transport) needs no
//! async runtime, and a language server for a file-sized language has no work
//! that benefits from one.

use std::collections::HashMap;
use std::error::Error;

use lsp_server::{Connection, ExtractError, Message, Request, RequestId, Response};
use lsp_types::request::{
    Completion, DocumentSymbolRequest, GotoDefinition, HoverRequest, Request as _,
};

use lsp_types::{
    CompletionItem, CompletionItemKind, CompletionOptions, CompletionResponse, Diagnostic,
    DiagnosticSeverity, DocumentSymbol, DocumentSymbolResponse, GotoDefinitionResponse, Hover,
    HoverContents, HoverProviderCapability, Location, MarkupContent, MarkupKind, OneOf, Position,
    PublishDiagnosticsParams, Range, ServerCapabilities, SymbolKind as LspSymbolKind,
    TextDocumentSyncCapability, TextDocumentSyncKind, Url,
};

use kora_types::{Analysis, Severity, Symbol, SymbolKind};

/// Open documents, by URI.
#[derive(Default)]
struct Documents {
    text: HashMap<Url, String>,
}

/// Run the server over stdio until the client disconnects.
pub fn run() -> Result<(), Box<dyn Error + Sync + Send>> {
    let (connection, io_threads) = Connection::stdio();

    let capabilities = serde_json::to_value(ServerCapabilities {
        text_document_sync: Some(TextDocumentSyncCapability::Kind(TextDocumentSyncKind::FULL)),
        hover_provider: Some(HoverProviderCapability::Simple(true)),
        definition_provider: Some(OneOf::Left(true)),
        document_symbol_provider: Some(OneOf::Left(true)),
        completion_provider: Some(CompletionOptions {
            trigger_characters: Some(vec![".".to_string()]),
            ..Default::default()
        }),
        ..Default::default()
    })?;

    connection.initialize(capabilities)?;
    serve(&connection)?;
    // The client may close the stream instead of sending `exit`, so a join
    // here must not be what keeps the process alive.
    drop(connection);
    io_threads.join()?;
    Ok(())
}

/// Serve one connection. Public so tests can drive it in memory.
pub fn serve(connection: &Connection) -> Result<(), Box<dyn Error + Sync + Send>> {
    let mut documents = Documents::default();

    for message in &connection.receiver {
        match message {
            Message::Request(request) => {
                if connection.handle_shutdown(&request)? {
                    return Ok(());
                }
                let response = respond(&request, &documents);
                connection.sender.send(Message::Response(response))?;
            }
            Message::Notification(notification) => {
                if let Some((uri, text)) = document_change(&notification) {
                    documents.text.insert(uri.clone(), text.clone());
                    let params = PublishDiagnosticsParams {
                        uri,
                        diagnostics: diagnose(&text),
                        version: None,
                    };
                    connection.sender.send(Message::Notification(
                        lsp_server::Notification::new(
                            "textDocument/publishDiagnostics".to_string(),
                            params,
                        ),
                    ))?;
                }
            }
            Message::Response(_) => {}
        }
    }
    Ok(())
}

/// The text of a document after an open or change notification.
fn document_change(notification: &lsp_server::Notification) -> Option<(Url, String)> {
    match notification.method.as_str() {
        "textDocument/didOpen" => {
            let params: lsp_types::DidOpenTextDocumentParams =
                serde_json::from_value(notification.params.clone()).ok()?;
            Some((params.text_document.uri, params.text_document.text))
        }
        "textDocument/didChange" => {
            let params: lsp_types::DidChangeTextDocumentParams =
                serde_json::from_value(notification.params.clone()).ok()?;
            // Full sync, so the last change carries the whole document.
            let text = params.content_changes.into_iter().next_back()?.text;
            Some((params.text_document.uri, text))
        }
        _ => None,
    }
}

fn respond(request: &Request, documents: &Documents) -> Response {
    let id = request.id.clone();
    match request.method.as_str() {
        HoverRequest::METHOD => reply(id, hover(request, documents)),
        GotoDefinition::METHOD => reply(id, definition(request, documents)),
        DocumentSymbolRequest::METHOD => reply(id, symbols(request, documents)),
        Completion::METHOD => reply(id, completion(request, documents)),
        _ => Response::new_ok(id, serde_json::Value::Null),
    }
}

fn reply<T: serde::Serialize>(id: RequestId, value: Option<T>) -> Response {
    match value {
        Some(v) => match serde_json::to_value(v) {
            Ok(json) => Response::new_ok(id, json),
            Err(e) => Response::new_err(id, 0, e.to_string()),
        },
        None => Response::new_ok(id, serde_json::Value::Null),
    }
}

/// Parse and check a document. Parse errors become a single diagnostic;
/// otherwise the checker's findings are reported.
fn diagnose(text: &str) -> Vec<Diagnostic> {
    match kora_syntax::parse(text) {
        Err(e) => vec![Diagnostic {
            range: range_at(
                e.span.line,
                e.span.col,
                e.span.end.saturating_sub(e.span.start),
            ),
            severity: Some(DiagnosticSeverity::ERROR),
            source: Some("kora".to_string()),
            message: match &e.hint {
                Some(hint) => format!("{}\nhint: {hint}", e.message),
                None => e.message.clone(),
            },
            ..Default::default()
        }],
        Ok(program) => kora_types::analyze(&program)
            .diagnostics
            .into_iter()
            .map(|d| Diagnostic {
                range: range_at(d.span.line, d.span.col, name_width(&d.message)),
                severity: Some(match d.severity {
                    Severity::Error => DiagnosticSeverity::ERROR,
                    Severity::Warning => DiagnosticSeverity::WARNING,
                }),
                source: Some("kora".to_string()),
                message: match &d.hint {
                    Some(hint) => format!("{}\nhint: {hint}", d.message),
                    None => d.message.clone(),
                },
                ..Default::default()
            })
            .collect(),
    }
}

/// Underline the offending name rather than a single character, by taking the
/// first backtick-quoted word out of the message.
fn name_width(message: &str) -> usize {
    message
        .split('`')
        .nth(1)
        .map(|name| name.chars().count())
        .unwrap_or(1)
}

fn range_at(line: u32, column: u32, width: usize) -> Range {
    // LSP positions are zero-based; ours are one-based.
    let start = Position {
        line: line.saturating_sub(1),
        character: column.saturating_sub(1),
    };
    Range {
        start,
        end: Position {
            character: start.character + width.max(1) as u32,
            ..start
        },
    }
}

fn analysis_for(documents: &Documents, uri: &Url) -> Option<Analysis> {
    let text = documents.text.get(uri)?;
    let program = kora_syntax::parse(text).ok()?;
    Some(kora_types::analyze(&program))
}

fn hover(request: &Request, documents: &Documents) -> Option<Hover> {
    let params: lsp_types::HoverParams = cast(request)?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let analysis = analysis_for(documents, &uri)?;
    let symbol = analysis.symbol_at(position.line + 1, position.character + 1)?;

    let mut markdown = format!("```kora\n{}\n```", symbol.detail);
    if let Some(doc) = &symbol.doc {
        markdown.push_str(&format!("\n\n{doc}"));
    }
    Some(Hover {
        contents: HoverContents::Markup(MarkupContent {
            kind: MarkupKind::Markdown,
            value: markdown,
        }),
        range: None,
    })
}

fn definition(request: &Request, documents: &Documents) -> Option<GotoDefinitionResponse> {
    let params: lsp_types::GotoDefinitionParams = cast(request)?;
    let uri = params.text_document_position_params.text_document.uri;
    let position = params.text_document_position_params.position;
    let analysis = analysis_for(documents, &uri)?;
    let symbol = analysis.symbol_at(position.line + 1, position.character + 1)?;

    Some(GotoDefinitionResponse::Scalar(Location {
        uri,
        range: range_at(
            symbol.span.line,
            symbol.span.col,
            symbol.name.chars().count(),
        ),
    }))
}

fn symbols(request: &Request, documents: &Documents) -> Option<DocumentSymbolResponse> {
    let params: lsp_types::DocumentSymbolParams = cast(request)?;
    let analysis = analysis_for(documents, &params.text_document.uri)?;

    let mut items: Vec<Symbol> = analysis.symbols.into_values().collect();
    items.sort_by_key(|s| s.span.line);

    #[allow(deprecated)] // `deprecated` field is required by the struct
    let out: Vec<DocumentSymbol> = items
        .into_iter()
        .map(|s| {
            let range = range_at(s.span.line, s.span.col, s.name.chars().count());
            DocumentSymbol {
                name: s.name,
                detail: Some(s.detail.lines().next().unwrap_or_default().to_string()),
                kind: lsp_kind(s.kind),
                tags: None,
                deprecated: None,
                range,
                selection_range: range,
                children: None,
            }
        })
        .collect();
    Some(DocumentSymbolResponse::Nested(out))
}

fn lsp_kind(kind: SymbolKind) -> LspSymbolKind {
    match kind {
        SymbolKind::Function => LspSymbolKind::FUNCTION,
        // Agents and tools are the language's distinctive callables, so they
        // get their own icons in the outline rather than all looking alike.
        SymbolKind::Agent => LspSymbolKind::CLASS,
        SymbolKind::Tool => LspSymbolKind::METHOD,
        SymbolKind::Type => LspSymbolKind::STRUCT,
        SymbolKind::Field => LspSymbolKind::FIELD,
        SymbolKind::Module => LspSymbolKind::MODULE,
        SymbolKind::Variable => LspSymbolKind::VARIABLE,
        SymbolKind::Test => LspSymbolKind::EVENT,
    }
}

fn completion(request: &Request, documents: &Documents) -> Option<CompletionResponse> {
    let params: lsp_types::CompletionParams = cast(request)?;
    let uri = params.text_document_position.text_document.uri;
    let position = params.text_document_position.position;
    let text = documents.text.get(&uri)?;

    let line = text.lines().nth(position.line as usize).unwrap_or("");
    let prefix: String = line.chars().take(position.character as usize).collect();

    // After `module.`, offer that module's functions and nothing else.
    if let Some(alias) = prefix.rsplit_once('.').and_then(|(head, tail)| {
        tail.is_empty()
            .then(|| {
                head.rsplit(|c: char| !c.is_alphanumeric() && c != '_')
                    .next()
            })
            .flatten()
    }) {
        // Completion happens *while* the code is incomplete, so this cannot
        // depend on the document parsing. Read the `use` lines directly.
        let aliases = module_aliases(text);
        if let Some(module) = aliases.get(alias) {
            if let Some(functions) = kora_types::module_functions(module) {
                return Some(CompletionResponse::Array(
                    functions
                        .iter()
                        .map(|name| CompletionItem {
                            label: (*name).to_string(),
                            kind: Some(CompletionItemKind::FUNCTION),
                            detail: Some(format!("{module}.{name}")),
                            ..Default::default()
                        })
                        .collect(),
                ));
            }
        }
        return Some(CompletionResponse::Array(Vec::new()));
    }

    let mut items: Vec<CompletionItem> = KEYWORDS
        .iter()
        .map(|k| CompletionItem {
            label: (*k).to_string(),
            kind: Some(CompletionItemKind::KEYWORD),
            ..Default::default()
        })
        .collect();

    items.extend(kora_types::builtin_names().iter().map(|b| CompletionItem {
        label: (*b).to_string(),
        kind: Some(CompletionItemKind::FUNCTION),
        ..Default::default()
    }));

    if let Some(analysis) = analysis_for(documents, &uri) {
        items.extend(analysis.symbols.into_values().map(|s| CompletionItem {
            label: s.name,
            kind: Some(match s.kind {
                SymbolKind::Type => CompletionItemKind::STRUCT,
                SymbolKind::Module => CompletionItemKind::MODULE,
                _ => CompletionItemKind::FUNCTION,
            }),
            detail: Some(s.detail.lines().next().unwrap_or_default().to_string()),
            ..Default::default()
        }));
    }
    Some(CompletionResponse::Array(items))
}

/// Module aliases, read straight from the text.
///
/// A lexical scan rather than a parse, because the document is mid-edit
/// whenever completion is asked for.
fn module_aliases(text: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        let Some(rest) = line.strip_prefix("use ") else {
            continue;
        };
        let mut words = rest.split_whitespace();
        let Some(module) = words.next() else { continue };
        let alias = match (words.next(), words.next()) {
            (Some("as"), Some(alias)) => alias,
            _ => module,
        };
        out.insert(alias.to_string(), module.to_string());
    }
    out
}

const KEYWORDS: &[&str] = &[
    "def",
    "agent",
    "tool",
    "type",
    "if",
    "elif",
    "else",
    "for",
    "while",
    "return",
    "match",
    "case",
    "parallel for",
    "budget",
    "classified",
    "declassify",
    "use",
    "test",
    "assert",
    "with mock",
    "with budget",
];

fn cast<P: serde::de::DeserializeOwned>(request: &Request) -> Option<P> {
    match serde_json::from_value(request.params.clone()) {
        Ok(params) => Some(params),
        Err(_) => {
            let _: ExtractError<Request> = ExtractError::MethodMismatch(request.clone());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_errors_become_a_diagnostic() {
        let diagnostics = diagnose("if x:\nprint(1)\n");
        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].severity, Some(DiagnosticSeverity::ERROR));
        assert!(
            diagnostics[0].message.contains("hint:"),
            "hints should reach the editor"
        );
    }

    #[test]
    fn checker_findings_become_diagnostics() {
        let diagnostics = diagnose("def main():\n    print(nope)\n");
        assert_eq!(diagnostics.len(), 1);
        assert!(diagnostics[0].message.contains("`nope` is not defined"));
    }

    #[test]
    fn a_clean_document_has_no_diagnostics() {
        assert!(diagnose("def main():\n    print(1)\n").is_empty());
    }

    #[test]
    fn ranges_are_zero_based_for_the_editor() {
        // Ours are one-based; LSP's are not, and an off-by-one here puts the
        // squiggle on the wrong line.
        let range = range_at(1, 1, 4);
        assert_eq!(range.start.line, 0);
        assert_eq!(range.start.character, 0);
        assert_eq!(range.end.character, 4);
    }

    #[test]
    fn diagnostics_underline_the_offending_name() {
        let diagnostics = diagnose("def main():\n    print(missing_name)\n");
        let range = diagnostics[0].range;
        assert_eq!(
            range.end.character - range.start.character,
            "missing_name".len() as u32,
            "the squiggle should cover the whole name"
        );
    }
}
