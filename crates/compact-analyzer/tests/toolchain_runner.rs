#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};

    use compact_analyzer::{
        CompilerCommand, CompilerCompatibility, DiagnosticEngine, FormatterCommand, FormatterEngine,
    };
    use semver::Version;
    use tokio::time::{sleep, timeout, Duration};

    fn executable_script(directory: &Path, name: &str, body: &str) -> PathBuf {
        let path = directory.join(name);
        fs::write(&path, format!("#!/bin/sh\nset -eu\n{body}\n")).unwrap();

        let mut permissions = fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&path, permissions).unwrap();
        path
    }

    #[tokio::test]
    async fn live_diagnostics_use_a_unique_source_beside_the_original_file() {
        let temp = tempfile::tempdir().unwrap();
        let project = temp.path().join("Compact Project");
        fs::create_dir(&project).unwrap();

        let recorded_source = temp.path().join("recorded-source");
        let compiler = executable_script(
            temp.path(),
            "mock-compactc",
            r#"
record_file="$3"
source_file="$4"
printf '%s' "$source_file" > "$record_file"
printf 'Exception: %s line 2 char 4: mock type error\n' "$source_file" >&2
exit 1
"#,
        );

        let engine = DiagnosticEngine::with_compiler(CompilerCommand::direct(
            compiler,
            vec![recorded_source.to_string_lossy().to_string()],
        ));
        let original = project.join("contract name.compact");
        let uri = url::Url::from_file_path(&original).unwrap();

        let diagnostics = engine
            .diagnose_content(uri.as_str(), "circuit broken(): Field {}")
            .await;

        assert_eq!(diagnostics.len(), 1);
        assert_eq!(diagnostics[0].range.start.line, 1);
        assert_eq!(diagnostics[0].range.start.character, 3);
        assert_eq!(diagnostics[0].message, "mock type error");

        let temporary_source = PathBuf::from(fs::read_to_string(recorded_source).unwrap());
        assert_eq!(temporary_source.parent(), Some(project.as_path()));
        assert_ne!(temporary_source, original);
        assert!(
            !temporary_source.exists(),
            "temporary source should be removed after diagnostics"
        );
    }

    #[tokio::test]
    async fn compiler_info_reports_primary_0_33_compatibility() {
        let temp = tempfile::tempdir().unwrap();
        let compiler = executable_script(
            temp.path(),
            "mock-compactc",
            r#"
case "$1" in
  --version) printf '0.33.0\n' ;;
  --language-version) printf '0.25.0\n' ;;
  *) exit 2 ;;
esac
"#,
        );
        let engine = DiagnosticEngine::with_compiler(CompilerCommand::direct(compiler, Vec::new()));

        let info = engine.compiler_info().await.unwrap().unwrap();

        assert_eq!(info.compiler_version, "0.33.0");
        assert_eq!(info.language_version, "0.25.0");
        assert_eq!(info.compatibility, CompilerCompatibility::Primary);
    }

    #[tokio::test]
    async fn direct_formatter_returns_stdout() {
        let temp = tempfile::tempdir().unwrap();
        let formatter = executable_script(
            temp.path(),
            "mock-format-compact",
            "printf 'export circuit formatted(): [] {}\\n'",
        );
        let engine = FormatterEngine::with_formatter(FormatterCommand::direct(formatter));

        let formatted = engine.format("unformatted").await.unwrap();

        assert_eq!(formatted, "export circuit formatted(): [] {}\n");
    }

    #[tokio::test]
    async fn compact_cli_formatter_reads_the_rewritten_file() {
        let temp = tempfile::tempdir().unwrap();
        let compact = executable_script(
            temp.path(),
            "mock-compact",
            r#"
test "$1" = "format"
printf 'export circuit formatted(): [] {}\n' > "$2"
"#,
        );
        let engine = FormatterEngine::with_formatter(FormatterCommand::compact_cli(compact));

        let formatted = engine.format("unformatted").await.unwrap();

        assert_eq!(formatted, "export circuit formatted(): [] {}\n");
    }

    #[tokio::test]
    async fn compact_cli_compiler_preserves_prerelease_version_selection() {
        let temp = tempfile::tempdir().unwrap();
        let recorded_args = temp.path().join("recorded-args");
        let compact = executable_script(
            temp.path(),
            "mock-compact",
            r#"
record_file="$5"
printf '%s\n' "$@" > "$record_file"
exit 0
"#,
        );
        let engine = DiagnosticEngine::with_compiler(CompilerCommand::compact_cli(
            compact,
            Some(Version::parse("0.33.0-rc.2").unwrap()),
            vec![recorded_args.to_string_lossy().to_string()],
        ));

        let source = temp.path().join("contract.compact");
        fs::write(&source, "export circuit test(): [] {}").unwrap();
        let uri = url::Url::from_file_path(source).unwrap();
        let diagnostics = engine.diagnose(uri.as_str(), "").await;

        assert!(diagnostics.is_empty());
        let args = fs::read_to_string(recorded_args).unwrap();
        assert!(args.starts_with("compile\n+0.33.0-rc.2\n--vscode\n--skip-zk\n"));
    }

    #[tokio::test]
    async fn cancelling_diagnostics_terminates_the_compiler_process() {
        let temp = tempfile::tempdir().unwrap();
        let pid_file = temp.path().join("compiler-pid");
        let compiler = executable_script(
            temp.path(),
            "slow-compactc",
            r#"
printf '%s' "$$" > "$3"
sleep 30
"#,
        );
        let engine = DiagnosticEngine::with_compiler(CompilerCommand::direct(
            compiler,
            vec![pid_file.to_string_lossy().to_string()],
        ));
        let original = temp.path().join("contract.compact");
        let uri = url::Url::from_file_path(original).unwrap().to_string();

        let task = tokio::spawn(async move {
            engine
                .diagnose_content(&uri, "export circuit test(): [] {}")
                .await
        });

        let pid = match timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(pid) = fs::read_to_string(&pid_file) {
                    if !pid.trim().is_empty() {
                        break pid;
                    }
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        {
            Ok(pid) => pid,
            Err(_) => {
                task.abort();
                let _ = task.await;
                panic!("mock compiler did not start");
            }
        };

        task.abort();
        let _ = task.await;

        let terminated = timeout(Duration::from_secs(5), async {
            loop {
                let status = std::process::Command::new("kill")
                    .args(["-0", pid.trim()])
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .unwrap();
                if !status.success() {
                    break;
                }
                sleep(Duration::from_millis(20)).await;
            }
        })
        .await;
        assert!(
            terminated.is_ok(),
            "compiler process {pid} survived cancellation"
        );
    }

    #[tokio::test]
    #[ignore = "requires COMPACT_LSP_TEST_COMPILER pointing to Compact 0.33"]
    async fn validates_fixture_with_real_compact_0_33_compiler() {
        let compiler = std::env::var_os("COMPACT_LSP_TEST_COMPILER")
            .map(PathBuf::from)
            .expect("set COMPACT_LSP_TEST_COMPILER to compactc 0.33");
        let engine = DiagnosticEngine::with_compiler(CompilerCommand::direct(
            compiler,
            vec!["--feature-zkir-v3".to_string()],
        ));

        let info = engine.compiler_info().await.unwrap().unwrap();
        assert_eq!(info.compatibility, CompilerCompatibility::Primary);
        assert_eq!(info.language_version, "0.25.0");

        let temp = tempfile::tempdir().unwrap();
        let original = temp.path().join("compact_0_33.compact");
        let uri = url::Url::from_file_path(original).unwrap();
        let content = include_str!("fixtures/compact_0_33.compact");

        let diagnostics = engine.diagnose_content(uri.as_str(), content).await;
        assert!(
            diagnostics.is_empty(),
            "real Compact 0.33 compiler returned diagnostics: {diagnostics:#?}"
        );
    }
}
