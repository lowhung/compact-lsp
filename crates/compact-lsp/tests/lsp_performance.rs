#![cfg(unix)]

//! End-to-end performance guard for a representative generated Compact workspace.
//!
//! These thresholds deliberately catch order-of-magnitude regressions instead of
//! comparing microsecond-level timings, which would make CI sensitive to runner load.

use std::collections::HashMap;
use std::env;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, ChildStdout, Command};

const WORKSPACE_FILES: usize = 300;
const TOTAL_LIMIT: Duration = Duration::from_secs(45);
const STARTUP_LIMIT: Duration = Duration::from_secs(15);
const INDEXING_REQUEST_LIMIT: Duration = Duration::from_secs(5);
const INTERACTIVE_REQUEST_LIMIT: Duration = Duration::from_secs(2);
const RENAME_LIMIT: Duration = Duration::from_secs(4);
const DIAGNOSTIC_LIMIT: Duration = Duration::from_secs(5);
const RSS_SCENARIO_LIMIT: Duration = Duration::from_secs(5 * 60);
const RSS_WORKSPACE_FILES: usize = 1_000;

struct LspHarness {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    next_id: u64,
    ready: bool,
    diagnostic_times: HashMap<String, Instant>,
}

impl LspHarness {
    /// Start the packaged test binary and drain stderr so verbose logging cannot
    /// fill the child pipe and stall the JSON-RPC transport.
    async fn start(compiler: &Path) -> Self {
        let mut command = Command::new(env!("CARGO_BIN_EXE_compact-lsp"));
        command
            .env("COMPACT_COMPILER", compiler)
            .env("RUST_LOG", "compact_lsp=info")
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
            ready: false,
            diagnostic_times: HashMap::new(),
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

    /// Consume server-initiated registration requests and remember readiness
    /// notifications even when they arrive while another response is awaited.
    async fn next_message(&mut self) -> Value {
        loop {
            let message = self.read().await;
            if message.get("method").and_then(Value::as_str) == Some("client/registerCapability") {
                self.send(json!({
                    "jsonrpc": "2.0",
                    "id": message["id"],
                    "result": null
                }))
                .await;
                continue;
            }
            if message.get("method").and_then(Value::as_str) == Some("window/logMessage")
                && message["params"]["message"] == "Compact LSP server ready"
            {
                self.ready = true;
            }
            if message.get("method").and_then(Value::as_str)
                == Some("textDocument/publishDiagnostics")
            {
                if let Some(uri) = message["params"]["uri"].as_str() {
                    self.diagnostic_times
                        .insert(uri.to_string(), Instant::now());
                }
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
        while !self.ready {
            self.next_message().await;
        }
    }

    /// Wait for the semantic diagnostic pass for `uri`, ignoring unrelated
    /// notifications that may still be in flight after workspace startup.
    async fn wait_for_diagnostics(&mut self, uri: &str) -> Instant {
        while !self.diagnostic_times.contains_key(uri) {
            self.next_message().await;
        }
        self.diagnostic_times[uri]
    }

    async fn shutdown(mut self) {
        assert_eq!(self.request("shutdown", Value::Null).await, Value::Null);
        self.notify("exit", Value::Null).await;
        self.stdin.shutdown().await.expect("close server stdin");
        let status = tokio::time::timeout(Duration::from_secs(5), self.child.wait())
            .await
            .expect("language server should stop")
            .expect("wait for language server");
        assert!(status.success());
    }

    /// Read the child server's resident set size through the POSIX `ps`
    /// interface shared by macOS and Linux.
    async fn resident_mib(&self) -> f64 {
        let process_id = self.child.id().expect("running language server");
        let output = Command::new("ps")
            .args(["-o", "rss=", "-p", &process_id.to_string()])
            .output()
            .await
            .expect("run ps for language server");
        assert!(
            output.status.success(),
            "ps failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let resident_kib = String::from_utf8(output.stdout)
            .expect("ps output should be UTF-8")
            .trim()
            .parse::<u64>()
            .expect("ps should report numeric RSS");
        resident_kib as f64 / 1_024.0
    }

    /// Forget an earlier diagnostic notification before waiting for a newer
    /// document version in the RSS churn loop.
    fn clear_diagnostics(&mut self, uri: &str) {
        self.diagnostic_times.remove(uri);
    }
}

fn file_uri(path: &Path) -> String {
    url::Url::from_file_path(path)
        .expect("absolute path")
        .to_string()
}

/// Generate enough independent declarations and import edges to exercise file
/// discovery, parsing, symbol caching, reverse dependencies, and JSON-RPC result
/// serialization without depending on a checked-in third-party contract corpus.
fn generate_workspace(root: &Path, file_count: usize, symbols_per_file: usize) {
    for index in 0..file_count {
        let next = (index + 1) % file_count;
        let mut source = format!(
            "import \"./Contract{next:04}\" prefix Next_;\n\
             circuit symbol_{index:04}(value: Field): Field {{ return value; }}\n\
             circuit caller_{index:04}(): Field {{ return symbol_{index:04}(1); }}\n"
        );
        for symbol in 2..symbols_per_file {
            source.push_str(&format!(
                "circuit extra_{index:04}_{symbol:03}(value: Field): Field {{ return value; }}\n"
            ));
        }
        std::fs::write(root.join(format!("Contract{index:04}.compact")), source).unwrap();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn large_workspace_remains_responsive() {
    tokio::time::timeout(TOTAL_LIMIT, run_large_workspace_benchmark())
        .await
        .expect("large-workspace performance guard timed out");
}

async fn run_large_workspace_benchmark() {
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    generate_workspace(&workspace, WORKSPACE_FILES, 2);

    let compiler = temporary.path().join("compactc");
    std::fs::write(&compiler, "#!/bin/sh\nexit 0\n").unwrap();
    let mut permissions = std::fs::metadata(&compiler).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&compiler, permissions).unwrap();

    let root_uri = file_uri(&workspace);
    let main_path = workspace.join("Contract0000.compact");
    let main_uri = file_uri(&main_path);
    let main_source = std::fs::read_to_string(&main_path).unwrap();
    let mut lsp = LspHarness::start(&compiler).await;

    lsp.request(
        "initialize",
        json!({
            "processId": null,
            "capabilities": {},
            "workspaceFolders": [{ "uri": root_uri, "name": "generated" }],
            "rootUri": null
        }),
    )
    .await;

    let startup_started = Instant::now();
    lsp.notify("initialized", json!({})).await;

    let indexing_request_started = Instant::now();
    let _ = lsp
        .request("workspace/symbol", json!({ "query": "symbol_0000" }))
        .await;
    let indexing_request = indexing_request_started.elapsed();
    assert!(
        indexing_request < INDEXING_REQUEST_LIMIT,
        "request during indexing took {indexing_request:?}, limit {INDEXING_REQUEST_LIMIT:?}"
    );
    assert!(
        !lsp.ready,
        "generated workspace indexed before the responsiveness probe completed"
    );

    lsp.wait_until_ready().await;
    let startup = startup_started.elapsed();
    assert!(
        startup < STARTUP_LIMIT,
        "workspace startup took {startup:?}, limit {STARTUP_LIMIT:?}"
    );

    let diagnostics_started = Instant::now();
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

    let completion_started = Instant::now();
    let completion = lsp
        .request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 2, "character": 48 },
                "context": { "triggerKind": 1 }
            }),
        )
        .await;
    let completion_latency = completion_started.elapsed();
    assert!(
        completion_latency < INTERACTIVE_REQUEST_LIMIT,
        "completion took {completion_latency:?}, limit {INTERACTIVE_REQUEST_LIMIT:?}"
    );
    assert!(completion
        .as_array()
        .expect("completion items")
        .iter()
        .any(|item| item["label"] == "symbol_0000"));

    let definition_started = Instant::now();
    let definition = lsp
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 2, "character": 41 }
            }),
        )
        .await;
    let definition_latency = definition_started.elapsed();
    assert!(
        definition_latency < INTERACTIVE_REQUEST_LIMIT,
        "definition took {definition_latency:?}, limit {INTERACTIVE_REQUEST_LIMIT:?}"
    );
    assert!(!definition.is_null(), "local definition should resolve");

    let rename_started = Instant::now();
    let rename = lsp
        .request(
            "textDocument/rename",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 1, "character": 10 },
                "newName": "renamed_symbol"
            }),
        )
        .await;
    let rename_latency = rename_started.elapsed();
    assert!(
        rename_latency < RENAME_LIMIT,
        "rename took {rename_latency:?}, limit {RENAME_LIMIT:?}"
    );
    assert!(
        rename["changes"][&main_uri]
            .as_array()
            .is_some_and(|edits| edits.len() >= 2),
        "rename should update the declaration and local call: {rename}"
    );

    let diagnostics_received = lsp.wait_for_diagnostics(&main_uri).await;
    let diagnostic_latency = diagnostics_received.saturating_duration_since(diagnostics_started);
    assert!(
        diagnostic_latency < DIAGNOSTIC_LIMIT,
        "diagnostics took {diagnostic_latency:?}, limit {DIAGNOSTIC_LIMIT:?}"
    );

    eprintln!(
        "compact-lsp benchmark: files={WORKSPACE_FILES} startup={startup:?} \
         during_index={indexing_request:?} completion={completion_latency:?} \
         definition={definition_latency:?} rename={rename_latency:?} \
         diagnostics={diagnostic_latency:?}"
    );

    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main_uri } }),
    )
    .await;
    lsp.shutdown().await;
}

/// Measure RSS without turning one machine's allocator behavior into a CI
/// promise. Run this ignored test explicitly in release mode as documented in
/// `docs/performance.md`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "manual RSS benchmark; see docs/performance.md"]
async fn resident_memory_benchmark() {
    let mut scenarios = vec![(300, 2, false), (RSS_WORKSPACE_FILES, 2, true)];
    if env::var_os("COMPACT_LSP_RSS_FULL").is_some() {
        scenarios = vec![
            (1, 2, false),
            (300, 2, false),
            (RSS_WORKSPACE_FILES, 2, true),
            (3_000, 2, false),
            (10_000, 2, false),
            (RSS_WORKSPACE_FILES, 20, false),
        ];
    }

    for (files, symbols_per_file, stress) in scenarios {
        tokio::time::timeout(
            RSS_SCENARIO_LIMIT,
            run_rss_scenario(files, symbols_per_file, stress),
        )
        .await
        .unwrap_or_else(|_| {
            panic!(
                "RSS scenario timed out after {RSS_SCENARIO_LIMIT:?}: \
                 files={files} symbols_per_file={symbols_per_file}"
            )
        });
    }
}

/// Run one generated workspace shape and report readiness, peak, and settled
/// RSS. Only the representative 1,000-file shape receives request and document
/// churn so the scaling scenarios remain quick.
async fn run_rss_scenario(files: usize, symbols_per_file: usize, stress: bool) {
    let platform = env::consts::OS;
    let architecture = env::consts::ARCH;
    let build_profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    let temporary = tempfile::tempdir().unwrap();
    let workspace = temporary.path().join("workspace");
    std::fs::create_dir(&workspace).unwrap();
    generate_workspace(&workspace, files, symbols_per_file);

    let compiler = temporary.path().join("compactc");
    std::fs::write(
        &compiler,
        "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'compactc 0.33.0'; fi\nexit 0\n",
    )
    .unwrap();
    let mut permissions = std::fs::metadata(&compiler).unwrap().permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&compiler, permissions).unwrap();

    let workspace_uri = file_uri(&workspace);
    let main_path = workspace.join("Contract0000.compact");
    let main_uri = file_uri(&main_path);
    let main_source = std::fs::read_to_string(&main_path).unwrap();
    let mut lsp = LspHarness::start(&compiler).await;

    lsp.request(
        "initialize",
        json!({
            "processId": null,
            "capabilities": {},
            "workspaceFolders": [{
                "uri": workspace_uri,
                "name": format!("generated-{files}")
            }],
            "rootUri": null
        }),
    )
    .await;

    let startup_started = Instant::now();
    lsp.notify("initialized", json!({})).await;
    lsp.wait_until_ready().await;
    let startup = startup_started.elapsed();
    tokio::time::sleep(Duration::from_millis(150)).await;
    let ready_mib = lsp.resident_mib().await;
    let mut peak_mib = ready_mib;

    let symbol_started = Instant::now();
    let symbols = lsp
        .request("workspace/symbol", json!({ "query": "symbol_0000" }))
        .await;
    let workspace_symbol = symbol_started.elapsed();
    assert!(
        symbols.as_array().is_some_and(|items| !items.is_empty()),
        "workspace symbol query should find the generated declaration"
    );
    peak_mib = peak_mib.max(lsp.resident_mib().await);

    if !stress {
        eprintln!(
            "compact-lsp RSS: platform={platform} architecture={architecture} \
             profile={build_profile} files={files} symbols_per_file={symbols_per_file} \
             startup={startup:?} workspace_symbol={workspace_symbol:?} \
             ready={ready_mib:.3} MiB peak={peak_mib:.3} MiB"
        );
        lsp.shutdown().await;
        return;
    }

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
    lsp.wait_for_diagnostics(&main_uri).await;

    let completion_count = configured_count("COMPACT_LSP_RSS_COMPLETIONS", 1_000);
    let mut completion_latencies = Vec::with_capacity(completion_count);
    let completion_sample_interval = (completion_count / 10).max(1);
    for iteration in 0..completion_count {
        let started = Instant::now();
        lsp.request(
            "textDocument/completion",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 2, "character": 48 },
                "context": { "triggerKind": 1 }
            }),
        )
        .await;
        completion_latencies.push(started.elapsed());
        if (iteration + 1) % completion_sample_interval == 0 {
            peak_mib = peak_mib.max(lsp.resident_mib().await);
        }
    }
    let after_completions_mib = lsp.resident_mib().await;
    peak_mib = peak_mib.max(after_completions_mib);

    let edit_count = configured_count("COMPACT_LSP_RSS_EDITS", 200);
    let edit_batches = edit_count.clamp(1, 10);
    let mut version = 1;
    for batch in 0..edit_batches {
        let batch_end = (batch + 1) * edit_count / edit_batches;
        lsp.clear_diagnostics(&main_uri);
        while version - 1 < batch_end {
            version += 1;
            lsp.notify(
                "textDocument/didChange",
                json!({
                    "textDocument": { "uri": main_uri, "version": version },
                    "contentChanges": [{ "text": format!("{main_source}// edit {version}\n") }]
                }),
            )
            .await;
        }
        if edit_count > 0 {
            lsp.wait_for_diagnostics(&main_uri).await;
        }
        peak_mib = peak_mib.max(lsp.resident_mib().await);
    }
    let after_edits_mib = lsp.resident_mib().await;
    peak_mib = peak_mib.max(after_edits_mib);

    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main_uri } }),
    )
    .await;

    let open_close_count = configured_count("COMPACT_LSP_RSS_OPEN_CLOSE", 200);
    let open_close_sample_interval = (open_close_count / 10).max(1);
    for iteration in 0..open_close_count {
        let document_index = iteration % files;
        let document_path = workspace.join(format!("Contract{document_index:04}.compact"));
        let document_uri = file_uri(&document_path);
        let document_source = std::fs::read_to_string(&document_path).unwrap();
        lsp.clear_diagnostics(&document_uri);
        lsp.notify(
            "textDocument/didOpen",
            json!({
                "textDocument": {
                    "uri": document_uri,
                    "languageId": "compact",
                    "version": 1,
                    "text": document_source
                }
            }),
        )
        .await;
        lsp.wait_for_diagnostics(&document_uri).await;
        lsp.notify(
            "textDocument/didClose",
            json!({ "textDocument": { "uri": document_uri } }),
        )
        .await;
        if (iteration + 1) % open_close_sample_interval == 0 {
            peak_mib = peak_mib.max(lsp.resident_mib().await);
        }
    }
    let after_open_close_mib = lsp.resident_mib().await;
    peak_mib = peak_mib.max(after_open_close_mib);

    tokio::time::sleep(Duration::from_secs(1)).await;
    let settled_mib = lsp.resident_mib().await;
    peak_mib = peak_mib.max(settled_mib);

    let completion_median = duration_percentile(&completion_latencies, 50);
    let completion_p95 = duration_percentile(&completion_latencies, 95);
    eprintln!(
        "compact-lsp RSS: platform={platform} architecture={architecture} \
         profile={build_profile} files={files} symbols_per_file={symbols_per_file} \
         startup={startup:?} workspace_symbol={workspace_symbol:?} \
         ready={ready_mib:.3} MiB completions={completion_count} \
         completion_median={completion_median:?} completion_p95={completion_p95:?} \
         after_completions={after_completions_mib:.3} MiB edits={edit_count} \
         after_edits={after_edits_mib:.3} MiB open_close={open_close_count} \
         after_open_close={after_open_close_mib:.3} MiB \
         settled={settled_mib:.3} MiB peak={peak_mib:.3} MiB"
    );

    #[cfg(target_os = "macos")]
    if env::var_os("COMPACT_LSP_RSS_LEAKS").is_some() {
        inspect_macos_leaks(lsp.child.id().expect("running language server")).await;
    }

    lsp.shutdown().await;
}

/// Parse a non-negative benchmark iteration count from the environment.
fn configured_count(name: &str, default: usize) -> usize {
    env::var(name)
        .map(|value| {
            value
                .parse::<usize>()
                .unwrap_or_else(|_| panic!("{name} must be a non-negative integer"))
        })
        .unwrap_or(default)
}

/// Return the nearest-rank percentile for a non-empty latency sample.
fn duration_percentile(values: &[Duration], percentile: usize) -> Option<Duration> {
    if values.is_empty() {
        return None;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let rank = (sorted.len() * percentile + 99) / 100;
    Some(sorted[rank.saturating_sub(1).min(sorted.len() - 1)])
}

/// Run Apple's malloc leak inspector while the stressed server is still alive.
#[cfg(target_os = "macos")]
async fn inspect_macos_leaks(process_id: u32) {
    let output = Command::new("/usr/bin/leaks")
        .arg(process_id.to_string())
        .output()
        .await
        .expect("run Apple's leaks tool");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let summary = stdout
        .lines()
        .chain(stderr.lines())
        .find(|line| line.starts_with(&format!("Process {process_id}:")))
        .unwrap_or("leaks completed without a process summary");
    eprintln!("compact-lsp RSS: {summary}");
}
