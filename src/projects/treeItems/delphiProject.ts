import { TreeItemCollapsibleState, ThemeIcon, Uri } from "vscode";
import { DelphiProjectMainTreeItem, DelphiProjectTreeItem } from "./delphiProjectTreeItem";
import { DelphiProjectTreeItemType } from "../../types";
import { DprojFile } from "./dprojFile";
import { DprFile } from "./dprFile";
import { IniFile } from "./iniFile";
import { ExeFile } from "./exeFile";
import { DpkFile } from "./dpkFile";
import { basename } from "path";
import { Entities } from "../../db/entities";
import { Runtime } from "../../runtime";
import { SortedItem } from "../../utils/lexoSorter";
import { fileExists } from "../../utils";
import { PROJECTS } from "../../constants";

export class DelphiProject extends DelphiProjectTreeItem implements DelphiProjectMainTreeItem, SortedItem {
  public entity: Entities.Project;
  public children: DelphiProjectTreeItem[] = [];

  constructor(
    public link: Entities.ProjectLink,
    selected: boolean = false
  ) {
    const entity = link.project;
    const path = entity.dproj || entity.dpr || entity.dpk || entity.exe || entity.ini;
    if (!path) { throw new Error("At least one project file must be provided."); }
    const uriPath = path.replace(basename(path), entity.name);
    if (selected) {
      Runtime.setContext(PROJECTS.CONTEXT.IS_PROJECT_SELECTED, true);
      Runtime.setContext(PROJECTS.CONTEXT.DOES_SELECTED_PROJECT_HAVE_EXE, !!entity.exe);
    }
    const resourceUri = selected ?
      Uri.from({ scheme: PROJECTS.SCHEME.SELECTED, path: uriPath }) :
      Uri.from({ scheme: PROJECTS.SCHEME.DEFAULT, path: uriPath });
    super(
      DelphiProjectTreeItemType.Project,
      entity.name,
      resourceUri
    );
    this.entity = entity;
    this.project = this;
    this.contextValue = PROJECTS.CONTEXT.PROJECT;
    this.setIcon();
  }

  public set sortValue(value: string) {
    this.link.sortValue = value;
  }

  public get sortValue(): string {
    return this.link.sortValue;
  }

  public static fromData(link: Entities.ProjectLink): DelphiProject {
    const project = new DelphiProject(
      link,
      link.owner.selectedProject?.id === link.project.id
    );
    project.updateCollapsibleState();
    return project;
  }

  setIcon(): void {
    if (this.projectDpk) {
      this.iconPath = new ThemeIcon("package");
    } else if (this.projectDpr) {
      this.iconPath = new ThemeIcon("run");
    } else {
      this.iconPath = new ThemeIcon("symbol-class");
    }
  }

  // Update collapsible state based on children
  updateCollapsibleState(): void {
    const hasChildren = !!(
      this.projectDproj ||
      this.projectDpr ||
      this.projectDpk ||
      this.projectExe ||
      this.projectIni
    );
    this.collapsibleState = hasChildren
      ? TreeItemCollapsibleState.Collapsed
      : TreeItemCollapsibleState.None;
  }

  async setDproj(value: string): Promise<void> {
    this.entity.dproj = value;
    await Runtime.db.saveProject(this.entity);
  }

  async setDpr(value: string): Promise<void> {
    this.entity.dpr = value;
    await Runtime.db.saveProject(this.entity);
  }

  async setDpk(value: string): Promise<void> {
    this.entity.dpk = value;
    await Runtime.db.saveProject(this.entity);
  }

  async setExecutable(value: string): Promise<void> {
    this.entity.exe = value;
    await Runtime.db.saveProject(this.entity);
  }

  async setIni(value: string): Promise<void> {
    this.entity.ini = value;
    await Runtime.db.saveProject(this.entity);
  }

  createChild(
    type: DelphiProjectTreeItemType,
    children: DelphiProjectTreeItem[]
  ): void {
    let item: DelphiProjectTreeItem | undefined = undefined;
    switch (type) {
      case DelphiProjectTreeItemType.DprojFile: {
        if (this.projectDproj && fileExists(this.projectDproj)) {
          item = new DprojFile(
            basename(this.projectDproj.fsPath),
            this.projectDproj
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.DprFile: {
        if (this.projectDpr && fileExists(this.projectDpr)) {
          item = new DprFile(
            basename(this.projectDpr.fsPath),
            this.projectDpr
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.DpkFile: {
        if (this.projectDpk && fileExists(this.projectDpk)) {
          item = new DpkFile(
            basename(this.projectDpk.fsPath),
            this.projectDpk,
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.ExecutableFile: {
        if (this.projectExe && fileExists(this.projectExe)) {
          item = new ExeFile(
            basename(this.projectExe.fsPath),
            this.projectExe,
          );
        }
        break;
      }
      case DelphiProjectTreeItemType.IniFile: {
        if (this.projectIni && fileExists(this.projectIni)) {
          item = new IniFile(
            basename(this.projectIni.fsPath),
            this.projectIni,
          );
        }
        break;
      }
    }
    if (item) {
      item.project = this;
      children.push(item);
    }
    this.children = children;
  }
}
