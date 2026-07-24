// This file is part of compact-lsp.
// Copyright (C) 2025 Midnight Foundation
// SPDX-License-Identifier: Apache-2.0

//! Compact toolchain discovery and command construction.

use std::env;
use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use semver::Version;
use tokio::process::Command;

const COMPACT_CLI_ENV: &str = "COMPACT_CLI";
const COMPACT_COMPILER_ENV: &str = "COMPACT_COMPILER";
const COMPACT_FORMATTER_ENV: &str = "COMPACT_FORMATTER";
const COMPACT_TOOLCHAIN_VERSION_ENV: &str = "COMPACT_TOOLCHAIN_VERSION";
const COMPACT_COMPILER_ARGS_ENV: &str = "COMPACT_COMPILER_ARGS";

/// How an executable is invoked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ToolSource {
    /// Invoke a tool through the `compact` toolchain manager.
    CompactCli,
    /// Invoke the compiler or formatter binary directly.
    Direct,
}

/// A discovered Compact compiler command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompilerCommand {
    executable: PathBuf,
    source: ToolSource,
    version: Option<Version>,
    additional_args: Vec<String>,
}

impl CompilerCommand {
    /// Discover a compiler from environment overrides, the Compact CLI, and
    /// legacy direct-binary locations.
    pub fn discover() -> Option<Self> {
        let additional_args = compiler_args_from_env();

        if let Some(executable) = executable_from_env(COMPACT_COMPILER_ENV) {
            return Some(Self::direct(executable, additional_args));
        }

        if let Some(executable) = discover_compact_cli() {
            return Some(Self::compact_cli(
                executable,
                toolchain_version_from_env(),
                additional_args,
            ));
        }

        discover_direct_compiler().map(|executable| Self::direct(executable, additional_args))
    }

    /// Construct a direct compiler command.
    pub fn direct(executable: PathBuf, additional_args: Vec<String>) -> Self {
        Self {
            executable,
            source: ToolSource::Direct,
            version: None,
            additional_args,
        }
    }

    /// Construct a command routed through the Compact CLI.
    pub fn compact_cli(
        executable: PathBuf,
        version: Option<Version>,
        additional_args: Vec<String>,
    ) -> Self {
        Self {
            executable,
            source: ToolSource::CompactCli,
            version,
            additional_args,
        }
    }

    /// Executable path used for this command.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Discovery source used for this command.
    pub fn source(&self) -> ToolSource {
        self.source
    }

    /// Optional toolchain version selected for Compact CLI compilation.
    pub fn version(&self) -> Option<&Version> {
        self.version.as_ref()
    }

    /// Arguments for compiling a source file into an output directory.
    pub fn compile_arguments(&self, source: &Path, output: &Path) -> Vec<OsString> {
        let mut args = self.base_arguments();
        args.push("--vscode".into());
        args.push("--skip-zk".into());
        args.extend(self.additional_args.iter().map(OsString::from));
        args.push(source.as_os_str().to_owned());
        args.push(output.as_os_str().to_owned());
        args
    }

    /// Arguments for querying compiler or language version information.
    pub fn info_arguments(&self, flag: &str) -> Vec<OsString> {
        let mut args = self.base_arguments();
        args.push(flag.into());
        args
    }

    /// Build a Tokio command that kills its compiler child if the surrounding
    /// diagnostic future is cancelled.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.executable);
        command.args(args);
        command.kill_on_drop(true);
        command
    }

    fn base_arguments(&self) -> Vec<OsString> {
        match self.source {
            ToolSource::CompactCli => {
                let mut args = vec![OsString::from("compile")];
                if let Some(version) = &self.version {
                    args.push(format!("+{version}").into());
                }
                args
            }
            ToolSource::Direct => Vec::new(),
        }
    }
}

/// A discovered Compact formatter command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormatterCommand {
    executable: PathBuf,
    source: ToolSource,
}

impl FormatterCommand {
    /// Discover a formatter from environment overrides, the Compact CLI, and
    /// legacy direct-binary locations.
    pub fn discover() -> Option<Self> {
        if let Some(executable) = executable_from_env(COMPACT_FORMATTER_ENV) {
            return Some(Self::direct(executable));
        }

        if let Some(executable) = discover_compact_cli() {
            return Some(Self::compact_cli(executable));
        }

        discover_direct_formatter().map(Self::direct)
    }

    /// Construct a direct formatter command.
    pub fn direct(executable: PathBuf) -> Self {
        Self {
            executable,
            source: ToolSource::Direct,
        }
    }

    /// Construct a command routed through the Compact CLI.
    pub fn compact_cli(executable: PathBuf) -> Self {
        Self {
            executable,
            source: ToolSource::CompactCli,
        }
    }

    /// Executable path used for this command.
    pub fn executable(&self) -> &Path {
        &self.executable
    }

    /// Discovery source used for this command.
    pub fn source(&self) -> ToolSource {
        self.source
    }

    /// Arguments for formatting the supplied temporary file.
    pub fn format_arguments(&self, source: &Path) -> Vec<OsString> {
        match self.source {
            ToolSource::CompactCli => {
                vec![OsString::from("format"), source.as_os_str().to_owned()]
            }
            ToolSource::Direct => vec![source.as_os_str().to_owned()],
        }
    }

    /// Build a Tokio command that kills its formatter child if the request is
    /// cancelled.
    pub fn command<I, S>(&self, args: I) -> Command
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(&self.executable);
        command.args(args);
        command.kill_on_drop(true);
        command
    }
}

fn compiler_args_from_env() -> Vec<String> {
    let Some(raw) = env::var_os(COMPACT_COMPILER_ARGS_ENV) else {
        return Vec::new();
    };
    let raw = raw.to_string_lossy();

    match serde_json::from_str::<Vec<String>>(&raw) {
        Ok(args) => args,
        Err(error) => {
            tracing::warn!(
                "{} must be a JSON string array; ignoring value: {}",
                COMPACT_COMPILER_ARGS_ENV,
                error
            );
            Vec::new()
        }
    }
}

fn toolchain_version_from_env() -> Option<Version> {
    let raw = env::var(COMPACT_TOOLCHAIN_VERSION_ENV).ok()?;

    match Version::parse(&raw) {
        Ok(version) => Some(version),
        Err(error) => {
            tracing::warn!(
                "{} is not a valid semantic version; ignoring value: {}",
                COMPACT_TOOLCHAIN_VERSION_ENV,
                error
            );
            None
        }
    }
}

fn discover_compact_cli() -> Option<PathBuf> {
    executable_from_env(COMPACT_CLI_ENV)
        .or_else(|| find_in_path("compact"))
        .or_else(|| {
            home_dir()
                .map(|home| home.join(".local/bin/compact"))
                .filter(|p| p.is_file())
        })
}

fn discover_direct_compiler() -> Option<PathBuf> {
    find_in_path("compactc.bin")
        .or_else(|| find_in_path("compactc"))
        .or_else(|| {
            home_dir()
                .map(|home| home.join(".compact/bin/compactc"))
                .filter(|path| path.is_file())
        })
        .or_else(|| {
            home_dir()
                .map(|home| home.join("compactc/compactc.bin"))
                .filter(|path| path.is_file())
        })
}

fn discover_direct_formatter() -> Option<PathBuf> {
    find_in_path("format-compact")
        .or_else(|| {
            home_dir()
                .map(|home| home.join(".compact/bin/format-compact"))
                .filter(|path| path.is_file())
        })
        .or_else(|| {
            home_dir()
                .map(|home| home.join("compactc/format-compact"))
                .filter(|path| path.is_file())
        })
}

fn executable_from_env(name: &str) -> Option<PathBuf> {
    let raw = env::var_os(name)?;
    let path = PathBuf::from(&raw);

    if path.is_file() {
        return Some(path);
    }

    if path.components().count() == 1 {
        if let Some(found) = find_in_path(&raw) {
            return Some(found);
        }
    }

    tracing::warn!(
        "{} points to an executable that could not be found: {}",
        name,
        path.display()
    );
    None
}

fn find_in_path(executable: impl AsRef<OsStr>) -> Option<PathBuf> {
    let executable = Path::new(executable.as_ref());
    let path = env::var_os("PATH")?;

    for directory in env::split_paths(&path) {
        let candidate = directory.join(executable);
        if candidate.is_file() {
            return Some(candidate);
        }

        #[cfg(windows)]
        {
            if candidate.extension().is_none() {
                if let Some(extensions) = env::var_os("PATHEXT") {
                    for extension in extensions.to_string_lossy().split(';') {
                        let extension = extension.trim_start_matches('.');
                        let candidate = candidate.with_extension(extension);
                        if candidate.is_file() {
                            return Some(candidate);
                        }
                    }
                }
            }
        }
    }

    None
}

fn home_dir() -> Option<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_compiler_arguments_include_diagnostic_flags_and_project_args() {
        let compiler = CompilerCommand::direct(
            PathBuf::from("/tools/compactc.bin"),
            vec!["--feature-zkir-v3".to_string()],
        );

        assert_eq!(
            compiler.compile_arguments(Path::new("contract.compact"), Path::new("output")),
            vec![
                "--vscode",
                "--skip-zk",
                "--feature-zkir-v3",
                "contract.compact",
                "output",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn compact_cli_arguments_include_subcommand_and_selected_version() {
        let compiler = CompilerCommand::compact_cli(
            PathBuf::from("/tools/compact"),
            Some(Version::parse("0.33.0-rc.2").unwrap()),
            Vec::new(),
        );

        assert_eq!(
            compiler.compile_arguments(Path::new("contract.compact"), Path::new("output")),
            vec![
                "compile",
                "+0.33.0-rc.2",
                "--vscode",
                "--skip-zk",
                "contract.compact",
                "output",
            ]
            .into_iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
        );
    }

    #[test]
    fn compact_cli_info_arguments_preserve_version_selection() {
        let compiler = CompilerCommand::compact_cli(
            PathBuf::from("/tools/compact"),
            Some(Version::new(0, 33, 0)),
            Vec::new(),
        );

        assert_eq!(
            compiler.info_arguments("--language-version"),
            ["compile", "+0.33.0", "--language-version"]
                .into_iter()
                .map(OsString::from)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn formatter_arguments_match_the_discovery_source() {
        let direct = FormatterCommand::direct(PathBuf::from("/tools/format-compact"));
        let compact = FormatterCommand::compact_cli(PathBuf::from("/tools/compact"));

        assert_eq!(
            direct.format_arguments(Path::new("contract.compact")),
            vec![OsString::from("contract.compact")]
        );
        assert_eq!(
            compact.format_arguments(Path::new("contract.compact")),
            vec![OsString::from("format"), OsString::from("contract.compact")]
        );
    }
}
