//! Drives the server over an in-memory connection, so the protocol wiring is
//! tested rather than only the helpers behind it.

use std::thread;

use lsp_server::{Connection, Message, Notification, Request};
use lsp_types::{
    DidOpenTextDocumentParams, DocumentSymbolResponse, GotoDefinitionResponse, Hover,
    HoverContents, Position, TextDocumentIdentifier, TextDocumentItem, Url,
};

const SOURCE: &str = r#"type Ticket:
    severity: str
    summary: str

agent triage(raw: str) -> str:
    "Classify a support ticket."
    return raw

def main():
    triage("x")
    print(undefined_name)
"#;

fn uri() -> Url {
    Url::parse("file:///test.ko").unwrap()
}

fn open(client: &Connection, text: &str) {
    client
        .sender
        .send(Message::Notification(Notification::new(
            "textDocument/didOpen".to_string(),
            DidOpenTextDocumentParams {
                text_document: TextDocumentItem {
                    uri: uri(),
                    language_id: "kora".to_string(),
                    version: 1,
                    text: text.to_string(),
                },
            },
        )))
        .unwrap();
}

fn start_with(text: &str) -> (Connection, thread::JoinHandle<()>) {
    let (client, server) = Connection::memory();
    let handle = thread::spawn(move || {
        kora_lsp::serve(&server).expect("server should run");
    });
    open(&client, text);
    (client, handle)
}

fn start() -> (Connection, thread::JoinHandle<()>) {
    start_with(SOURCE)
}

fn finish(client: Connection, handle: thread::JoinHandle<()>) {
    drop(client);
    handle.join().unwrap();
}

fn request(
    client: &Connection,
    id: i32,
    method: &str,
    params: serde_json::Value,
) -> serde_json::Value {
    client
        .sender
        .send(Message::Request(Request {
            id: id.into(),
            method: method.to_string(),
            params,
        }))
        .unwrap();
    loop {
        // Diagnostics arrive unprompted; skip past them.
        if let Message::Response(response) = client.receiver.recv().unwrap() {
            return response.result.unwrap_or(serde_json::Value::Null);
        }
    }
}

#[test]
fn opening_a_document_publishes_diagnostics() {
    let (client, handle) = start();
    let Message::Notification(notification) = client.receiver.recv().unwrap() else {
        panic!("expected a diagnostics notification");
    };
    assert_eq!(notification.method, "textDocument/publishDiagnostics");

    let params: lsp_types::PublishDiagnosticsParams =
        serde_json::from_value(notification.params).unwrap();
    assert_eq!(params.diagnostics.len(), 1, "{:?}", params.diagnostics);
    assert!(params.diagnostics[0]
        .message
        .contains("`undefined_name` is not defined"));
    // Source line 11 is line 10 for the editor.
    assert_eq!(params.diagnostics[0].range.start.line, 10);

    finish(client, handle);
}

#[test]
fn hover_shows_a_signature_and_docstring() {
    let (client, handle) = start();
    let result = request(
        &client,
        1,
        "textDocument/hover",
        serde_json::json!({
            "textDocument": TextDocumentIdentifier { uri: uri() },
            "position": Position { line: 9, character: 5 },
        }),
    );
    let hover: Hover = serde_json::from_value(result).expect("a hover response");
    let HoverContents::Markup(markup) = hover.contents else {
        panic!("expected markdown hover");
    };
    assert!(
        markup.value.contains("agent triage(raw: str) -> str"),
        "got: {}",
        markup.value
    );
    assert!(
        markup.value.contains("Classify a support ticket."),
        "the docstring should appear on hover, got: {}",
        markup.value
    );

    finish(client, handle);
}

#[test]
fn go_to_definition_finds_the_declaration() {
    let (client, handle) = start();
    let result = request(
        &client,
        2,
        "textDocument/definition",
        serde_json::json!({
            "textDocument": TextDocumentIdentifier { uri: uri() },
            "position": Position { line: 9, character: 5 },
        }),
    );
    let response: GotoDefinitionResponse =
        serde_json::from_value(result).expect("a definition response");
    let GotoDefinitionResponse::Scalar(location) = response else {
        panic!("expected a single location");
    };
    // `agent triage` is on source line 5, which is line 4 for the editor.
    assert_eq!(location.range.start.line, 4);

    finish(client, handle);
}

#[test]
fn document_symbols_list_the_outline_in_file_order() {
    let (client, handle) = start();
    let result = request(
        &client,
        3,
        "textDocument/documentSymbol",
        serde_json::json!({ "textDocument": TextDocumentIdentifier { uri: uri() } }),
    );
    let response: DocumentSymbolResponse =
        serde_json::from_value(result).expect("a symbol response");
    let DocumentSymbolResponse::Nested(symbols) = response else {
        panic!("expected nested symbols");
    };
    let names: Vec<&str> = symbols.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"Ticket"), "{names:?}");
    assert!(names.contains(&"triage"), "{names:?}");
    assert!(names.contains(&"main"), "{names:?}");
    assert_eq!(names[0], "Ticket", "the outline should read in file order");

    finish(client, handle);
}

#[test]
fn completion_after_a_module_offers_only_its_functions() {
    let (client, handle) = start_with("use json\ndef main():\n    json.\n");
    let result = request(
        &client,
        4,
        "textDocument/completion",
        serde_json::json!({
            "textDocument": TextDocumentIdentifier { uri: uri() },
            "position": Position { line: 2, character: 9 },
        }),
    );
    let items: Vec<lsp_types::CompletionItem> =
        serde_json::from_value(result).expect("completion items");
    let labels: Vec<&str> = items.iter().map(|i| i.label.as_str()).collect();
    assert!(labels.contains(&"parse"), "{labels:?}");
    assert!(labels.contains(&"stringify"), "{labels:?}");
    assert!(
        !labels.contains(&"print"),
        "after `json.` only module functions belong, got {labels:?}"
    );

    finish(client, handle);
}

#[test]
fn editing_a_document_refreshes_its_diagnostics() {
    let (client, handle) = start_with("def main():\n    print(nope)\n");
    let _ = client.receiver.recv().unwrap();

    // Fix the problem; the next publish should be clean.
    client
        .sender
        .send(Message::Notification(Notification::new(
            "textDocument/didChange".to_string(),
            serde_json::json!({
                "textDocument": { "uri": uri(), "version": 2 },
                "contentChanges": [{ "text": "def main():\n    print(1)\n" }],
            }),
        )))
        .unwrap();

    let Message::Notification(notification) = client.receiver.recv().unwrap() else {
        panic!("expected diagnostics after the edit");
    };
    let params: lsp_types::PublishDiagnosticsParams =
        serde_json::from_value(notification.params).unwrap();
    assert!(
        params.diagnostics.is_empty(),
        "fixing the code should clear the squiggle, got {:?}",
        params.diagnostics
    );

    finish(client, handle);
}
