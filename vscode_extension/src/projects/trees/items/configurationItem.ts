import { TreeItem, TreeItemCollapsibleState, ThemeIcon } from 'vscode';
import { PROJECTS } from '../../../constants';
import { DprojMetadata } from '../../../client';

export class ConfigurationGroupItem extends TreeItem {
  public readonly projectId: number;
  public readonly projectLinkId: number;
  public readonly metadata: DprojMetadata;

  constructor(projectId: number, projectLinkId: number, metadata: DprojMetadata) {
    super('Configurations', TreeItemCollapsibleState.Collapsed);
    this.projectId = projectId;
    this.projectLinkId = projectLinkId;
    this.metadata = metadata;
    this.contextValue = PROJECTS.CONTEXT.CONFIGURATION_GROUP;
    this.iconPath = new ThemeIcon('symbol-enum');
    this.description = metadata.active_configuration;
  }

  public getChildren(): ConfigurationItem[] {
    return this.metadata.configurations.map(
      (cfg) => new ConfigurationItem(this.projectId, this.projectLinkId, cfg, cfg === this.metadata.active_configuration)
    );
  }
}

export class PlatformGroupItem extends TreeItem {
  public readonly projectId: number;
  public readonly projectLinkId: number;
  public readonly metadata: DprojMetadata;

  constructor(projectId: number, projectLinkId: number, metadata: DprojMetadata) {
    super('Platforms', TreeItemCollapsibleState.Collapsed);
    this.projectId = projectId;
    this.projectLinkId = projectLinkId;
    this.metadata = metadata;
    this.contextValue = PROJECTS.CONTEXT.PLATFORM_GROUP;
    this.iconPath = new ThemeIcon('device-desktop');
    this.description = metadata.active_platform;
  }

  public getChildren(): PlatformItem[] {
    return this.metadata.platforms.map(
      (plat) => new PlatformItem(this.projectId, this.projectLinkId, plat, plat === this.metadata.active_platform)
    );
  }
}

export class ConfigurationItem extends TreeItem {
  public readonly projectId: number;
  public readonly projectLinkId: number;
  public readonly configName: string;

  constructor(projectId: number, projectLinkId: number, configName: string, isActive: boolean) {
    super(configName, TreeItemCollapsibleState.None);
    this.projectId = projectId;
    this.projectLinkId = projectLinkId;
    this.configName = configName;
    this.contextValue = PROJECTS.CONTEXT.CONFIGURATION_ITEM;
    this.iconPath = isActive ? new ThemeIcon('check') : new ThemeIcon('circle-outline');
    this.command = {
      command: PROJECTS.COMMAND.SET_PROJECT_CONFIGURATION,
      title: 'Set Configuration',
      arguments: [this]
    };
  }
}

export class PlatformItem extends TreeItem {
  public readonly projectId: number;
  public readonly projectLinkId: number;
  public readonly platformName: string;

  constructor(projectId: number, projectLinkId: number, platformName: string, isActive: boolean) {
    super(platformName, TreeItemCollapsibleState.None);
    this.projectId = projectId;
    this.projectLinkId = projectLinkId;
    this.platformName = platformName;
    this.contextValue = PROJECTS.CONTEXT.PLATFORM_ITEM;
    this.iconPath = isActive ? new ThemeIcon('check') : new ThemeIcon('circle-outline');
    this.command = {
      command: PROJECTS.COMMAND.SET_PROJECT_PLATFORM,
      title: 'Set Platform',
      arguments: [this]
    };
  }
}
