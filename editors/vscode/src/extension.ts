import * as vscode from "vscode";
import {
  LanguageClient,
  type LanguageClientOptions,
  type ServerOptions,
} from "vscode-languageclient/node";

import {
  resolveServerPath,
  serverEnvironment,
} from "./serverInstaller.js";

let client: LanguageClient | undefined;
let output: vscode.LogOutputChannel | undefined;

function append(message: string): void {
  output?.appendLine(`[extension] ${message}`);
}

async function stopClient(): Promise<void> {
  if (client) {
    const running = client;
    client = undefined;
    await running.stop();
  }
}

async function startClient(
  context: vscode.ExtensionContext,
  forceDownload = false,
): Promise<void> {
  await stopClient();
  const serverPath = await vscode.window.withProgress(
    {
      location: vscode.ProgressLocation.Notification,
      title: forceDownload
        ? "Installing Compact language server"
        : "Starting Compact language server",
      cancellable: false,
    },
    () => resolveServerPath(context, forceDownload, append),
  );

  append(`Starting ${serverPath}`);
  const serverOptions: ServerOptions = {
    command: serverPath,
    options: {
      env: serverEnvironment(),
    },
  };
  const clientOptions: LanguageClientOptions = {
    documentSelector: [{ scheme: "file", language: "compact" }],
    outputChannel: output,
  };
  client = new LanguageClient(
    "compact-lsp",
    "Compact Language Server",
    serverOptions,
    clientOptions,
  );
  await client.start();
}

async function reportStartupError(error: unknown): Promise<void> {
  const message = error instanceof Error ? error.message : String(error);
  append(`Startup failed: ${message}`);
  const action = await vscode.window.showErrorMessage(
    `Compact language server could not start: ${message}`,
    "Configure Server Path",
    "Open Releases",
    "Show Output",
  );
  if (action === "Configure Server Path") {
    await vscode.commands.executeCommand(
      "workbench.action.openSettings",
      "compact.server.path",
    );
  } else if (action === "Open Releases") {
    const repository = vscode.workspace
      .getConfiguration("compact")
      .get<string>("server.repository", "lowhung/compact-lsp");
    await vscode.env.openExternal(
      vscode.Uri.parse(`https://github.com/${repository}/releases`),
    );
  } else if (action === "Show Output") {
    output?.show();
  }
}

export async function activate(
  context: vscode.ExtensionContext,
): Promise<void> {
  output = vscode.window.createOutputChannel("Compact Language Server", {
    log: true,
  });
  context.subscriptions.push(output);
  context.subscriptions.push(
    vscode.commands.registerCommand("compact.showOutput", () => output?.show()),
    vscode.commands.registerCommand("compact.restartServer", async () => {
      try {
        await startClient(context);
      } catch (error) {
        await reportStartupError(error);
      }
    }),
    vscode.commands.registerCommand("compact.installServer", async () => {
      try {
        await startClient(context, true);
        await vscode.window.showInformationMessage(
          "Compact language server installed and started.",
        );
      } catch (error) {
        await reportStartupError(error);
      }
    }),
  );

  try {
    await startClient(context);
  } catch (error) {
    await reportStartupError(error);
  }
}

export async function deactivate(): Promise<void> {
  await stopClient();
}
