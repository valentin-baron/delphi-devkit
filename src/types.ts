import { Entities } from "./db/entities";
import { CompilerConfiguration } from "./projects/compiler/compiler";

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

export enum ProjectFileStatus {
  Normal,
  Selected,
  Missing
}

export enum ProjectType {
  Application,
  Package
}

export interface Feature {
  initialize(): Promise<void>;
}

export namespace ExtensionDataExport {
  export enum Version {
    V1_0 = 1.0
  };
  export const CURRENT_VERSION = Math.max(...Object.values(Version).filter(v => typeof v === "number")) as Version;

  export class FileContent {
    constructor(
      public readonly configuration: Entities.Configuration,
      public readonly compilers: CompilerConfiguration[],
      public readonly version: number = CURRENT_VERSION,
    ) {}
  }
}