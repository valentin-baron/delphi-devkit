import { TreeItemCollapsibleState, ThemeIcon, Uri, commands } from "vscode";
import { DelphiProjectMainTreeItem, DelphiProjectTreeItem } from "./delphiProjectTreeItem";
import { DelphiProjectTreeItemType } from "../../types";
import { DprojFile } from "./dprojFile";
import { DprFile } from "./dprFile";
import { IniFile } from "./iniFile";
import { ExeFile } from "./exeFile";
import { DpkFile } from "./dpkFile";
import { basename } from "path";
import { ProjectEntity, WorkspaceEntity } from "../../db/entities";
import { Runtime } from "../../runtime";
import { SortedItem } from "../../utils/lexoSorter";
import { fileExists } from "../../utils";
import { Projects } from "../../constants";

export enum ProjectType {
  Application = "application",
  Package = "package",
}

export class DelphiProject extends DelphiProjectTreeItem implements DelphiProjectMainTreeItem, SortedItem {
  public projectId?: number;
  public dproj?: Uri;
  public dpr?: Uri;
  public dpk?: Uri;
  public exe?: Uri;
  public ini?: Uri;
  public sortValue: string;

  constructor(
    label: string,
    projectType: ProjectType,
    dproj?: Uri,
    dpr?: Uri,
    dpk?: Uri,
    exe?: Uri,
    ini?: Uri,
    selected: boolean = false
  ) {
    const path = dproj || dpr || dpk || exe || ini;
    if (!path) { throw new Error("At least one project file must be provided."); }
    const uriPath = path.fsPath.replace(basename(path.fsPath), label);
    if (selected) {
      commands.executeCommand(
        "setContext",
        Projects.Context.IsProjectSelected,
        true,
      );
      commands.executeCommand(
        "setContext",
        Projects.Context.DoesSelectedProjectHaveExe,
        !!exe
      );
    }
    const resourceUri = selected ?
      Uri.from({ scheme: Projects.Scheme.Selected, path: uriPath }) :
      Uri.from({ scheme: Projects.Scheme.Default, path: uriPath });
    super(
      DelphiProjectTreeItemType.Project,
      label,
      resourceUri,
      projectType,
    );
    this.project = this;
    this.dproj = dproj;
    this.dpr = dpr;
    this.dpk = dpk;
    this.exe = exe;
    this.ini = ini;
    this.contextValue = "delphiProject";
    this.setIcon();
  }

  public static fromData(workspace: WorkspaceEntity, data: ProjectEntity): DelphiProject {
    const selected =
      workspace.currentGroupProject?.currentProject?.id === data.id ||
      workspace.currentProject?.id === data.id;
    const project = new DelphiProject(
      data.name,
      <ProjectType>data.type,
      data.dprojPath ? Uri.file(data.dprojPath) : undefined,
      data.dprPath ? Uri.file(data.dprPath) : undefined,
      data.dpkPath ? Uri.file(data.dpkPath) : undefined,
      data.exePath ? Uri.file(data.exePath) : undefined,
      data.iniPath ? Uri.file(data.iniPath) : undefined,
      selected
    );
    project.projectId = data.id;
    project.sortValue = data.sortValue;
    project.updateCollapsibleState();
    return project;
  }

  setIcon(): void {
    if (this.dpk) {
      this.iconPath = new ThemeIcon("package");
    } else if (this.dpr) {
      this.iconPath = new ThemeIcon("run");
    } else {
      this.iconPath = new ThemeIcon("symbol-class");
    }
  }

  // Get the most appropriate resource URI for commands
  getResourceUri(): Uri {
    if (this.dproj) {
      return this.dproj;
    }
    if (this.dpr) {
      return this.dpr;
    }
    if (this.dpk) {
      return this.dpk;
    }
    return this.resourceUri;
  }

  // Update collapsible state based on children
  updateCollapsibleState(): void {
    const hasChildren = !!(
      this.dproj ||
      this.dpr ||
      this.dpk ||
      this.exe ||
      this.ini
    );
    this.collapsibleState = hasChildren
      ? TreeItemCollapsibleState.Collapsed
      : TreeItemCollapsibleState.None;
  }

  async marshal(): Promise<ProjectEntity> {
    let data = new ProjectEntity();
    data.name = this.label;
    data.type = this.projectType;
    if (this.dproj) {
      data.dprojPath = this.dproj.fsPath;
    }
    if (this.dpr) {
      data.dprPath = this.dpr.fsPath;
    }
    if (this.dpk) {
      data.dpkPath = this.dpk.fsPath;
    }
    if (this.exe) {
      data.exePath = this.exe.fsPath;
    }
    if (this.ini) {
      data.iniPath = this.ini.fsPath;
    }
    if (this.projectId) {
      data.id = this.projectId;
    }
    data.sortValue = this.sortValue;
    return data;
  }

  async setDproj(value: Uri, save: boolean = false): Promise<void> {
    this.dproj = value;
    if (save) {
      await Runtime.projects.treeView.save();
    }
  }

  async setDpr(value: Uri, save: boolean = false): Promise<void> {
    this.dpr = value;
    if (save) {
      await Runtime.projects.treeView.save();
    }
  }

  async setDpk(value: Uri, save: boolean = false): Promise<void> {
    this.dpk = value;
    if (save) {
      await Runtime.projects.treeView.save();
    }
  }

  async setExecutable(value: Uri, save: boolean = false): Promise<void> {
    this.exe = value;
    if (save) {
      await Runtime.projects.treeView.save();
    }
  }

  async setIni(value: Uri, save: boolean = false): Promise<void> {
    this.ini = value;
    if (save) {
      await Runtime.projects.treeView.save();
    }
  }

  createChild(
    type: DelphiProjectTreeItemType,
    children: DelphiProjectTreeItem[]
  ): void {
    let item: DelphiProjectTreeItem | undefined = undefined;
    switch (type) {
      case DelphiProjectTreeItemType.DprojFile: {
        if (this.dproj && fileExists(this.dproj)) {
          item = new DprojFile(
            basename(this.dproj.fsPath),
            this.dproj,
            this.projectType,
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.DprFile: {
        if (this.dpr && fileExists(this.dpr)) {
          item = new DprFile(
            basename(this.dpr.fsPath),
            this.dpr
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.DpkFile: {
        if (this.dpk && fileExists(this.dpk)) {
          item = new DpkFile(
            basename(this.dpk.fsPath),
            this.dpk,
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.ExecutableFile: {
        if (this.exe && fileExists(this.exe)) {
          item = new ExeFile(
            basename(this.exe.fsPath),
            this.exe,
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.IniFile: {
        if (this.ini && fileExists(this.ini)) {
          item = new IniFile(
            basename(this.ini.fsPath),
            this.ini,
          );
        }
        break;
      }
    }
    if (item) {
      item.project = this;
      children.push(item);
    }
  }
}
