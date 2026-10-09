import { CancellationToken, ExtensionMode, lm, McpStdioServerDefinition } from 'vscode';
import type { McpServerDefinitionProvider } from 'vscode';
import { Feature } from '../types';
import { Runtime } from '../runtime';
import { join } from 'path';
import { existsSync } from 'fs';

/**
 * Registers ddk-mcp-server (a Rust STDIO binary) with VS Code's MCP
 * infrastructure, which spawns it as a child process and speaks MCP over its
 * stdin/stdout. State is shared with ddk-server through RON files on disk.
 */
export class McpServerFeature implements Feature, McpServerDefinitionProvider<McpStdioServerDefinition> {
  async initialize(): Promise<void> {
    Runtime.extension.subscriptions.push(
      lm.registerMcpServerDefinitionProvider('ddk.mcp', this),
    );
  }

  provideMcpServerDefinitions(_token: CancellationToken) {
    const serverPath = this.resolveServerPath();
    if (!existsSync(serverPath)) {
      console.warn(`[DDK] ddk-mcp-server not found at: ${serverPath}`);
      return [];
    }
    return [new McpStdioServerDefinition('DDK - Delphi Development Kit', serverPath, [])];
  }

  private resolveServerPath(): string {
    const ext = Runtime.extension;
    const isDev = ext.extensionMode !== ExtensionMode.Production;
    return isDev
      ? join(ext.extensionUri.fsPath, '..', 'target', 'debug', 'ddk-mcp-server.exe')
      : join(ext.extensionUri.fsPath, 'server', 'ddk-mcp-server.exe');
  }
}


