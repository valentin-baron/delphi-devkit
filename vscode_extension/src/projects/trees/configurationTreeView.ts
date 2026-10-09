import {
  TreeItem, TreeItemCollapsibleState, TreeDataProvider,
  EventEmitter, Event, ThemeIcon, Uri, window, commands
} from 'vscode';
import { join } from 'path';
import { promises as fs } from 'fs';
import { env as osEnv } from 'process';
import { PROJECTS } from '../../constants';
import { Runtime } from '../../runtime';

async function ensureFile(filePath: string, defaultContent: string = ''): Promise<string> {
  try {
    await fs.access(filePath);
  } catch {
    const dir = join(filePath, '..');
    await fs.mkdir(dir, { recursive: true });
    await fs.writeFile(filePath, defaultContent, 'utf8');
  }
  return filePath;
}

const DEFAULT_INI_CONTENT = `; Default INI template – used as the starting content\n; when DDK creates a new .ini file for a project.\n[CmdLineParam]\n`;

class ConfigFileItem extends TreeItem {
  constructor(
    label: string,
    public readonly openAction: () => Promise<void>,
    icon: string,
    description?: string
  ) {
    super(label, TreeItemCollapsibleState.None);
    this.iconPath = new ThemeIcon(icon);
    this.description = description;
    this.command = {
      command: 'ddk.configuration.openItem',
      title: 'Open',
      arguments: [this]
    };
  }
}

export class ConfigurationTreeView implements TreeDataProvider<TreeItem> {
  private changeEmitter = new EventEmitter<void>();
  public readonly onDidChangeTreeData: Event<void> = this.changeEmitter.event;

  public static get ddkDir(): string {
    return join(osEnv.APPDATA || osEnv.HOME || '', 'ddk');
  }

  constructor() {
    Runtime.extension.subscriptions.push(
      window.createTreeView(PROJECTS.VIEW.CONFIGURATION, {
        treeDataProvider: this,
        showCollapseAll: false
      }),
      commands.registerCommand('ddk.configuration.openItem', (item: ConfigFileItem) => item.openAction())
    );
  }

  getTreeItem(element: TreeItem): TreeItem {
    return element;
  }

  async getChildren(): Promise<TreeItem[]> {
    const ddkDir = ConfigurationTreeView.ddkDir;
    return [
      new ConfigFileItem(
        'Default INI',
        async () => {
          const path = await ensureFile(join(ddkDir, 'default.ini'), DEFAULT_INI_CONTENT);
          await commands.executeCommand('vscode.open', Uri.file(path));
        },
        'file',
        'Template for new .ini files'
      ),
      new ConfigFileItem(
        'Projects Data',
        async () => {
          const path = await ensureFile(join(ddkDir, 'projects.ron'), '// Created by DDK\n');
          await commands.executeCommand('vscode.open', Uri.file(path));
        },
        'notebook',
        'projects.ron'
      ),
      new ConfigFileItem(
        'Compiler Configurations',
        async () => {
          const path = await ensureFile(join(ddkDir, 'compilers.ron'), '// Created by DDK\n');
          await commands.executeCommand('vscode.open', Uri.file(path));
        },
        'tools',
        'compilers.ron'
      ),
      new ConfigFileItem(
        'Formatter Configuration',
        async () => {
          const target = join(ddkDir, 'ddk_formatter.config');
          try { await fs.access(target); } catch {
            const bundled = Runtime.extension.asAbsolutePath('dist/ddk_formatter.config');
            try {
              const content = await fs.readFile(bundled, 'utf8');
              await fs.mkdir(ddkDir, { recursive: true });
              await fs.writeFile(target, content, 'utf8');
            } catch {
              await ensureFile(target, '');
            }
          }
          await commands.executeCommand('vscode.open', Uri.file(target));
        },
        'symbol-misc',
        'ddk_formatter.config'
      ),
      new ConfigFileItem(
        'Extension Settings',
        async () => {
          await commands.executeCommand('workbench.action.openSettings', '@ext:Snowcaloid.delphi-devkit');
        },
        'settings-gear',
        'VS Code settings for DDK'
      ),
    ];
  }

  public refresh(): void {
    this.changeEmitter.fire();
  }
}
