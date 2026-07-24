import { createHash } from "node:crypto";
import {
  constants as fsConstants,
  createReadStream,
  createWriteStream,
} from "node:fs";
import {
  access,
  chmod,
  mkdir,
  mkdtemp,
  rename,
  rm,
} from "node:fs/promises";
import type { IncomingMessage } from "node:http";
import { get as httpsGet } from "node:https";
import { homedir } from "node:os";
import { delimiter, dirname, isAbsolute, join, resolve } from "node:path";
import { pipeline } from "node:stream/promises";

import extractZip from "extract-zip";
import * as tar from "tar";
import * as vscode from "vscode";

import {
  parseSha256Manifest,
  platformArtifact,
  releaseDownloadBase,
} from "./release.js";

const MAX_REDIRECTS = 8;
const REQUEST_TIMEOUT_MS = 30_000;
const MAX_MANIFEST_BYTES = 1024 * 1024;
let installation: Promise<string> | undefined;

function expandPath(value: string): string {
  const expanded = value.startsWith("~/")
    ? join(homedir(), value.slice(2))
    : value;
  return isAbsolute(expanded) ? expanded : resolve(expanded);
}

async function isExecutable(file: string): Promise<boolean> {
  try {
    await access(
      file,
      process.platform === "win32" ? fsConstants.F_OK : fsConstants.X_OK,
    );
    return true;
  } catch {
    return false;
  }
}

async function findOnPath(binaryName: string): Promise<string | undefined> {
  const candidates = new Set<string>();
  const pathEntries = (process.env.PATH ?? "")
    .split(delimiter)
    .filter((entry) => entry.length > 0);
  pathEntries.push(join(homedir(), ".local", "bin"));

  const extensions =
    process.platform === "win32"
      ? (process.env.PATHEXT ?? ".EXE;.CMD;.BAT")
          .split(";")
          .filter((extension) => extension.length > 0)
      : [""];
  for (const entry of pathEntries) {
    for (const extension of extensions) {
      const candidate =
        process.platform === "win32" &&
        !binaryName.toUpperCase().endsWith(extension.toUpperCase())
          ? join(entry, `${binaryName}${extension}`)
          : join(entry, binaryName);
      candidates.add(candidate);
    }
  }

  for (const candidate of candidates) {
    if (await isExecutable(candidate)) {
      return candidate;
    }
  }
  return undefined;
}

function response(url: string, redirects = MAX_REDIRECTS): Promise<IncomingMessage> {
  return new Promise((resolveResponse, reject) => {
    const request = httpsGet(
      url,
      {
        headers: {
          Accept: "application/octet-stream",
          "User-Agent": "compact-lsp-vscode",
        },
      },
      (incoming) => {
        const status = incoming.statusCode ?? 0;
        const location = incoming.headers.location;
        if (status >= 300 && status < 400 && location) {
          incoming.resume();
          if (redirects === 0) {
            reject(new Error(`Too many redirects downloading ${url}`));
            return;
          }
          const redirected = new URL(location, url);
          if (redirected.protocol !== "https:") {
            reject(new Error(`Refusing non-HTTPS redirect to ${redirected}`));
            return;
          }
          response(redirected.toString(), redirects - 1).then(
            resolveResponse,
            reject,
          );
          return;
        }
        if (status !== 200) {
          incoming.resume();
          reject(new Error(`Download failed with HTTP ${status}: ${url}`));
          return;
        }
        resolveResponse(incoming);
      },
    );
    request.on("error", reject);
    request.setTimeout(REQUEST_TIMEOUT_MS, () => {
      request.destroy(new Error(`Download timed out: ${url}`));
    });
  });
}

async function downloadText(url: string): Promise<string> {
  const incoming = await response(url);
  const chunks: Buffer[] = [];
  let totalBytes = 0;
  for await (const chunk of incoming) {
    const buffer = Buffer.isBuffer(chunk) ? chunk : Buffer.from(chunk);
    totalBytes += buffer.length;
    if (totalBytes > MAX_MANIFEST_BYTES) {
      incoming.destroy();
      throw new Error(`Checksum manifest exceeds ${MAX_MANIFEST_BYTES} bytes`);
    }
    chunks.push(buffer);
  }
  return Buffer.concat(chunks).toString("utf8");
}

async function downloadFile(url: string, destination: string): Promise<void> {
  const incoming = await response(url);
  await pipeline(incoming, createWriteStream(destination, { mode: 0o600 }));
}

async function sha256(file: string): Promise<string> {
  const hash = createHash("sha256");
  for await (const chunk of createReadStream(file)) {
    hash.update(chunk);
  }
  return hash.digest("hex");
}

async function installRelease(
  context: vscode.ExtensionContext,
  force: boolean,
  log: (message: string) => void,
): Promise<string> {
  const configuration = vscode.workspace.getConfiguration("compact");
  const repository = configuration.get<string>(
    "server.repository",
    "lowhung/compact-lsp",
  );
  const version = configuration.get<string>("server.version", "latest");
  const artifact = platformArtifact(process.platform, process.arch);
  const storage = context.globalStorageUri.fsPath;
  const installKey = `${repository.replace("/", "-")}-${version}-${artifact.assetName.replace(/\.(tar\.gz|zip)$/u, "")}`;
  const destination = join(storage, "servers", installKey);
  const binary = join(destination, artifact.binaryName);

  if (!force && (await isExecutable(binary))) {
    return binary;
  }

  await mkdir(storage, { recursive: true });
  const temporary = await mkdtemp(join(storage, "install-"));
  const archive = join(temporary, artifact.assetName);
  const extracted = join(temporary, "extracted");
  const base = releaseDownloadBase(repository, version);

  try {
    await mkdir(extracted, { recursive: true });
    log(`Downloading ${artifact.assetName} from ${repository}@${version}`);
    const manifest = await downloadText(`${base}/SHA256SUMS`);
    const expected = parseSha256Manifest(manifest, artifact.assetName);
    await downloadFile(`${base}/${artifact.assetName}`, archive);
    const actual = await sha256(archive);
    if (actual !== expected) {
      throw new Error(
        `Checksum mismatch for ${artifact.assetName}: expected ${expected}, received ${actual}`,
      );
    }

    if (artifact.archiveKind === "zip") {
      await extractZip(archive, { dir: extracted });
    } else {
      await tar.x({ file: archive, cwd: extracted, strict: true });
    }

    const extractedBinary = join(extracted, artifact.binaryName);
    if (!(await isExecutable(extractedBinary))) {
      if (process.platform === "win32") {
        await access(extractedBinary, fsConstants.F_OK);
      } else {
        await chmod(extractedBinary, 0o755);
      }
    }
    await mkdir(dirname(destination), { recursive: true });
    await rm(destination, { recursive: true, force: true });
    await rename(extracted, destination);
    log(`Installed compact-lsp at ${binary}`);
    return binary;
  } finally {
    await rm(temporary, { recursive: true, force: true });
  }
}

export async function resolveServerPath(
  context: vscode.ExtensionContext,
  forceDownload: boolean,
  log: (message: string) => void,
): Promise<string> {
  const configuration = vscode.workspace.getConfiguration("compact");
  const configured = configuration.get<string>("server.path", "").trim();
  if (configured.length > 0 && !forceDownload) {
    const server = expandPath(configured);
    if (!(await isExecutable(server))) {
      throw new Error(`Configured compact.server.path is not executable: ${server}`);
    }
    return server;
  }

  if (!forceDownload) {
    const discovered = await findOnPath(
      process.platform === "win32" ? "compact-lsp.exe" : "compact-lsp",
    );
    if (discovered) {
      log(`Using compact-lsp from PATH: ${discovered}`);
      return discovered;
    }
  }

  if (
    !forceDownload &&
    !configuration.get<boolean>("server.autoDownload", true)
  ) {
    throw new Error(
      "compact-lsp was not found. Configure compact.server.path or enable compact.server.autoDownload.",
    );
  }

  if (!installation) {
    installation = installRelease(context, forceDownload, log).finally(() => {
      installation = undefined;
    });
  }
  return installation;
}

export function serverEnvironment(): NodeJS.ProcessEnv {
  const resource = vscode.workspace.workspaceFolders?.[0]?.uri;
  const configuration = vscode.workspace.getConfiguration("compact", resource);
  const environment: NodeJS.ProcessEnv = { ...process.env };
  const toolchainVersion = configuration
    .get<string>("toolchain.version", "0.33.0")
    .trim();
  const compiler = configuration.get<string>("compiler.path", "").trim();
  const formatter = configuration.get<string>("formatter.path", "").trim();
  const compilerArguments = configuration.get<string[]>(
    "compiler.arguments",
    [],
  );

  if (toolchainVersion) {
    environment.COMPACT_TOOLCHAIN_VERSION = toolchainVersion;
  }
  if (compiler) {
    environment.COMPACT_COMPILER = expandPath(compiler);
  }
  if (formatter) {
    environment.COMPACT_FORMATTER = expandPath(formatter);
  }
  if (compilerArguments.length > 0) {
    environment.COMPACT_COMPILER_ARGS = JSON.stringify(compilerArguments);
  }
  return environment;
}
