import { ExtensionContext, commands, languages, window } from 'vscode';
import { dfmSwap } from './dfmSwap/command';
import { DfmLanguageProvider } from './dfmLanguageSupport/provider';
import { Runtime } from './runtime';
import { DFM, Projects } from './constants';
import { Commands } from './projects/commands';

export async function activate(context: ExtensionContext): Promise<void> {
  await Runtime.initialize(context);
  const swapCommand = commands.registerCommand(DFM.Commands.SwapToDfmPas, dfmSwap);
  const definitionProvider = languages.registerDefinitionProvider(
    { language: 'delphi-devkit.dfm', scheme: 'file' }, new DfmLanguageProvider());

  // Register Delphi Projects Explorer
  const projectsTreeView = window.createTreeView(Projects.View.Main, {
    treeDataProvider: Runtime.projectsTreeView,
    dragAndDropController: Runtime.projectsTreeView.dragAndDropController
  });

  // Register Delphi Projects context menu commands
  Commands.register();
  context.subscriptions.push(
    swapCommand,
    definitionProvider,
    projectsTreeView,
  );
}

export async function deactivate(): Promise<void> { }
