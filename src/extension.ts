import * as vscode from 'vscode';
import { dfmSwap } from './dfmSwap/command';
import { DfmLanguageProvider } from './dfmLanguageSupport/provider';

export function activate(context: vscode.ExtensionContext): void {
  const swapCommand = vscode.commands.registerCommand('delphi-utils.swapToDfmPas', dfmSwap);
  const definitionProvider = vscode.languages.registerDefinitionProvider(
    { language: 'delphi-dfm', scheme: 'file' }, new DfmLanguageProvider());

  context.subscriptions.push(swapCommand, definitionProvider);
}

export function deactivate(): void {}
