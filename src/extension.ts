import { ExtensionContext, commands, languages, window } from 'vscode';
import { dfmSwap } from './dfmSwap/command';
import { DfmLanguageProvider } from './dfmLanguageSupport/provider';
import { DprExplorerProvider } from './dprExplorer/provider';

export function activate(context: ExtensionContext): void {
  const swapCommand = commands.registerCommand('delphi-utils.swapToDfmPas', dfmSwap);
  const definitionProvider = languages.registerDefinitionProvider(
    { language: 'delphi-dfm', scheme: 'file' }, new DfmLanguageProvider());

  // Register DPR Explorer
  const dprExplorerProvider = new DprExplorerProvider();
  const dprTreeView = window.createTreeView('dprExplorer', {
    treeDataProvider: dprExplorerProvider
  });

  const refreshDprCommand = commands.registerCommand('delphi-utils.refreshDprExplorer', () => {
    dprExplorerProvider.refresh();
  });

  context.subscriptions.push(
    swapCommand,
    definitionProvider,
    dprTreeView,
    refreshDprCommand
  );
}

export function deactivate(): void {}
