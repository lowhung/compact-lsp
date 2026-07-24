import assert from "node:assert/strict";
import test from "node:test";

import {
  parseSha256Manifest,
  platformArtifact,
  releaseDownloadBase,
} from "../release.js";

test("maps supported platforms to release assets", () => {
  assert.equal(
    platformArtifact("darwin", "arm64").assetName,
    "compact-lsp-macos-arm64.tar.gz",
  );
  assert.equal(
    platformArtifact("darwin", "x64").assetName,
    "compact-lsp-macos-x86_64.tar.gz",
  );
  assert.equal(
    platformArtifact("linux", "x64").assetName,
    "compact-lsp-linux-x86_64.tar.gz",
  );
  assert.equal(
    platformArtifact("win32", "x64").binaryName,
    "compact-lsp.exe",
  );
  assert.throws(() => platformArtifact("linux", "arm64"), /Unsupported/u);
});

test("constructs latest and pinned release download bases", () => {
  assert.equal(
    releaseDownloadBase("lowhung/compact-lsp", "latest"),
    "https://github.com/lowhung/compact-lsp/releases/latest/download",
  );
  assert.equal(
    releaseDownloadBase("lowhung/compact-lsp", "v0.2.0"),
    "https://github.com/lowhung/compact-lsp/releases/download/v0.2.0",
  );
  assert.throws(
    () => releaseDownloadBase("https://example.com", "latest"),
    /owner\/repository/u,
  );
  assert.throws(
    () => releaseDownloadBase("lowhung/compact-lsp", "../latest"),
    /release tag/u,
  );
});

test("selects an exact asset checksum", () => {
  const expected = "a".repeat(64);
  const manifest = [
    `${"b".repeat(64)}  compact-lsp-linux-x86_64.tar.gz`,
    `${expected} *compact-lsp-macos-arm64.tar.gz`,
  ].join("\n");

  assert.equal(
    parseSha256Manifest(manifest, "compact-lsp-macos-arm64.tar.gz"),
    expected,
  );
  assert.throws(
    () => parseSha256Manifest(manifest, "missing.zip"),
    /does not contain/u,
  );
});
