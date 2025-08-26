import { ExtensionContext } from 'vscode';
import { Runtime } from './runtime';

export async function activate(context: ExtensionContext): Promise<void> {
  await Runtime.initialize(context);
}

export async function deactivate(): Promise<void> { }
