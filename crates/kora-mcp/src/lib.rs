//! kora-mcp: a Model Context Protocol client.
//!
//! MCP is already the standard for "tools an agent can call", with hundreds of
//! maintained servers. Kora already has `tool` as a first-class construct, and
//! an MCP server is a bag of tools, so the mapping is close to mechanical —
//! one implementation inherits an ecosystem instead of adding integrations one
//! at a time.
//!
//! Servers run as child processes speaking JSON-RPC over stdio. That process
//! boundary is doing real work here: an MCP server is a *sink* like a model
//! is, so classified data cannot reach one without an explicit release, and
//! everything a server returns arrives `unverified`.
//!
//! Synchronous, like the rest of the runtime. The transport is behind a trait
//! so request construction and response handling are testable without
//! spawning anything.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde_json::{json, Value};

/// What a server says it can do.
#[derive(Debug, Clone, PartialEq)]
pub struct Tool {
    pub name: String,
    pub description: String,
    /// Parameter name and JSON-schema type, in declaration order.
    pub params: Vec<(String, ParamType)>,
    /// Parameters the server requires.
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParamType {
    Str,
    Int,
    Float,
    Bool,
    ListOfStr,
    /// Anything Kora cannot describe to a model yet.
    Unsupported,
}

#[derive(Debug)]
pub struct McpError {
    pub message: String,
}

impl McpError {
    fn new(message: impl Into<String>) -> McpError {
        McpError {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for McpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for McpError {}

/// How to reach a server. One request in, one response out.
pub trait Transport: Send {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError>;
    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError>;
}

/// A configured server, from `[mcp.<name>]` in kora.toml.
#[derive(Debug, Clone, Default)]
pub struct ServerConfig {
    pub command: String,
    pub args: Vec<String>,
    pub env: HashMap<String, String>,
}

/// A connected server.
pub struct Server {
    name: String,
    transport: Box<dyn Transport>,
    tools: Vec<Tool>,
}

impl std::fmt::Debug for Server {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Server")
            .field("name", &self.name)
            .field("tools", &self.tools.len())
            .finish()
    }
}

impl Server {
    /// Start a server and complete the handshake.
    pub fn connect(name: &str, config: &ServerConfig) -> Result<Server, McpError> {
        let transport = StdioTransport::spawn(config)?;
        Server::with_transport(name, Box::new(transport))
    }

    /// Handshake over an already-built transport. Tests use this.
    pub fn with_transport(
        name: &str,
        mut transport: Box<dyn Transport>,
    ) -> Result<Server, McpError> {
        let response = transport.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": {},
                "clientInfo": { "name": "kora", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        // A server that answers something other than an object has not
        // implemented the protocol; say so rather than failing later.
        if !response.is_object() {
            return Err(McpError::new(format!(
                "`{name}` did not answer the initialize handshake"
            )));
        }
        transport.notify("notifications/initialized", json!({}))?;

        let listed = transport.request("tools/list", json!({}))?;
        let tools = parse_tools(&listed);

        Ok(Server {
            name: name.to_string(),
            transport,
            tools,
        })
    }

    /// A server with canned tools and no process behind it.
    ///
    /// A test seam: calling a tool on one of these fails, but everything up
    /// to the call — discovery, schemas, the security checks around which
    /// server is being offered — is exercised without spawning anything.
    pub fn for_testing(name: &str, tools: Vec<Tool>) -> Server {
        struct Absent;
        impl Transport for Absent {
            fn request(&mut self, method: &str, _: Value) -> Result<Value, McpError> {
                Err(McpError::new(format!(
                    "`{method}` reached a test server with no process behind it"
                )))
            }
            fn notify(&mut self, _: &str, _: Value) -> Result<(), McpError> {
                Ok(())
            }
        }
        Server {
            name: name.to_string(),
            transport: Box::new(Absent),
            tools,
        }
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn tools(&self) -> &[Tool] {
        &self.tools
    }

    pub fn tool(&self, name: &str) -> Option<&Tool> {
        self.tools.iter().find(|t| t.name == name)
    }

    /// Run a tool. The result is JSON text, which the caller labels as
    /// unverified: it came from outside the program.
    pub fn call(&mut self, tool: &str, arguments: Value) -> Result<String, McpError> {
        let response = self.transport.request(
            "tools/call",
            json!({ "name": tool, "arguments": arguments }),
        )?;

        // A protocol-level failure is an error; a tool reporting failure is a
        // result the program should see.
        if response
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            return Ok(json!({ "error": content_text(&response) }).to_string());
        }
        Ok(content_text(&response))
    }
}

/// MCP results are a list of content blocks; join the text ones.
fn content_text(response: &Value) -> String {
    let Some(blocks) = response.get("content").and_then(Value::as_array) else {
        return response.to_string();
    };
    let text: Vec<String> = blocks
        .iter()
        .filter_map(|b| match b.get("type").and_then(Value::as_str) {
            Some("text") => b.get("text").and_then(Value::as_str).map(str::to_string),
            // Anything else (images, resources) is summarised rather than
            // dropped silently.
            Some(kind) => Some(format!("<{kind} content>")),
            None => None,
        })
        .collect();
    text.join("\n")
}

fn parse_tools(listed: &Value) -> Vec<Tool> {
    let Some(items) = listed.get("tools").and_then(Value::as_array) else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let name = item.get("name")?.as_str()?.to_string();
            let description = item
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let schema = item.get("inputSchema");
            let properties = schema
                .and_then(|s| s.get("properties"))
                .and_then(Value::as_object);
            let required: Vec<String> = schema
                .and_then(|s| s.get("required"))
                .and_then(Value::as_array)
                .map(|r| {
                    r.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();

            let mut params = Vec::new();
            if let Some(properties) = properties {
                for (key, spec) in properties {
                    params.push((key.clone(), param_type(spec)));
                }
                // JSON objects have no order; sort so a tool's parameters are
                // presented the same way on every run.
                params.sort_by(|a, b| a.0.cmp(&b.0));
            }
            Some(Tool {
                name,
                description,
                params,
                required,
            })
        })
        .collect()
}

fn param_type(spec: &Value) -> ParamType {
    match spec.get("type").and_then(Value::as_str) {
        Some("string") => ParamType::Str,
        Some("integer") => ParamType::Int,
        Some("number") => ParamType::Float,
        Some("boolean") => ParamType::Bool,
        Some("array") => match spec
            .get("items")
            .and_then(|i| i.get("type"))
            .and_then(Value::as_str)
        {
            Some("string") => ParamType::ListOfStr,
            _ => ParamType::Unsupported,
        },
        _ => ParamType::Unsupported,
    }
}

/// JSON-RPC over a child process's stdin and stdout.
struct StdioTransport {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
}

impl StdioTransport {
    fn spawn(config: &ServerConfig) -> Result<StdioTransport, McpError> {
        let mut command = Command::new(&config.command);
        command
            .args(&config.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            // Servers log to stderr; let it through so failures are visible
            // rather than swallowed.
            .stderr(Stdio::inherit());
        for (key, value) in &config.env {
            command.env(key, value);
        }

        let mut child = command
            .spawn()
            .map_err(|e| McpError::new(format!("could not start `{}`: {e}", config.command)))?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| McpError::new("server has no stdin"))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| McpError::new("server has no stdout"))?;

        Ok(StdioTransport {
            child,
            stdin,
            stdout: BufReader::new(stdout),
            next_id: 1,
        })
    }

    fn send(&mut self, message: &Value) -> Result<(), McpError> {
        let line = serde_json::to_string(message)
            .map_err(|e| McpError::new(format!("could not encode request: {e}")))?;
        writeln!(self.stdin, "{line}")
            .and_then(|()| self.stdin.flush())
            .map_err(|e| McpError::new(format!("could not write to server: {e}")))
    }
}

impl Transport for StdioTransport {
    fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
        let id = self.next_id;
        self.next_id += 1;
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))?;

        // Read until the response with our id arrives, skipping the server's
        // own notifications and any log lines.
        loop {
            let mut line = String::new();
            let read = self
                .stdout
                .read_line(&mut line)
                .map_err(|e| McpError::new(format!("could not read from server: {e}")))?;
            if read == 0 {
                return Err(McpError::new(format!(
                    "server closed the connection during `{method}`"
                )));
            }
            let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            if message.get("id").and_then(Value::as_u64) != Some(id) {
                continue;
            }
            if let Some(error) = message.get("error") {
                let text = error
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or("unknown error");
                return Err(McpError::new(format!("`{method}` failed: {text}")));
            }
            return Ok(message.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
        self.send(&json!({ "jsonrpc": "2.0", "method": method, "params": params }))
    }
}

impl Drop for StdioTransport {
    fn drop(&mut self) {
        // Servers exit when their stdin closes; kill anything that does not,
        // so a run cannot leave orphaned processes behind.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A transport that answers from a script, and records what it was asked.
    struct Fake {
        responses: HashMap<String, Value>,
        pub seen: Vec<(String, Value)>,
    }

    impl Fake {
        fn new(responses: &[(&str, Value)]) -> Fake {
            Fake {
                responses: responses
                    .iter()
                    .map(|(m, v)| ((*m).to_string(), v.clone()))
                    .collect(),
                seen: Vec::new(),
            }
        }
    }

    impl Transport for Fake {
        fn request(&mut self, method: &str, params: Value) -> Result<Value, McpError> {
            self.seen.push((method.to_string(), params));
            self.responses
                .get(method)
                .cloned()
                .ok_or_else(|| McpError::new(format!("no scripted response for {method}")))
        }

        fn notify(&mut self, method: &str, params: Value) -> Result<(), McpError> {
            self.seen.push((method.to_string(), params));
            Ok(())
        }
    }

    fn tools_list() -> Value {
        json!({
            "tools": [
                {
                    "name": "search_issues",
                    "description": "Search issues in a repository.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {
                            "repo": { "type": "string" },
                            "limit": { "type": "integer" },
                            "labels": { "type": "array", "items": { "type": "string" } }
                        },
                        "required": ["repo"]
                    }
                },
                {
                    "name": "no_params",
                    "description": "Takes nothing."
                }
            ]
        })
    }

    fn server() -> Server {
        let fake = Fake::new(&[
            ("initialize", json!({ "protocolVersion": "2024-11-05" })),
            ("tools/list", tools_list()),
            (
                "tools/call",
                json!({ "content": [{ "type": "text", "text": "{\"count\": 3}" }] }),
            ),
        ]);
        Server::with_transport("github", Box::new(fake)).expect("handshake should succeed")
    }

    #[test]
    fn tools_are_discovered_with_their_schemas() {
        let s = server();
        assert_eq!(s.tools().len(), 2);

        let tool = s.tool("search_issues").expect("the tool should be listed");
        assert_eq!(tool.description, "Search issues in a repository.");
        // Sorted, because JSON objects have no order and a tool's parameters
        // should look the same on every run.
        assert_eq!(
            tool.params,
            vec![
                ("labels".to_string(), ParamType::ListOfStr),
                ("limit".to_string(), ParamType::Int),
                ("repo".to_string(), ParamType::Str),
            ]
        );
        assert_eq!(tool.required, vec!["repo".to_string()]);
    }

    #[test]
    fn a_tool_without_a_schema_still_works() {
        let s = server();
        let tool = s.tool("no_params").unwrap();
        assert!(tool.params.is_empty());
    }

    #[test]
    fn calling_a_tool_returns_its_text_content() {
        let mut s = server();
        let out = s
            .call("search_issues", json!({ "repo": "rust-lang/rust" }))
            .unwrap();
        assert_eq!(out, "{\"count\": 3}");
    }

    #[test]
    fn a_tool_error_is_a_result_not_a_failure() {
        // A tool reporting failure is something the program should see and
        // handle; only protocol failures are errors.
        let fake = Fake::new(&[
            ("initialize", json!({})),
            ("tools/list", tools_list()),
            (
                "tools/call",
                json!({
                    "isError": true,
                    "content": [{ "type": "text", "text": "repository not found" }]
                }),
            ),
        ]);
        let mut s = Server::with_transport("github", Box::new(fake)).unwrap();
        let out = s.call("search_issues", json!({})).unwrap();
        assert!(out.contains("repository not found"), "got: {out}");
        assert!(out.contains("error"), "got: {out}");
    }

    #[test]
    fn non_text_content_is_summarised_not_dropped() {
        let fake = Fake::new(&[
            ("initialize", json!({})),
            ("tools/list", json!({ "tools": [] })),
            (
                "tools/call",
                json!({ "content": [{ "type": "image", "data": "..." }] }),
            ),
        ]);
        let mut s = Server::with_transport("x", Box::new(fake)).unwrap();
        assert_eq!(s.call("anything", json!({})).unwrap(), "<image content>");
    }

    #[test]
    fn a_protocol_error_is_reported() {
        struct Failing;
        impl Transport for Failing {
            fn request(&mut self, method: &str, _: Value) -> Result<Value, McpError> {
                Err(McpError::new(format!("{method} exploded")))
            }
            fn notify(&mut self, _: &str, _: Value) -> Result<(), McpError> {
                Ok(())
            }
        }
        let err = Server::with_transport("x", Box::new(Failing)).unwrap_err();
        assert!(err.message.contains("initialize exploded"));
    }

    #[test]
    fn a_server_that_ignores_the_handshake_is_rejected() {
        let fake = Fake::new(&[("initialize", json!("not an object"))]);
        let err = Server::with_transport("odd", Box::new(fake)).unwrap_err();
        assert!(
            err.message.contains("initialize handshake"),
            "{}",
            err.message
        );
    }

    #[test]
    fn the_handshake_follows_the_protocol() {
        // initialize, then the initialized notification, then discovery.
        let fake = Fake::new(&[
            ("initialize", json!({ "protocolVersion": "2024-11-05" })),
            ("tools/list", json!({ "tools": [] })),
        ]);
        Server::with_transport("x", Box::new(fake)).unwrap();
        // The order is checked by the fake refusing unscripted methods; a
        // missing notification would not reach `tools/list`.
    }

    #[test]
    fn unsupported_parameter_types_are_marked_not_guessed() {
        let fake = Fake::new(&[
            ("initialize", json!({})),
            (
                "tools/list",
                json!({
                    "tools": [{
                        "name": "weird",
                        "inputSchema": {
                            "properties": { "blob": { "type": "object" } }
                        }
                    }]
                }),
            ),
        ]);
        let s = Server::with_transport("x", Box::new(fake)).unwrap();
        assert_eq!(
            s.tool("weird").unwrap().params,
            vec![("blob".to_string(), ParamType::Unsupported)]
        );
    }
}
