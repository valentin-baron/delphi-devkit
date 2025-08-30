export namespace PROJECTS {
  export namespace CONFIG {
    export const KEY = 'ddk.projects';
    export function full(element: string): string {
      return `${KEY}.${element}`;
    }
    export namespace DISCOVERY {
      const NS = 'discovery';
      export const ENABLE = `${NS}.enable`;
      export const PROJECT_PATHS = `${NS}.projectPaths`;
      export const EXCLUDE_PATTERNS = `${NS}.excludePatterns`;
    }
    export const SortProjects = 'sortProjects';
    export namespace COMPILER {
      export const NS = 'compiler';
      export const CONFIGURATIONS = `${NS}.configurations`;
    }
  }
  export namespace COMMAND {
    export const REFRESH = `${PROJECTS.CONFIG.KEY}.refresh`;
    export const ADD_WORKSPACE = `${PROJECTS.CONFIG.KEY}.addWorkspace`;
    export const RENAME_WORKSPACE = `${PROJECTS.CONFIG.KEY}.renameWorkspace`;
    export const REMOVE_WORKSPACE = `${PROJECTS.CONFIG.KEY}.removeWorkspace`;
    export const ADD_PROJECT = `${PROJECTS.CONFIG.KEY}.addProject`;
    export const REMOVE_PROJECT = `${PROJECTS.CONFIG.KEY}.removeProject`;
    export const COMPILE = `${PROJECTS.CONFIG.KEY}.compile`;
    export const RECREATE = `${PROJECTS.CONFIG.KEY}.recreate`;
    export const SHOW_IN_EXPLORER = `${PROJECTS.CONFIG.KEY}.showInExplorer`;
    export const OPEN_IN_FILE_EXPLORER = `${PROJECTS.CONFIG.KEY}.openInFileExplorer`;
    export const RUN_EXECUTABLE = `${PROJECTS.CONFIG.KEY}.runExecutable`;
    export const CONFIGURE_OR_CREATE_INI = `${PROJECTS.CONFIG.KEY}.configureOrCreateIni`;
    export const SELECT_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.pickGroupProject`;
    export const UNLOAD_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.unloadGroupProject`;
    export const SELECT_COMPILER = `${PROJECTS.CONFIG.KEY}.selectCompilerConfiguration`;
    export const SELECT_PROJECT = `${PROJECTS.CONFIG.KEY}.selectProject`;
    export const COMPILE_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.compileSelectedProject`;
    export const RECREATE_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.recreateSelectedProject`;
    export const RUN_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.runSelectedProject`;
    export const EDIT_DEFAULT_INI = `${PROJECTS.CONFIG.KEY}.editDefaultIni`;
  }

  export namespace CONTEXT {
    export const IS_GROUP_PROJECT_OPENED = 'ddk:isGroupProjectOpened';
    export const IS_PROJECT_SELECTED = 'ddk:isProjectSelected';
    export const DOES_SELECTED_PROJECT_HAVE_EXE = 'ddk:doesSelectedProjectHaveExe';

    export const WORKSPACE = 'ddk.context.projects.workspace';
    export const PROJECT = 'ddk.context.projects.project';
    export const PROJECT_FILE = 'ddk.context.projects.projectFile';
  }

  export namespace VIEW {
    export const WORKSPACES = 'ddk.view.projects.workspaces';
    export const GROUP_PROJECT = 'ddk.view.projects.groupProject';
  }

  export namespace STATUS_BAR {
    export const COMPILER = 'ddk.statusBar.projects.compiler';
  }

  export namespace SCHEME {
    export const DEFAULT = 'ddk';
    export const SELECTED = `${DEFAULT}.selected`;
    export const MISSING = `${DEFAULT}.missing`;
  }

  export namespace MIME_TYPES {
    export const FS_FILES = 'text/uri-list';
    /**
     * **Multi**: forbidden
     *
     * **Data**: `Entities.Workspace.id` (number)
     */
    export const WORKSPACE = 'application/vnd.code.tree.ddk.workspace';
    /**
     * **Multi**: forbidden
     *
     * **Data**: `Entities.WorkspaceProjectLink.id` (number)
     */
    export const WORKSPACE_PROJECT = 'application/vnd.code.tree.ddk.workspaceproject';
    /**
     * **Multi**: forbidden
     *
     * **Data**: `Entities.GroupProjectLink.id` (number)
     */
    export const GROUP_PROJECT_CHILD = 'application/vnd.code.tree.ddk.groupprojchild';
  }
}

export namespace DFM {
  export enum Commands {
    SWAP_DFM_PAS = 'ddk.dfm.swapToDfmPas'
  }
}

export namespace COMMANDS {
  export const IMPORT_CONFIGURATION = 'ddk.importConfiguration';
  export const EXPORT_CONFIGURATION = 'ddk.exportConfiguration';
}
