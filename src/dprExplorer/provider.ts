import { TreeItem, TreeDataProvider, TreeItemCollapsibleState, EventEmitter, Event, ThemeIcon, Uri, workspace, RelativePattern, ConfigurationChangeEvent } from 'vscode';
import { basename, dirname, join } from 'path';
import { minimatch } from 'minimatch';
import { promises as fs } from 'fs';

export class DprFile extends TreeItem {
  constructor(
    public readonly label: string,
    public readonly resourceUri: Uri,
    public readonly collapsibleState: TreeItemCollapsibleState = TreeItemCollapsibleState.None
  ) {
    super(label, collapsibleState);
    this.tooltip = this.resourceUri.fsPath;
    this.description = dirname(this.resourceUri.fsPath);
    this.command = {
      command: 'vscode.open',
      title: 'Open DPR File',
      arguments: [this.resourceUri]
    };
    this.iconPath = new ThemeIcon('file-code');
    this.contextValue = 'dprFile';
  }
}

export class DprExplorerProvider implements TreeDataProvider<DprFile> {
  private _onDidChangeTreeData: EventEmitter<DprFile | undefined | null | void> = new EventEmitter<DprFile | undefined | null | void>();
  readonly onDidChangeTreeData: Event<DprFile | undefined | null | void> = this._onDidChangeTreeData.event;
  private configFileName = 'delphi-utils-dpr-list.json';

  constructor() {
    // Watch for file system changes to refresh the tree (case-insensitive pattern)
    const watcher = workspace.createFileSystemWatcher('**/*.[Dd][Pp][Rr]');
    watcher.onDidCreate(() => {
      this.refresh();
      this.saveDprListToConfig();
    });
    watcher.onDidDelete(() => {
      this.refresh();
      this.saveDprListToConfig();
    });
    watcher.onDidChange(() => this.refresh());

    // Watch for configuration changes
    workspace.onDidChangeConfiguration((event: ConfigurationChangeEvent) => {
      if (event.affectsConfiguration('delphi-utils.dprExplorer.excludePatterns')) {
        this.refresh();
        this.saveDprListToConfig();
      }
    });
  }

  private async getConfigFilePath(): Promise<string | null> {
    if (!workspace.workspaceFolders || workspace.workspaceFolders.length === 0) {
      return null;
    }

    const workspaceRoot = workspace.workspaceFolders[0].uri.fsPath;
    const vscodeDir = join(workspaceRoot, '.vscode');

    // Ensure .vscode directory exists
    try {
      await fs.access(vscodeDir);
    } catch {
      await fs.mkdir(vscodeDir, { recursive: true });
    }

    return join(vscodeDir, this.configFileName);
  }

  private async saveDprListToConfig(): Promise<void> {
    const configPath = await this.getConfigFilePath();
    if (!configPath) {
      return;
    }

    try {
      const dprFiles = await this.getAllDprFiles();
      const configData = {
        lastUpdated: new Date().toISOString(),
        dprFiles: dprFiles.map(file => ({
          name: file.label,
          path: workspace.asRelativePath(file.resourceUri),
          absolutePath: file.resourceUri.fsPath
        }))
      };

      await fs.writeFile(configPath, JSON.stringify(configData, null, 2), 'utf8');
    } catch (error) {
      console.error('Failed to save DPR list to config:', error);
    }
  }

  private async loadDprListFromConfig(): Promise<any> {
    const configPath = await this.getConfigFilePath();
    if (!configPath) {
      return null;
    }

    try {
      const configContent = await fs.readFile(configPath, 'utf8');
      return JSON.parse(configContent);
    } catch {
      // Config file doesn't exist or is invalid, return null
      return null;
    }
  }

  private async getAllDprFiles(): Promise<DprFile[]> {
    if (!workspace.workspaceFolders) {
      return [];
    }

    const dprFiles: DprFile[] = [];

    // Get exclude patterns from configuration
    const config = workspace.getConfiguration('delphi-utils.dprExplorer');
    const excludePatterns: string[] = config.get('excludePatterns', []);

    for (const folder of workspace.workspaceFolders) {
      // Search for DPR files with case-insensitive pattern
      const pattern = new RelativePattern(folder, '**/*.[Dd][Pp][Rr]');
      const files = await workspace.findFiles(pattern);

      for (const file of files) {
        const relativePath = workspace.asRelativePath(file, false);

        // Check if file should be excluded based on patterns
        const shouldExclude = excludePatterns.some(pattern =>
          minimatch(relativePath, pattern, { matchBase: true })
        );

        if (!shouldExclude) {
          const fileName = basename(file.fsPath);
          dprFiles.push(new DprFile(fileName, file));
        }
      }
    }

    return dprFiles;
  }

  refresh(): void {
    this._onDidChangeTreeData.fire();
  }

  getTreeItem(element: DprFile): TreeItem {
    return element;
  }

  async getChildren(element?: DprFile): Promise<DprFile[]> {
    if (!element) {
      // Root level - get all DPR files and save to config
      const dprFiles = await this.getAllDprFiles();

      // Sort files alphabetically
      dprFiles.sort((a, b) => a.label.localeCompare(b.label));

      // Save the current list to config file (async, don't wait)
      this.saveDprListToConfig().catch(error => {
        console.error('Failed to save DPR list:', error);
      });

      return dprFiles;
    }

    return [];
  }
}
