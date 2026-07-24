import { TreeItemCollapsibleState, ThemeIcon, Uri, MarkdownString, workspace } from 'vscode';
import { BaseFileItem, MainProjectItem } from './baseFile';
import { DelphiProjectTreeItemType } from '../../../types';
import { DprojFileItem } from './dprojFile';
import { DprFileItem } from './dprFile';
import { IniFileItem } from './iniFile';
import { ExeFileItem } from './exeFile';
import { DpkFileItem } from './dpkFile';
import { ConfigurationGroupItem, PlatformGroupItem } from './configurationItem';
import { basename } from 'path';
import { Entities } from '../../entities';
import { Runtime } from '../../../runtime';
import { fileExists } from '../../../utils';
import { PROJECTS } from '../../../constants';
import { DprojMetadata } from '../../../client';

/**
 * Hover tooltip summarizing how the project runs: its exe, the effective
 * Host Application (with origin — DevKit override vs the dproj's own value)
 * and the effective run parameters (with origin — saved Start Parameters,
 * the dproj's Debugger_RunParams, or both fused).
 */
function buildProjectTooltip(entity: Entities.Project): MarkdownString {
  const useDprojRunParams = workspace.getConfiguration(PROJECTS.CONFIG.KEY).get<boolean>(PROJECTS.CONFIG.USE_DEBUGGER_RUN_PARAMS, true);
  const markdown = new MarkdownString();
  markdown.appendMarkdown(`**${entity.name}**`);
  const overrides = [entity.active_configuration, entity.active_platform].filter(Boolean).join(' · ');
  if (overrides)
    markdown.appendMarkdown(` (${overrides})`);

  const appendRow = (label: string, value?: string, origin?: string) => {
    if (!value) return;
    markdown.appendMarkdown(`\n\n${label}: \`${value}\``);
    if (origin)
      markdown.appendMarkdown(` — ${origin}`);
  };

  appendRow('Exe', entity.exe ?? undefined);
  appendRow(
    'Host',
    Entities.effectiveHostApplication(entity),
    entity.host_application?.trim() ? 'override' : 'from dproj'
  );

  const runParamsOrigin = () => {
    const hasSaved = !!entity.start_parameters?.trim();
    const hasDproj = !!entity.dproj_run_params?.trim() && useDprojRunParams;
    if (hasSaved && hasDproj) return 'dproj + saved';
    if (hasSaved) return 'saved';
    if (hasDproj) return 'from dproj';
    return undefined;
  };
  appendRow('Run params', Entities.resolveEffectiveStartParameters(entity, useDprojRunParams), runParamsOrigin());

  return markdown;
}

export class ProjectItem extends BaseFileItem implements MainProjectItem {
  public entity: Entities.Project;
  public children: BaseFileItem[] = [];
  /** Populated lazily when the tree expands this project. */
  public dprojMetadata?: DprojMetadata;

  constructor(
    public link: Entities.ProjectLink,
    selected: boolean = false
  ) {
    const projectEntity = Runtime.getProjectOfLink(link);
    if (!projectEntity) throw new Error('Project link does not have an associated project.');
    const path = projectEntity.dproj || projectEntity.dpr || projectEntity.dpk || projectEntity.exe || projectEntity.ini;
    if (!path) throw new Error('At least one project file must be provided.');
    const uriPath = path.replace(basename(path), projectEntity.name);
    if (selected) {
      Runtime.setContext(PROJECTS.CONTEXT.IS_PROJECT_SELECTED, true);
      Runtime.setContext(PROJECTS.CONTEXT.DOES_SELECTED_PROJECT_HAVE_EXE, !!Entities.resolveRunTarget(projectEntity));
    }
    const resourceUri = selected
        ? Uri.from({ scheme: PROJECTS.SCHEME.SELECTED, path: uriPath })
        : Uri.from({ scheme: PROJECTS.SCHEME.DEFAULT, path: uriPath });
    super(DelphiProjectTreeItemType.Project, projectEntity.name, resourceUri);
    this.entity = projectEntity;
    this.project = this;
    this.contextValue = PROJECTS.CONTEXT.PROJECT;
    this.tooltip = buildProjectTooltip(projectEntity);
    // Inline cue that this project runs through a hosting executable (a .dpk
    // package or DLL): the full path and origin live in the tooltip.
    const hostApplication = Entities.effectiveHostApplication(projectEntity);
    if (hostApplication)
      this.description = `⇢ ${basename(hostApplication)}`;
    this.setIcon();
    this.updateCollapsibleState();
  }

  public static fromData(link: Entities.ProjectLink): ProjectItem {
    const data = Runtime.projectsData;
    if (!data) throw new Error('Projects data is not loaded.');
    const project = new ProjectItem(link, (Runtime.activeProject?.id || -1) === (Runtime.getProjectOfLink(link)?.id || -2));
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

  async createChild(type: DelphiProjectTreeItemType, children: BaseFileItem[]): Promise<void> {
    let item: BaseFileItem | undefined = undefined;
    let uri: Uri | undefined | null = null;
    switch (type) {
      case DelphiProjectTreeItemType.DprojFile:
        uri = this.projectDproj;
        if (uri?.fsPath)
          item = new DprojFileItem(
            this,
            basename(uri!.fsPath),
            await fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.DprFile:
        uri = this.projectDpr;
        if (uri?.fsPath)
          item = new DprFileItem(
            this,
            basename(uri!.fsPath),
            await fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.DpkFile:
        uri = this.projectDpk;
        if (uri?.fsPath)
          item = new DpkFileItem(
            this,
            basename(uri!.fsPath),
            await fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
          );

        break;
      case DelphiProjectTreeItemType.ExecutableFile:
        uri = this.projectExe;
        if (uri?.fsPath)
          item = new ExeFileItem(
            this,
            basename(uri!.fsPath),
            await fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
        );

        break;
      case DelphiProjectTreeItemType.IniFile:
        uri = this.projectIni;
        if (uri?.fsPath)
          item = new IniFileItem(
            this,
            basename(uri!.fsPath),
            await fileExists(uri) ? uri : Uri.from({ scheme: PROJECTS.SCHEME.MISSING, path: uri.fsPath })
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
