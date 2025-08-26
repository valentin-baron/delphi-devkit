export enum DelphiProjectTreeItemType {
  Project,
  DprojFile,
  DpkFile,
  DprFile,
  ExecutableFile,
  IniFile
}

export enum WorkspaceViewMode {
  GroupProject,
  Discovery,
  Empty
}

export interface Feature {
  initialize(): Promise<void>;
}