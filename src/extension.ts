import { ExtensionContext, commands, languages, window, Uri, env, workspace, ProgressLocation } from 'vscode';
import { dfmSwap } from './dfmSwap/command';
import { DfmLanguageProvider } from './dfmLanguageSupport/provider';
import { DelphiProjectsProvider, DelphiProjectContextMenuCommands, CompilerStatusBar, Compiler } from './delphiProjects';
import { GroupProjectService } from './delphiProjects/groupProject/GroupProjectService';

export function activate(context: ExtensionContext): void {
  const swapCommand = commands.registerCommand('delphi-utils.swapToDfmPas', dfmSwap);
  const definitionProvider = languages.registerDefinitionProvider(
    { language: 'delphi-dfm', scheme: 'file' }, new DfmLanguageProvider());

  // Register Delphi Projects Explorer
  const delphiProjectsProvider = new DelphiProjectsProvider();
  const dprTreeView = window.createTreeView('delphiProjects', {
    treeDataProvider: delphiProjectsProvider
  });

  const refreshDprCommand = commands.registerCommand('delphi-utils.refreshDelphiProjects', () => {
    delphiProjectsProvider.refresh(true); // Force cache refresh
  });

  const launchExecutableCommand = commands.registerCommand('delphi-utils.launchExecutable', async (uri: Uri) => {
    try {
      // Use the system's default application handler to launch the executable
      await env.openExternal(uri);
    } catch (error) {
      window.showErrorMessage(`Failed to launch executable: ${error}`);
    }
  });

  // Register Delphi Projects context menu commands
  const contextMenuCommands = DelphiProjectContextMenuCommands.registerCommands();

  // Initialize compiler status bar
  const compilerStatusBar = CompilerStatusBar.initialize();
  const compilerStatusBarCommands = CompilerStatusBar.registerCommands();

  const pickGroupProjectCommand = commands.registerCommand('delphi-utils.pickGroupProject', async () => {
    await GroupProjectService.pickGroupProject(delphiProjectsProvider);
  });

  const unloadGroupProjectCommand = commands.registerCommand('delphi-utils.unloadGroupProject', async () => {
    await GroupProjectService.unloadGroupProject(delphiProjectsProvider);
  });

  context.subscriptions.push(
    swapCommand,
    definitionProvider,
    dprTreeView,
    refreshDprCommand,
    launchExecutableCommand,
    ...contextMenuCommands,
    ...compilerStatusBarCommands,
    compilerStatusBar,
    pickGroupProjectCommand,
    unloadGroupProjectCommand
  );
}

export function deactivate(): void {
  // Clean up compiler terminal
  Compiler.dispose();
}
