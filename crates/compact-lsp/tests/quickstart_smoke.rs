//! Hermetic clone-to-first-result smoke test exposed as `cargo smoke`.
//!
//! The test starts Cargo's exact `compact-lsp` binary and speaks framed
//! JSON-RPC over standard input/output. It intentionally uses a missing
//! compiler path so parser and protocol behavior cannot depend on a developer's
//! installed Compact toolchain.

use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::json;

mod support;

use support::{file_uri, LspHarness};

#[tokio::test]
async fn fresh_clone_serves_checked_in_workspace() {
    tokio::time::timeout(Duration::from_secs(30), run_smoke())
        .await
        .expect("fresh-clone smoke test timed out");
}

/// Exercise representative language features through the real stdio process.
async fn run_smoke() {
    let root = smoke_workspace();
    let main_path = root.join("Main.compact");
    let broken_path = root.join("Broken.compact");
    let main_uri = file_uri(&main_path);
    let broken_uri = file_uri(&broken_path);
    let root_uri = file_uri(&root);
    let main_source = std::fs::read_to_string(&main_path).expect("read Main.compact");
    let broken_source = std::fs::read_to_string(&broken_path).expect("read Broken.compact");

    let mut lsp = LspHarness::start(&root.join("missing-compactc")).await;
    let initialize = lsp
        .request(
            "initialize",
            json!({
                "processId": null,
                "capabilities": {
                    "workspace": {
                        "workspaceFolders": true,
                        "didChangeWatchedFiles": { "dynamicRegistration": false }
                    }
                },
                "workspaceFolders": [{
                    "uri": root_uri,
                    "name": "compact-lsp smoke"
                }],
                "rootUri": null
            }),
        )
        .await;

    let capabilities = &initialize["capabilities"];
    assert!(capabilities["completionProvider"].is_object());
    assert_eq!(capabilities["hoverProvider"], true);
    assert_eq!(capabilities["definitionProvider"], true);
    assert_eq!(capabilities["documentSymbolProvider"], true);
    assert!(capabilities["semanticTokensProvider"].is_object());
    assert!(capabilities["codeActionProvider"].is_object());
    println!("ok - initialized compact-lsp and negotiated language capabilities");

    lsp.notify("initialized", json!({})).await;
    lsp.wait_until_ready().await;
    println!("ok - server reported ready");

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

    lsp.wait_for_completion(&main_uri, "Utils_scale", true)
        .await;
    lsp.wait_for_workspace_symbol("scale", "scale", true).await;
    println!("ok - imported completion and workspace symbol resolved");

    let hover = lsp
        .request(
            "textDocument/hover",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 8, "character": 17 }
            }),
        )
        .await;
    assert!(!hover.is_null(), "hover did not resolve add");

    let definition = lsp
        .request(
            "textDocument/definition",
            json!({
                "textDocument": { "uri": main_uri },
                "position": { "line": 13, "character": 17 }
            }),
        )
        .await;
    assert!(!definition.is_null(), "definition did not resolve add");
    println!("ok - hover and definition resolved");

    let symbols = lsp
        .request(
            "textDocument/documentSymbol",
            json!({ "textDocument": { "uri": main_uri } }),
        )
        .await;
    assert!(
        symbols.as_array().is_some_and(|symbols| symbols.len() >= 3),
        "document symbols were not returned"
    );

    let semantic_tokens = lsp
        .request(
            "textDocument/semanticTokens/full",
            json!({ "textDocument": { "uri": main_uri } }),
        )
        .await;
    assert!(
        semantic_tokens["data"]
            .as_array()
            .is_some_and(|tokens| !tokens.is_empty()),
        "semantic tokens were empty"
    );
    println!("ok - document symbols and semantic tokens returned");

    lsp.notify(
        "textDocument/didOpen",
        json!({
            "textDocument": {
                "uri": broken_uri,
                "languageId": "compact",
                "version": 1,
                "text": broken_source
            }
        }),
    )
    .await;
    let publication = lsp
        .wait_for_diagnostic(&broken_uri, 1, "Syntax error: missing ;")
        .await;
    let diagnostic = publication["params"]["diagnostics"]
        .as_array()
        .and_then(|diagnostics| {
            diagnostics
                .iter()
                .find(|diagnostic| diagnostic["message"] == "Syntax error: missing ;")
        })
        .cloned()
        .expect("missing-semicolon diagnostic");
    assert_eq!(diagnostic["source"], "compact-syntax");

    let actions = lsp
        .request(
            "textDocument/codeAction",
            json!({
                "textDocument": { "uri": broken_uri },
                "range": diagnostic["range"],
                "context": {
                    "diagnostics": [diagnostic],
                    "only": ["quickfix"],
                    "triggerKind": 1
                }
            }),
        )
        .await;
    assert_eq!(actions[0]["title"], "Insert missing `;`");
    println!("ok - syntax diagnostic and safe quick fix returned");

    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": broken_uri } }),
    )
    .await;
    lsp.notify(
        "textDocument/didClose",
        json!({ "textDocument": { "uri": main_uri } }),
    )
    .await;
    lsp.shutdown().await;
    println!("ok - clean shutdown completed");
}

/// Return the checked-in smoke workspace as an absolute path for file URIs.
fn smoke_workspace() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../test-fixtures/client-smoke")
        .canonicalize()
        .expect("checked-in client smoke workspace")
}
