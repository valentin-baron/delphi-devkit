import { ExtensionContext, commands, languages, window, Uri, env } from 'vscode';
import { dfmSwap } from './dfmSwap/command';
import { DfmLanguageProvider } from './dfmLanguageSupport/provider';
import { Runtime } from './runtime';
import { DelphiProjectContextMenuCommands } from './projects/contextMenu/commands';
import { DFM, Projects } from './constants';

export async function activate(context: ExtensionContext): Promise<void> {
  await Runtime.initialize(context);
  const swapCommand = commands.registerCommand(DFM.Commands.SwapToDfmPas, dfmSwap);
  const definitionProvider = languages.registerDefinitionProvider(
    { language: 'delphi-devkit.dfm', scheme: 'file' }, new DfmLanguageProvider());

  // Register Delphi Projects Explorer
  const projectsTreeView = window.createTreeView(Projects.View.Main, {
    treeDataProvider: Runtime.projectsProvider,
    dragAndDropController: Runtime.projectsProvider.dragAndDropController
  });

  const launchExecutableCommand = commands.registerCommand(Projects.Command.LaunchExecutable, async (uri: Uri) => {
    try {
      // Use the system's default application handler to launch the executable
      await env.openExternal(uri);
    } catch (error) {
      window.showErrorMessage(`Failed to launch executable: ${error}`);
    }
  });

  // Register Delphi Projects context menu commands
  const contextMenuCommands = DelphiProjectContextMenuCommands.registerCommands();

  context.subscriptions.push(
    swapCommand,
    definitionProvider,
    projectsTreeView,
    launchExecutableCommand,
    ...contextMenuCommands,
  );
}

export function deactivate(): void {}
