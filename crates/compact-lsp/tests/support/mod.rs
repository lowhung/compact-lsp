//! Shared process-level JSON-RPC harness for `compact-lsp` integration tests.

use std::collections::HashSet;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

/// A running `compact-lsp` process connected through standard LSP framing.
pub(crate) struct LspHarness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    /// The latest dynamic capability registration received from the server.
    pub(crate) registration: Option<Value>,
}

impl LspHarness {
    /// Start Cargo's exact integration-test server binary with an explicit compiler.
    ///
    /// Tests may pass a deliberately missing path when they only need parser and
    /// protocol behavior. This prevents a developer's globally installed Compact
    /// toolchain from making a supposedly hermetic test behave differently.
    pub(crate) async fn start(compiler: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_compact-lsp"));
        command
            .env("COMPACT_COMPILER", compiler)
            .env("RUST_LOG", "compact_lsp=debug")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = command.spawn().expect("language server should start");
        let stdin = child.stdin.take().expect("language server stdin");
        let stdout = BufReader::new(child.stdout.take().expect("language server stdout"));
        let mut stderr = child.stderr.take().expect("language server stderr");
        tokio::spawn(async move {
            let mut output = Vec::new();
            let _ = stderr.read_to_end(&mut output).await;
        });

        Self {
            child,
            stdin,
            stdout,
            next_id: 1,
            registration: None,
        }
    }

    /// Write one JSON-RPC message using Language Server Protocol framing.
    async fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("valid JSON-RPC message");
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .expect("write LSP header");
        self.stdin.write_all(&body).await.expect("write LSP body");
        self.stdin.flush().await.expect("flush LSP message");
    }

    /// Read one complete LSP-framed JSON-RPC message from the server.
    async fn read(&mut self) -> Value {
        let mut content_length = None;
        loop {
            let mut header = String::new();
            let bytes = self
                .stdout
                .read_line(&mut header)
                .await
                .expect("read LSP header");
            assert!(
                bytes > 0,
                "language server closed before sending a response"
            );
            if header == "\r\n" {
                break;
            }
            if let Some(length) = header.strip_prefix("Content-Length:") {
                content_length = Some(
                    length
                        .trim()
                        .parse::<usize>()
                        .expect("numeric Content-Length"),
                );
            }
        }

        let mut body = vec![0; content_length.expect("Content-Length header")];
        self.stdout
            .read_exact(&mut body)
            .await
            .expect("read LSP body");
        serde_json::from_slice(&body).expect("valid JSON-RPC response")
    }

    /// Read the next message while acknowledging dynamic registrations.
    async fn next_message(&mut self) -> Value {
        loop {
            let message = self.read().await;
            if message.get("method").and_then(Value::as_str) == Some("client/registerCapability") {
                self.registration = message.get("params").cloned();
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": null
                }))
                .await;
                continue;
            }
            return message;
        }
    }

    /// Send a JSON-RPC notification that does not expect a response.
    pub(crate) async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await;
    }

    /// Send a request and wait for its matching response.
    ///
    /// Unrelated server notifications are consumed while waiting. Dynamic
    /// registrations are acknowledged by [`Self::next_message`].
    pub(crate) async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id;
        self.next_id += 1;
        self.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params
        }))
        .await;

        loop {
            let message = self.next_message().await;
            if message.get("id").and_then(Value::as_u64) == Some(id) {
                assert!(
                    message.get("error").is_none(),
                    "LSP request {method} failed: {message}"
                );
                return message.get("result").cloned().unwrap_or(Value::Null);
            }
        }
    }

    /// Wait for the server's explicit post-initialization ready message.
    pub(crate) async fn wait_until_ready(&mut self) {
        loop {
            let message = self.next_message().await;
            if message.get("method").and_then(Value::as_str) == Some("window/logMessage")
                && message["params"]["message"] == "Compact LSP server ready"
            {
                return;
            }
        }
    }

    /// Wait for one versioned diagnostic publication containing an exact message.
    pub(crate) async fn wait_for_diagnostic(
        &mut self,
        uri: &str,
        version: i64,
        expected_message: &str,
    ) -> Value {
        tokio::time::timeout(Duration::from_secs(8), async {
            loop {
                let message = self.next_message().await;
                if message.get("method").and_then(Value::as_str)
                    != Some("textDocument/publishDiagnostics")
                    || message["params"]["uri"].as_str() != Some(uri)
                    || message["params"]["version"].as_i64() != Some(version)
                {
                    continue;
                }
                if message["params"]["diagnostics"]
                    .as_array()
                    .is_some_and(|diagnostics| {
                        diagnostics
                            .iter()
                            .any(|diagnostic| diagnostic["message"] == expected_message)
                    })
                {
                    return message;
                }
            }
        })
        .await
        .expect("expected diagnostics were not published")
    }

    /// Return all completion labels visible at the beginning of a document.
    pub(crate) async fn completion_labels(&mut self, uri: &str) -> HashSet<String> {
        let result = self
            .request(
                "textDocument/completion",
                json!({
                    "textDocument": { "uri": uri },
                    "position": { "line": 0, "character": 0 },
                    "context": { "triggerKind": 1 }
                }),
            )
            .await;
        completion_labels(&result)
    }

    /// Poll completion until a workspace-indexed label reaches the expected state.
    pub(crate) async fn wait_for_completion(
        &mut self,
        uri: &str,
        expected: &str,
        should_exist: bool,
    ) -> HashSet<String> {
        for _ in 0..20 {
            let labels = self.completion_labels(uri).await;
            if labels.contains(expected) == should_exist {
                return labels;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("completion item {expected:?} did not reach expected state {should_exist}");
    }

    /// Request workspace symbols and normalize the response to an array.
    pub(crate) async fn workspace_symbols(&mut self, query: &str) -> Vec<Value> {
        self.request("workspace/symbol", json!({ "query": query }))
            .await
            .as_array()
            .cloned()
            .expect("workspace symbol array")
    }

    /// Poll workspace symbols until an indexed declaration reaches the expected state.
    pub(crate) async fn wait_for_workspace_symbol(
        &mut self,
        query: &str,
        expected: &str,
        should_exist: bool,
    ) -> Vec<Value> {
        for _ in 0..20 {
            let symbols = self.workspace_symbols(query).await;
            let exists = symbols
                .iter()
                .any(|symbol| symbol["name"].as_str() == Some(expected));
            if exists == should_exist {
                return symbols;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("workspace symbol {expected:?} did not reach expected state {should_exist}");
    }

    /// Perform the LSP shutdown handshake and require a clean process exit.
    pub(crate) async fn shutdown(mut self) {
        assert_eq!(self.request("shutdown", Value::Null).await, Value::Null);
        self.notify("exit", Value::Null).await;
        self.stdin
            .shutdown()
            .await
            .expect("close language server stdin");
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("language server should stop")
            .expect("wait for language server");
        assert!(status.success());
    }
}

/// Collect labels from either an LSP completion array or completion-list object.
pub(crate) fn completion_labels(result: &Value) -> HashSet<String> {
    let items = result
        .as_array()
        .cloned()
        .or_else(|| result.get("items").and_then(Value::as_array).cloned())
        .expect("completion array");
    items
        .into_iter()
        .filter_map(|item| item.get("label").and_then(Value::as_str).map(str::to_owned))
        .collect()
}

/// Convert an absolute local path to a standards-compliant file URI.
pub(crate) fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("absolute path")
        .to_string()
}
