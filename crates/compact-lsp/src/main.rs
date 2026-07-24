//! Compact LSP - Language Server for the Compact smart contract language
//!
//! # How this works
//!
//! 1. This binary is started by the editor (e.g., Neovim)
//! 2. Communication happens over stdin/stdout using JSON-RPC
//! 3. The editor sends requests (initialize, textDocument/*, etc.)
//! 4. We respond with results or send notifications (diagnostics, etc.)
//!
//! # Why we use stderr for logging
//!
//! Since stdin/stdout are used for the LSP protocol, we CANNOT use
//! println!() for debugging. Instead, we use the `tracing` crate
//! which writes to stderr.

use compact_lsp::server;
use std::ffi::OsString;
use tower_lsp::{LspService, Server};
use tracing_subscriber::EnvFilter;

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help,
    Serve,
    Version,
}

fn parse_command(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let arguments: Vec<_> = arguments.into_iter().collect();
    match arguments.as_slice() {
        [] => Ok(Command::Serve),
        [argument] if argument == "--help" || argument == "-h" => Ok(Command::Help),
        [argument] if argument == "--version" || argument == "-V" => Ok(Command::Version),
        _ => Err(format!(
            "unexpected arguments: {}",
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy())
                .collect::<Vec<_>>()
                .join(" ")
        )),
    }
}

#[tokio::main]
async fn main() {
    match parse_command(std::env::args_os().skip(1)) {
        Ok(Command::Help) => {
            println!(
                "compact-lsp {}\n\nUsage: compact-lsp [--help | --version]\n\nRuns the Compact language server over standard input and output.",
                env!("CARGO_PKG_VERSION")
            );
            return;
        }
        Ok(Command::Version) => {
            println!("compact-lsp {}", env!("CARGO_PKG_VERSION"));
            return;
        }
        Ok(Command::Serve) => {}
        Err(error) => {
            eprintln!("compact-lsp: {error}\nTry 'compact-lsp --help' for usage.");
            std::process::exit(2);
        }
    }

    // Initialize logging to stderr
    // Set RUST_LOG=debug to see debug messages
    // Example: RUST_LOG=compact_lsp=debug cargo run
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr) // IMPORTANT: write to stderr, not stdout
        .init();

    tracing::info!("Starting compact-lsp server");

    // Create stdin/stdout handles for the LSP transport
    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    // Build the LSP service
    // The Client is used to send notifications TO the editor (e.g., diagnostics)
    let (service, socket) = LspService::build(server::CompactLanguageServer::new).finish();

    // Start the server - this runs until the editor disconnects
    Server::new(stdin, stdout, socket).serve(service).await;

    tracing::info!("compact-lsp server stopped");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_server_and_metadata_commands() {
        assert_eq!(parse_command([]), Ok(Command::Serve));
        assert_eq!(parse_command([OsString::from("--help")]), Ok(Command::Help));
        assert_eq!(parse_command([OsString::from("-V")]), Ok(Command::Version));
    }

    #[test]
    fn rejects_unknown_or_multiple_arguments() {
        assert!(parse_command([OsString::from("--tcp")]).is_err());
        assert!(parse_command([OsString::from("--help"), OsString::from("extra")]).is_err());
    }
}
