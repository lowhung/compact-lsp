# Editor marketplace publishing

The VS Code extension is published to the Visual Studio Marketplace and Open
VSX from the same VSIX attached to a `compact-lsp` GitHub release. Registry
publishing does not rebuild the extension. This keeps the downloadable VSIX,
its `SHA256SUMS` entry, and both registry packages byte-for-byte identical.

Publishing is deliberately separate from the release workflow. A maintainer
must dispatch the `Publish VS Code extension` workflow and approve its
`editor-marketplaces` environment before either registry is changed.

## One-time publisher setup

### Visual Studio Marketplace

1. Create the `lowhung` publisher in the
   [Visual Studio Marketplace publisher portal][marketplace-publishers]. The
   value must match `publisher` in `editors/vscode/package.json`.
2. Create an Azure DevOps personal access token with the Marketplace `Manage`
   scope for all accessible organizations.
3. Store the token as `VSCE_PAT` in the `editor-marketplaces` GitHub
   environment.

The publishing workflow uses the repository-pinned `@vscode/vsce` dependency.
It exposes `VSCE_PAT` only to the Marketplace publication step, after
dependencies and the release artifact have been validated. Do not put the
token in a command, checked-in file, workflow input, or release artifact.

### Open VSX

1. Create an Eclipse account whose GitHub username is `lowhung`.
2. Sign in to Open VSX with the same GitHub account, connect the Eclipse
   account, and accept the Publisher Agreement.
3. Generate an Open VSX access token.
4. Create the `lowhung` namespace once:

   ```bash
   npx ovsx@1.0.2 create-namespace lowhung --pat <token>
   ```

5. Store the token as `OVSX_PAT` in the `editor-marketplaces` GitHub
   environment.
6. Claim verified ownership of the namespace separately if a verified publisher
   badge is desired.

The workflow uses the repository-pinned `ovsx` dependency and exposes
`OVSX_PAT` only to the Open VSX publication step. A namespace must exist before
its first extension can be published.

### GitHub environment

Create an `editor-marketplaces` environment in the repository and configure:

- `VSCE_PAT` for the Visual Studio Marketplace.
- `OVSX_PAT` for Open VSX.
- Required reviewers so a manual workflow dispatch cannot publish without a
  second explicit approval.

Limit both tokens to publishing. Rotate them when a maintainer changes, after
suspected exposure, or before their configured expiry.

## Publish a release

1. Complete the GitHub release and verification procedure in
   [Releasing compact-lsp](releasing.md).
2. Confirm the release contains `compact-lsp-vscode-v<version>.vsix` and
   `SHA256SUMS`.
3. Open **Actions → Publish VS Code extension → Run workflow** on `main`.
4. Enter the existing release tag, choose `both`, and select the release
   channel:
   - Use `pre-release` while `compact-lsp` is in public beta. This marks the
     Visual Studio Marketplace package as a pre-release.
   - Use `stable` only for a version that has completed stable-release review.
5. Review and approve the `editor-marketplaces` deployment.

Open VSX ignores its pre-release option for an already packaged VSIX. The
workflow therefore publishes the exact checked artifact as a normal Open VSX
version and identifies its beta status in the packaged extension
documentation. It does not rebuild the VSIX just to change registry metadata.

Before publishing, the workflow:

- Rejects tags that are not `v<major>.<minor>.<patch>`.
- Checks out the exact tag.
- Downloads the VSIX and checksum manifest from that GitHub release.
- Verifies the VSIX checksum.
- Verifies the VSIX GitHub build-provenance attestation.
- Confirms the tag, source manifest, and packaged manifest versions match.
- Confirms the packaged extension is `lowhung.compact-lsp`.

The workflow can target one registry when retrying a partial publication.
Duplicate versions are skipped, so a retry cannot replace an existing package
with different bytes.

## Verify registry installation

Wait for both registries to finish processing the extension, then record the
evidence in the [client compatibility matrix](client-compatibility.md).

For Visual Studio Code:

1. Start with a clean profile and no manually installed VSIX.
2. Install `lowhung.compact-lsp` from the Extensions view, selecting the
   pre-release version when applicable.
3. Open `test-fixtures/client-smoke/src/main.compact`.
4. Confirm the language server downloads from the matching GitHub release,
   starts, and reports the expected version in the Compact output channel.
5. Run the VS Code smoke checks in the compatibility matrix.

For an Open VSX client:

1. Start with a clean editor profile and an Open VSX-backed extension gallery.
2. Install `lowhung.compact-lsp` from the gallery.
3. Repeat the same Compact 0.33 workspace and server-version checks.

Keep the GitHub release VSIX available as the manual installation fallback.

## Rollback and recovery

Do not move a release tag or replace a registry version. If a published
extension or server release is defective:

1. Mark the affected GitHub release as a pre-release and add a warning.
2. Pin `compact.server.version` to the last known-good server tag as an
   immediate local workaround.
3. Fix the defect on `main`, increment the patch version, and publish a new
   signed GitHub release.
4. Publish that new VSIX to both registries and repeat the clean-profile smoke
   tests.

Unpublishing removes an installation path for existing users and is reserved
for security, legal, or secret-exposure incidents.

[marketplace-publishers]: https://marketplace.visualstudio.com/manage/publishers/
