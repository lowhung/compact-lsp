export type ArchiveKind = "tar.gz" | "zip";

export interface PlatformArtifact {
  readonly assetName: string;
  readonly archiveKind: ArchiveKind;
  readonly binaryName: string;
}

const REPOSITORY_PATTERN = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/;
const RELEASE_TAG_PATTERN = /^[A-Za-z0-9._-]+$/;
const SHA256_PATTERN = /^[a-fA-F0-9]{64}$/;

export function platformArtifact(
  platform: NodeJS.Platform,
  arch: string,
): PlatformArtifact {
  if (platform === "darwin" && arch === "arm64") {
    return {
      assetName: "compact-lsp-macos-arm64.tar.gz",
      archiveKind: "tar.gz",
      binaryName: "compact-lsp",
    };
  }
  if (platform === "darwin" && arch === "x64") {
    return {
      assetName: "compact-lsp-macos-x86_64.tar.gz",
      archiveKind: "tar.gz",
      binaryName: "compact-lsp",
    };
  }
  if (platform === "linux" && arch === "x64") {
    return {
      assetName: "compact-lsp-linux-x86_64.tar.gz",
      archiveKind: "tar.gz",
      binaryName: "compact-lsp",
    };
  }
  if (platform === "win32" && arch === "x64") {
    return {
      assetName: "compact-lsp-windows-x86_64.zip",
      archiveKind: "zip",
      binaryName: "compact-lsp.exe",
    };
  }

  throw new Error(`Unsupported compact-lsp platform: ${platform}/${arch}`);
}

export function releaseDownloadBase(repository: string, version: string): string {
  if (!REPOSITORY_PATTERN.test(repository)) {
    throw new Error(
      `Invalid Compact server repository "${repository}"; expected owner/repository`,
    );
  }
  if (version === "latest") {
    return `https://github.com/${repository}/releases/latest/download`;
  }
  if (!RELEASE_TAG_PATTERN.test(version)) {
    throw new Error(`Invalid Compact server release tag "${version}"`);
  }
  return `https://github.com/${repository}/releases/download/${version}`;
}

export function parseSha256Manifest(
  manifest: string,
  assetName: string,
): string {
  for (const line of manifest.split(/\r?\n/u)) {
    const match = /^([a-fA-F0-9]{64}) [ *](.+)$/u.exec(line.trim());
    if (match?.[2] === assetName && SHA256_PATTERN.test(match[1] ?? "")) {
      return (match[1] ?? "").toLowerCase();
    }
  }
  throw new Error(`SHA256SUMS does not contain ${assetName}`);
}
