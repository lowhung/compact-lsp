#![cfg(unix)]

use std::collections::HashSet;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

struct LspHarness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    registration: Option<Value>,
}

impl LspHarness {
    async fn start(compiler: &Path) -> Self {
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

    async fn send(&mut self, message: Value) {
        let body = serde_json::to_vec(&message).expect("valid JSON-RPC message");
        self.stdin
            .write_all(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes())
            .await
            .expect("write LSP header");
        self.stdin.write_all(&body).await.expect("write LSP body");
        self.stdin.flush().await.expect("flush LSP message");
    }

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

    async fn notify(&mut self, method: &str, params: Value) {
        self.send(json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params
        }))
        .await;
    }

    async fn request(&mut self, method: &str, params: Value) -> Value {
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

    async fn wait_until_ready(&mut self) {
        loop {
            let message = self.next_message().await;
            if message.get("method").and_then(Value::as_str) == Some("window/logMessage")
                && message["params"]["message"] == "Compact LSP server ready"
            {
                return;
            }
        }
    }

    async fn completion_labels(&mut self, uri: &str) -> HashSet<String> {
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

    async fn wait_for_completion(
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

    async fn workspace_symbols(&mut self, query: &str) -> Vec<Value> {
        self.request("workspace/symbol", json!({ "query": query }))
            .await
            .as_array()
            .cloned()
            .expect("workspace symbol array")
    }

    async fn wait_for_workspace_symbol(
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

    async fn shutdown(mut self) {
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

fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("absolute path")
        .to_string()
}

#[tokio::test]
async fn multi_root_index_tracks_compact_file_lifecycle() {
    tokio::time::timeout(Duration::from_secs(20), run_multi_root_file_lifecycle())
        .await
        .expect("workspace protocol test timed out");
}

async fn run_multi_root_file_lifecycle() {
    let temporary = tempfile::tempdir().unwrap();
    let root_a = temporary.path().join("Root A");
    let root_b = temporary.path().join("Root B");
    std::fs::create_dir_all(&root_a).unwrap();
    std::fs::create_dir_all(&root_b).unwrap();

    let compiler = temporary.path().join("compactc");
    std::fs::write(&compiler, "#!/bin/sh\nexit 1\n").unwrap();
    let mut permissions = std::fs::metadata(&compiler).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&compiler, permissions).unwrap();

    let main_path = root_a.join("Main.compact");
    let new_path = root_a.join("New.compact");
    let other_path = root_b.join("Other.compact");
    let user_path = root_b.join("User.compact");
    let main_source =
        "import \"./Utility\";\nimport \"./New\";\ncircuit main(): Field { return utility(); }";
    let user_source = "import \"./Other\";\ncircuit user(): Field { return other(); }";
    let other_source = "circuit other(): Field { return 2; }";
    std::fs::write(
        root_a.join("Utility.compact"),
        "circuit utility(): Field { return 1; }",
    )
    .unwrap();
    std::fs::write(&main_path, main_source).unwrap();
    std::fs::write(&other_path, other_source).unwrap();
    std::fs::write(&user_path, user_source).unwrap();
    std::fs::write(
        root_a.join("Unicode.compact"),
        "/*😀*/ circuit unicode_name(): Field { return 5; }",
    )
    .unwrap();

    let root_a_uri = file_uri(&root_a);
    let root_b_uri = file_uri(&root_b);
    let main_uri = file_uri(&main_path);
    let new_uri = file_uri(&new_path);
    let other_uri = file_uri(&other_path);
    let user_uri = file_uri(&user_path);
    let mut lsp = LspHarness::start(&compiler).await;

    let initialize = lsp
        .request(
            "initialize",
            json!({
                "processId": null,
                "capabilities": {
                    "workspace": {
                        "workspaceFolders": true,
                        "didChangeWatchedFiles": { "dynamicRegistration": true }
                    }
                },
                "workspaceFolders": [
                    { "uri": root_a_uri, "name": "Root A" },
                    { "uri": root_b_uri, "name": "Root B" }
                ],
                "rootUri": null
            }),
        )
        .await;
    assert_eq!(
        initialize["capabilities"]["workspace"]["workspaceFolders"]["supported"],
        true
    );
    assert_eq!(
        initialize["capabilities"]["codeActionProvider"]["codeActionKinds"],
        json!(["quickfix"])
    );
    assert_eq!(
        initialize["capabilities"]["textDocumentSync"]["change"], 2,
        "the server should negotiate incremental document synchronization"
    );
    assert_eq!(
        initialize["capabilities"]["workspaceSymbolProvider"]["resolveProvider"],
        false
    );

    lsp.notify("initialized", json!({})).await;
    lsp.wait_until_ready().await;
    assert_eq!(
        lsp.registration.as_ref().unwrap()["registrations"][0]["method"],
        "workspace/didChangeWatchedFiles"
    );
    assert_eq!(
        lsp.registration.as_ref().unwrap()["registrations"][0]["registerOptions"]["watchers"][0]
            ["globPattern"],
        "**/*.compact"
    );

    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": main_uri,
                "languageId": "compact",
                "version": 1,
                "text": main_source
            }
        }),
    )
    .await;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": user_uri,
                "languageId": "compact",
                "version": 1,
                "text": user_source
            }
        }),
    )
    .await;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": other_uri,
                "languageId": "compact",
                "version": 1,
                "text": other_source
            }
        }),
    )
    .await;

    assert!(lsp.completion_labels(&user_uri).await.contains("other"));
    assert!(!lsp.completion_labels(&main_uri).await.contains("fresh"));
    let initial_symbols = lsp.workspace_symbols("").await;
    let initial_names: Vec<_> = initial_symbols
        .iter()
        .filter_map(|symbol| symbol["name"].as_str())
        .collect();
    assert!(
        initial_names.windows(2).all(|names| names[0] <= names[1]),
        "empty workspace queries should be deterministically sorted"
    );
    let unicode_symbol = initial_symbols
        .iter()
        .find(|symbol| symbol["name"] == "unicode_name")
        .expect("indexed Unicode-adjacent symbol");
    assert_eq!(
        unicode_symbol["location"]["range"]["start"]["character"], 7,
        "workspace symbol locations should use UTF-16 columns"
    );
    let code_actions = lsp
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": main_uri },
                "range": {
                    "start": { "line": 2, "character": 66 },
                    "end": { "line": 2, "character": 66 }
                },
                "context": {
                    "diagnostics": [{
                        "range": {
                            "start": { "line": 2, "character": 66 },
                            "end": { "line": 2, "character": 66 }
                        },
                        "severity": 1,
                        "source": "compact-syntax",
                        "message": "Syntax error: missing ;"
                    }],
                    "only": ["quickfix"],
                    "triggerKind": 1
                }
            }),
        )
        .await;
    assert_eq!(code_actions[0]["title"], "Insert missing `;`");
    assert_eq!(code_actions[0]["kind"], "quickfix");
    assert_eq!(
        code_actions[0]["edit"]["changes"][&main_uri][0]["newText"],
        ";"
    );
    let user_last_line_length = user_source.lines().last().unwrap().encode_utf16().count();
    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": {
                "uri": user_uri,
                "version": 2
            },
            "contentChanges": [{
                "range": {
                    "start": { "line": 1, "character": user_last_line_length },
                    "end": { "line": 1, "character": user_last_line_length }
                },
                "rangeLength": 0,
                "text": "\ncircuit added(): Field { return 3; }"
            }]
        }),
    )
    .await;
    assert!(lsp
        .wait_for_completion(&user_uri, "added", true)
        .await
        .contains("added"));
    assert!(lsp
        .wait_for_workspace_symbol("ADD", "added", true)
        .await
        .iter()
        .any(|symbol| symbol["containerName"] == "User.compact"));
    let references = lsp
        .request(
            "textDocument/references",
            json!({
                "textDocument": { "uri": other_uri },
                "position": { "line": 0, "character": 9 },
                "context": { "includeDeclaration": true }
            }),
        )
        .await;
    assert_eq!(
        references.as_array().expect("reference locations").len(),
        2,
        "canonical and editor URI spellings must not duplicate indexed references"
    );

    std::fs::write(&new_path, "circuit fresh(): Field { return 3; }").unwrap();
    lsp.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": new_uri, "type": 1 }] }),
    )
    .await;
    assert!(lsp
        .wait_for_completion(&main_uri, "fresh", true)
        .await
        .contains("fresh"));
    let canonical_new_uri = file_uri(&new_path.canonicalize().unwrap());
    let fresh_symbols = lsp.wait_for_workspace_symbol("fresh", "fresh", true).await;
    assert!(
        fresh_symbols
            .iter()
            .any(|symbol| symbol["location"]["uri"] == canonical_new_uri),
        "unexpected workspace symbols: {fresh_symbols:#?}; expected URI {canonical_new_uri}"
    );

    std::fs::write(&new_path, "circuit newer(): Field { return 4; }").unwrap();
    lsp.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": new_uri, "type": 2 }] }),
    )
    .await;
    let changed = lsp.wait_for_completion(&main_uri, "newer", true).await;
    assert!(!changed.contains("fresh"));
    assert!(!lsp
        .wait_for_workspace_symbol("fresh", "fresh", false)
        .await
        .iter()
        .any(|symbol| symbol["name"] == "fresh"));
    assert!(lsp
        .wait_for_workspace_symbol("newer", "newer", true)
        .await
        .iter()
        .any(|symbol| symbol["name"] == "newer"));

    std::fs::remove_file(&new_path).unwrap();
    lsp.notify(
        "workspace/didChangeWatchedFiles",
        json!({ "changes": [{ "uri": new_uri, "type": 3 }] }),
    )
    .await;
    assert!(!lsp
        .wait_for_completion(&main_uri, "newer", false)
        .await
        .contains("newer"));
    assert!(!lsp
        .wait_for_workspace_symbol("newer", "newer", false)
        .await
        .iter()
        .any(|symbol| symbol["name"] == "newer"));

    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main_uri } }),
    )
    .await;
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": user_uri } }),
    )
    .await;
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": other_uri } }),
    )
    .await;
    lsp.shutdown().await;
}
