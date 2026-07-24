# Releasing compact-lsp

Releases are maintained from `lowhung/compact-lsp` while the project is in
public beta. The server crates and VS Code extension share one version.

## Prepare a release

1. Start from a clean, reviewed `main` commit with green CI.
2. Update the workspace version in `Cargo.toml` and the extension version in
   `editors/vscode/package.json`; refresh both lockfiles.
3. Move the relevant changelog entries from `Unreleased` to the release
   version and date.
4. Run the Rust and VS Code checks from `CONTRIBUTING.md`.
5. Confirm the release version contract:

   ```bash
   cargo metadata --locked --no-deps --format-version 1 \
     | jq -r '.packages[] | select(.name == "compact-lsp") | .version'
   jq -r '.version' editors/vscode/package.json
   ```

6. Confirm the latest automatic `Release` workflow run on `main` succeeded.
   Every update to `main` executes the full verification, cross-platform build,
   packaging, artifact download, and checksum assembly without creating
   attestations or publishing a GitHub release. The workflow can also be run
   manually from `main` when needed. Download the
   `compact-lsp-release-dry-run-*` artifact and inspect its archives, VSIX, and
   `SHA256SUMS`.
7. Create a Verified signed tag named `v<version>` and push it.

The tag starts `.github/workflows/release.yml`. The workflow rejects a tag that
does not match both manifests. Only a matching `v*` tag creates attestations
and publishes a GitHub release; branch and manual runs only upload the dry-run
bundle.

## Verify the published release

The workflow publishes:

- Server archives for macOS arm64, macOS x86-64, Linux x86-64, and Windows
  x86-64.
- A versioned VSIX.
- `SHA256SUMS`.
- GitHub build-provenance attestations for every archive, the VSIX, and the
  checksum manifest.

After the workflow completes:

1. Download every asset and check it against `SHA256SUMS`.
2. Verify provenance:

   ```bash
   gh attestation verify <asset> --repo lowhung/compact-lsp
   ```

3. Run `compact-lsp --version` from each available server platform.
4. Install the VSIX into a clean VS Code profile and open a Compact 0.33
   workspace.
5. Exercise initialization, completion, navigation, compiler diagnostics,
   formatting, restart, and clean shutdown before marking the release ready.

## Cargo publication

GitHub release binaries and the VSIX are the beta distribution channel. If the
Rust crates are published later, publish and verify `compact-analyzer` before
`compact-lsp`; the server package has a versioned dependency on the analyzer.

## Bad release

If a release has a correctness, security, or packaging defect:

1. Mark the GitHub release as a prerelease and add a prominent warning.
2. Do not move or reuse its tag.
3. Fix the issue on `main`, increment the patch version, and publish a new
   signed tag.
4. Pin `compact.server.version` to the last known-good tag as a temporary
   client-side rollback.
