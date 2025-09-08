import { TreeItemCollapsibleState, ThemeIcon, Uri } from 'vscode';
import { BaseFileItem, MainProjectItem } from './baseFile';
import { DelphiProjectTreeItemType } from '../../../types';
import { DprojFileItem } from './dprojFile';
import { DprFileItem } from './dprFile';
import { IniFileItem } from './iniFile';
import { ExeFileItem } from './exeFile';
import { DpkFileItem } from './dpkFile';
import { basename } from 'path';
import { Entities } from '../../../db/entities';
import { Runtime } from '../../../runtime';
import { SortedItem } from '../../../utils/lexoSorter';
import { fileExists } from '../../../utils';
import { PROJECTS } from '../../../constants';

export class ProjectItem extends BaseFileItem implements SortedItem, MainProjectItem {
  public entity: Entities.Project;
  public children: BaseFileItem[] = [];

  constructor(
    public link: Entities.ProjectLink,
    selected: boolean = false
  ) {
    const entity = link.project;
    const path = entity.dproj || entity.dpr || entity.dpk || entity.exe || entity.ini;
    if (!path) throw new Error('At least one project file must be provided.');
    const uriPath = path.replace(basename(path), entity.name);
    if (selected) {
      Runtime.setContext(PROJECTS.CONTEXT.IS_PROJECT_SELECTED, true);
      Runtime.setContext(PROJECTS.CONTEXT.DOES_SELECTED_PROJECT_HAVE_EXE, !!entity.exe);
    }
    let resourceUri: Uri;
    if (Runtime.projects.isCurrentlyCompiling(entity))
      resourceUri = Uri.from({ scheme: PROJECTS.SCHEME.COMPILING, path: uriPath });
    else
      resourceUri = selected
        ? Uri.from({ scheme: PROJECTS.SCHEME.SELECTED, path: uriPath })
        : Uri.from({ scheme: PROJECTS.SCHEME.DEFAULT, path: uriPath });
    super(DelphiProjectTreeItemType.Project, entity.name, resourceUri);
    this.entity = entity;
    this.project = this;
    this.contextValue = PROJECTS.CONTEXT.PROJECT;
    this.setIcon();
    this.updateCollapsibleState();
  }

  public set sortValue(value: string) {
    this.link.sortValue = value;
  }

  public get sortValue(): string {
    return this.link.sortValue;
  }

  public static fromData(link: Entities.ProjectLink): ProjectItem {
    const config = Runtime.configEntity;
    const project = new ProjectItem(link, config.selectedProject?.id === link.project.id);
    return project;
  }

  setIcon(): void {
    if (this.projectDpk) this.iconPath = new ThemeIcon('package');
    else if (this.projectDpr) this.iconPath = new ThemeIcon('run');
    else this.iconPath = new ThemeIcon('symbol-class');
  }

  // Update collapsible state based on children
  updateCollapsibleState(): void {
    const hasChildren = !!(this.projectDproj || this.projectDpr || this.projectDpk || this.projectExe || this.projectIni);
    this.collapsibleState = hasChildren ? TreeItemCollapsibleState.Collapsed : TreeItemCollapsibleState.None;
  }

  async setDproj(value: string): Promise<void> {
    this.entity.dproj = value;
    await Runtime.db.save(this.entity);
  }

  async setDpr(value: string): Promise<void> {
    this.entity.dpr = value;
    await Runtime.db.save(this.entity);
  }

  async setDpk(value: string): Promise<void> {
    this.entity.dpk = value;
    await Runtime.db.save(this.entity);
  }

  async setExecutable(value: string): Promise<void> {
    this.entity.exe = value;
    await Runtime.db.save(this.entity);
  }

  async setIni(value: string): Promise<void> {
    this.entity.ini = value;
    await Runtime.db.save(this.entity);
  }

  createChild(type: DelphiProjectTreeItemType, children: BaseFileItem[]): void {
    let item: BaseFileItem | undefined = undefined;
    let uri: Uri | undefined | null = null;
    switch (type) {
      case DelphiProjectTreeItemType.DprojFile:
        uri = this.projectDproj;
        if (uri?.fsPath)
          item = new DprojFileItem(
            this,
            basename(uri!.fsPath),
            fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.DprFile:
        uri = this.projectDpr;
        if (uri?.fsPath)
          item = new DprFileItem(
            this,
            basename(uri!.fsPath),
            fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.DpkFile:
        uri = this.projectDpk;
        if (uri?.fsPath)
          item = new DpkFileItem(
            this,
            basename(uri!.fsPath),
            fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.ExecutableFile:
        uri = this.projectExe;
        if (uri?.fsPath)
          item = new ExeFileItem(
            this,
            basename(uri!.fsPath),
            fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
        );

        break;
      case DelphiProjectTreeItemType.IniFile:
        uri = this.projectIni;
        if (uri?.fsPath)
          item = new IniFileItem(
            this,
            basename(uri!.fsPath),
            fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
        );

        break;
    }
    if (item) {
      item.project = this;
      children.push(item);
    }
    this.children = children;
  }
}
