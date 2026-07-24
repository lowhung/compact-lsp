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

    async fn wait_for_diagnostic(
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

async fn wait_for_pid_file(path: &Path) -> String {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if let Ok(pid) = std::fs::read_to_string(path) {
                if !pid.trim().is_empty() {
                    return pid;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("mock compiler did not start")
}

async fn wait_for_process_exit(pid: &str) {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let status = std::process::Command::new("kill")
                .args(["-0", pid.trim()])
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .expect("check mock compiler process");
            if !status.success() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("cancelled compiler process remained alive");
}

#[tokio::test]
async fn semantic_diagnostics_cancel_stale_close_and_shutdown_work() {
    tokio::time::timeout(Duration::from_secs(30), run_diagnostic_cancellation())
        .await
        .expect("diagnostic cancellation test timed out");
}

async fn run_diagnostic_cancellation() {
    let temporary = tempfile::tempdir().unwrap();
    let pid_file = temporary.path().join("compiler-pid");
    let compiler = temporary.path().join("compactc");
    let script = format!(
        r#"#!/bin/sh
case "$1" in
  --version) printf '0.33.0\n'; exit 0 ;;
  --language-version) printf '0.25.0\n'; exit 0 ;;
esac
content=$(cat "$3")
case "$content" in
  *slow*)
    printf '%s' "$$" > "{}"
    sleep 30
    printf 'Exception: %s line 1 char 1: stale type error\n' "$3"
    ;;
  *fresh*)
    printf 'Exception: %s line 1 char 1: fresh type error\n' "$3"
    ;;
esac
"#,
        pid_file.display()
    );
    std::fs::write(&compiler, script).unwrap();
    let mut permissions = std::fs::metadata(&compiler).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&compiler, permissions).unwrap();

    let source_path = temporary.path().join("Main.compact");
    let initial_source = "/* fresh */ export circuit main(): Field { return 0; }";
    std::fs::write(&source_path, initial_source).unwrap();
    let uri = file_uri(&source_path);
    let root_uri = file_uri(temporary.path());
    let mut lsp = LspHarness::start(&compiler).await;
    lsp.request(
        "initialize",
        json!({
            "processId": null,
            "capabilities": {},
            "workspaceFolders": [{ "uri": root_uri, "name": "diagnostics" }],
            "rootUri": null
        }),
    )
    .await;
    lsp.notify("initialized", json!({})).await;
    lsp.wait_until_ready().await;
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "compact",
                "version": 1,
                "text": initial_source
            }
        }),
    )
    .await;
    lsp.wait_for_diagnostic(&uri, 1, "fresh type error").await;

    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "text": "/* slow */ export circuit main(): Field { return 1; }"
            }]
        }),
    )
    .await;
    let stale_pid = wait_for_pid_file(&pid_file).await;
    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 3 },
            "contentChanges": [{
                "text": "/* fresh */ export circuit main(): Field { return 2; }"
            }]
        }),
    )
    .await;
    let diagnostic = lsp.wait_for_diagnostic(&uri, 3, "fresh type error").await;
    assert_eq!(diagnostic["params"]["diagnostics"][0]["source"], "compactc");
    wait_for_process_exit(&stale_pid).await;

    let _ = std::fs::remove_file(&pid_file);
    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 4 },
            "contentChanges": [{
                "text": "/* slow close */ export circuit main(): Field { return 3; }"
            }]
        }),
    )
    .await;
    let close_pid = wait_for_pid_file(&pid_file).await;
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": uri } }),
    )
    .await;
    wait_for_process_exit(&close_pid).await;

    let _ = std::fs::remove_file(&pid_file);
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": uri,
                "languageId": "compact",
                "version": 1,
                "text": initial_source
            }
        }),
    )
    .await;
    lsp.notify(
        "textDocument/didChange",
        json!({
            "textDocument": { "uri": uri, "version": 2 },
            "contentChanges": [{
                "text": "/* slow shutdown */ export circuit main(): Field { return 4; }"
            }]
        }),
    )
    .await;
    let shutdown_pid = wait_for_pid_file(&pid_file).await;
    lsp.shutdown().await;
    wait_for_process_exit(&shutdown_pid).await;
}

#[tokio::test]
async fn multi_root_index_tracks_compact_file_lifecycle() {
    tokio::time::timeout(Duration::from_secs(20), run_multi_root_file_lifecycle())
        .await
        .expect("workspace protocol test timed out");
}

#[tokio::test]
async fn inlay_hints_load_imports_without_a_workspace_index() {
    tokio::time::timeout(Duration::from_secs(20), run_cold_import_inlay_hints())
        .await
        .expect("cold import inlay-hint test timed out");
}

async fn run_cold_import_inlay_hints() {
    let temporary = tempfile::tempdir().unwrap();
    let compiler = temporary.path().join("compactc");
    std::fs::write(&compiler, "#!/bin/sh\nexit 1\n").unwrap();
    let mut permissions = std::fs::metadata(&compiler).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&compiler, permissions).unwrap();

    let main_path = temporary.path().join("Main.compact");
    let main_source = "\
import \"./Utility\" prefix Utils_;
circuit main(): Field { return Utils_scale(3, 4); }";
    std::fs::write(&main_path, main_source).unwrap();
    std::fs::write(
        temporary.path().join("Utility.compact"),
        "circuit scale(value: Field, factor: Field): Field { return value * factor; }",
    )
    .unwrap();
    let main_uri = file_uri(&main_path);
    let mut lsp = LspHarness::start(&compiler).await;

    lsp.request(
        "initialize",
        json!({
            "processId": null,
            "capabilities": {},
            "workspaceFolders": null,
            "rootUri": null
        }),
    )
    .await;
    lsp.notify("initialized", json!({})).await;
    lsp.wait_until_ready().await;
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

    let hints = lsp
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": main_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 3, "character": 0 }
                }
            }),
        )
        .await;
    assert_eq!(
        hints
            .as_array()
            .expect("imported inlay hints")
            .iter()
            .filter_map(|hint| hint["label"].as_str())
            .collect::<Vec<_>>(),
        vec!["value:", "factor:"],
        "the first hint request should load a missing imported file on demand"
    );

    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main_uri } }),
    )
    .await;
    lsp.shutdown().await;
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
    let highlight_path = root_a.join("Highlight.compact");
    let hints_path = root_a.join("Hints.compact");
    let main_source =
        "import \"./Utility\";\nimport \"./New\";\ncircuit main(): Field { return utility(); }";
    let user_source = "import \"./Other\";\ncircuit user(): Field { return other(); }";
    let other_source = "circuit other(): Field { return 2; }";
    let highlight_source = "\
/*😀*/ circuit target(): Field { return 1; }
circuit caller(): Field { return target(); }
circuit unresolved_user(): Field { return missing(); }";
    let hints_source = "\
import \"./HintUtility\" prefix Hint_;
ledger rounds: Counter;
circuit local(left: Field, right: Field): Field { return left + right; }
circuit duplicate(one: Field): Field { return one; }
circuit duplicate(two: Field): Field { return two; }
circuit hints(value: Field): Field {
    local(1, 2);
    Hint_utility(3, 4);
    transientCommit(5, 6);
    rounds.increment(7);
    local(9);
    duplicate(10);
    return value;
}
circuit forward(left: Field): Field {
    local(left, 8);
    return left;
}";
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
    std::fs::write(&highlight_path, highlight_source).unwrap();
    std::fs::write(
        root_a.join("HintUtility.compact"),
        "circuit utility(source: Field, factor: Field): Field { return source * factor; }",
    )
    .unwrap();
    std::fs::write(&hints_path, hints_source).unwrap();

    let root_a_uri = file_uri(&root_a);
    let root_b_uri = file_uri(&root_b);
    let main_uri = file_uri(&main_path);
    let new_uri = file_uri(&new_path);
    let other_uri = file_uri(&other_path);
    let user_uri = file_uri(&user_path);
    let highlight_uri = file_uri(&highlight_path);
    let hints_uri = file_uri(&hints_path);
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
    assert_eq!(
        initialize["capabilities"]["documentHighlightProvider"],
        true
    );
    assert_eq!(
        initialize["capabilities"]["inlayHintProvider"]["resolveProvider"],
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
                "uri": hints_uri,
                "languageId": "compact",
                "version": 1,
                "text": hints_source
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
    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": highlight_uri,
                "languageId": "compact",
                "version": 1,
                "text": highlight_source
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
    let highlights = lsp
        .request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": { "uri": highlight_uri },
                "position": { "line": 0, "character": 15 }
            }),
        )
        .await;
    assert_eq!(
        highlights,
        json!([
            {
                "range": {
                    "start": { "line": 0, "character": 15 },
                    "end": { "line": 0, "character": 21 }
                },
                "kind": 3
            },
            {
                "range": {
                    "start": { "line": 1, "character": 33 },
                    "end": { "line": 1, "character": 39 }
                },
                "kind": 2
            }
        ])
    );
    assert_eq!(
        lsp.request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": { "uri": highlight_uri },
                "position": { "line": 0, "character": 8 }
            }),
        )
        .await,
        Value::Null,
        "keywords should not produce document highlights"
    );
    assert_eq!(
        lsp.request(
            "textDocument/documentHighlight",
            json!({
                "textDocument": { "uri": highlight_uri },
                "position": { "line": 2, "character": 44 }
            }),
        )
        .await,
        Value::Null,
        "unresolved symbols should not produce document highlights"
    );
    let inlay_hints = lsp
        .request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": hints_uri },
                "range": {
                    "start": { "line": 0, "character": 0 },
                    "end": { "line": 100, "character": 0 }
                }
            }),
        )
        .await;
    let inlay_hints = inlay_hints.as_array().expect("inlay hint array");
    assert_eq!(
        inlay_hints
            .iter()
            .filter_map(|hint| hint["label"].as_str())
            .collect::<Vec<_>>(),
        vec!["left:", "right:", "source:", "factor:", "value:", "rand:", "amount:", "right:"],
        "resolved calls should be hinted while wrong-arity and ambiguous calls are omitted"
    );
    assert!(inlay_hints
        .iter()
        .all(|hint| hint["kind"] == 2 && hint["paddingRight"] == true));
    assert_eq!(
        lsp.request(
            "textDocument/inlayHint",
            json!({
                "textDocument": { "uri": hints_uri },
                "range": {
                    "start": { "line": 6, "character": 0 },
                    "end": { "line": 7, "character": 0 }
                }
            }),
        )
        .await
        .as_array()
        .expect("range-filtered inlay hints")
        .len(),
        2,
        "the server should only return hints inside the requested range"
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
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": highlight_uri } }),
    )
    .await;
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": hints_uri } }),
    )
    .await;
    lsp.shutdown().await;
}
