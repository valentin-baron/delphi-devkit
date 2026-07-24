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
    export const CONFIG_PLATFORM_DISPLAY = 'configPlatformDisplay';
    export const USE_DEBUGGER_RUN_PARAMS = 'useDebuggerRunParams';
    export namespace COMPILER {
      export const NS = 'compiler';
      export const CONFIGURATIONS = `${NS}.configurations`;
    }
  }

  export namespace SETTINGS {
    export const SECTION = 'ddk';
    export const COMPILER_ENCODING = 'compiler.encoding';
    export const COMPILER_RESULT_TIMEOUT = 'compiler.resultTimeout';
  }
  export namespace COMMAND {
    export const ADD_WORKSPACE = `${PROJECTS.CONFIG.KEY}.addWorkspace`;
    export const RENAME_WORKSPACE = `${PROJECTS.CONFIG.KEY}.renameWorkspace`;
    export const REMOVE_WORKSPACE = `${PROJECTS.CONFIG.KEY}.removeWorkspace`;
    export const ADD_PROJECT = `${PROJECTS.CONFIG.KEY}.addProject`;
    export const REMOVE_PROJECT = `${PROJECTS.CONFIG.KEY}.removeProject`;
    export const COMPILE = `${PROJECTS.CONFIG.KEY}.compile`;
    export const RECREATE = `${PROJECTS.CONFIG.KEY}.recreate`;
    export const REFRESH = `${PROJECTS.CONFIG.KEY}.refresh`;
    export const DISCOVER_PROJECT_PATHS = `${PROJECTS.CONFIG.KEY}.discoverProjectPaths`;
    export const SET_MANUAL_PATH = `${PROJECTS.CONFIG.KEY}.setManualPath`;
    export const COMPILE_ALL_IN_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.compileAllInGroupProject`;
    export const RECREATE_ALL_IN_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.recreateAllInGroupProject`;
    export const COMPILE_ALL_IN_WORKSPACE = `${PROJECTS.CONFIG.KEY}.compileAllInWorkspace`;
    export const RECREATE_ALL_IN_WORKSPACE = `${PROJECTS.CONFIG.KEY}.recreateAllInWorkspace`;
    export const COMPILE_ALL_FROM_HERE = `${PROJECTS.CONFIG.KEY}.compileAllFromHere`;
    export const RECREATE_ALL_FROM_HERE = `${PROJECTS.CONFIG.KEY}.recreateAllFromHere`;
    export const SHOW_IN_EXPLORER = `${PROJECTS.CONFIG.KEY}.showInExplorer`;
    export const OPEN_IN_FILE_EXPLORER = `${PROJECTS.CONFIG.KEY}.openInFileExplorer`;
    export const RUN_EXECUTABLE = `${PROJECTS.CONFIG.KEY}.runExecutable`;
    export const CONFIGURE_OR_CREATE_INI = `${PROJECTS.CONFIG.KEY}.configureOrCreateIni`;
    export const SET_START_PARAMETERS = `${PROJECTS.CONFIG.KEY}.setStartParameters`;
    export const SET_HOST_APPLICATION = `${PROJECTS.CONFIG.KEY}.setHostApplication`;
    export const SELECT_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.pickGroupProject`;
    export const UNLOAD_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.unloadGroupProject`;
    export const SELECT_COMPILER = `${PROJECTS.CONFIG.KEY}.selectCompilerConfiguration`;
    export const SELECT_PROJECT = `${PROJECTS.CONFIG.KEY}.selectProject`;
    export const COMPILE_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.compileSelectedProject`;
    export const RECREATE_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.recreateSelectedProject`;
    export const RUN_SELECTED_PROJECT = `${PROJECTS.CONFIG.KEY}.runSelectedProject`;
    export const EDIT_DEFAULT_INI = `${PROJECTS.CONFIG.KEY}.editDefaultIni`;
    export const CANCEL_COMPILATION = `${PROJECTS.CONFIG.KEY}.cancelCompilation`;
    export const SET_PROJECT_CONFIGURATION = `${PROJECTS.CONFIG.KEY}.setProjectConfiguration`;
    export const SET_PROJECT_PLATFORM = `${PROJECTS.CONFIG.KEY}.setProjectPlatform`;
    export const SET_WORKSPACE_CONFIGURATION = `${PROJECTS.CONFIG.KEY}.setWorkspaceConfiguration`;
    export const SET_WORKSPACE_PLATFORM = `${PROJECTS.CONFIG.KEY}.setWorkspacePlatform`;
    export const SET_GROUP_PROJECT_CONFIGURATION = `${PROJECTS.CONFIG.KEY}.setGroupProjectConfiguration`;
    export const SET_GROUP_PROJECT_PLATFORM = `${PROJECTS.CONFIG.KEY}.setGroupProjectPlatform`;
    export const TRANSFER_GROUP_PROJECT = `${PROJECTS.CONFIG.KEY}.transferGroupProject`;
  }

  export namespace CONTEXT {
    export const IS_GROUP_PROJECT_OPENED = 'ddk:isGroupProjectOpened';
    export const IS_PROJECT_SELECTED = 'ddk:isProjectSelected';
    export const DOES_SELECTED_PROJECT_HAVE_EXE = 'ddk:doesSelectedProjectHaveExe';
    export const IS_COMPILING = 'ddk:isCompiling';

    export const WORKSPACE = 'ddk.context.projects.workspace';
    export const PROJECT = 'ddk.context.projects.project';
    export const PROJECT_FILE = 'ddk.context.projects.projectFile';
    export const CONFIGURATION_GROUP = 'ddk.context.projects.configurationGroup';
    export const PLATFORM_GROUP = 'ddk.context.projects.platformGroup';
    export const CONFIGURATION_ITEM = 'ddk.context.projects.configurationItem';
    export const PLATFORM_ITEM = 'ddk.context.projects.platformItem';
  }

  export namespace VIEW {
    export const WORKSPACES = 'ddk.view.projects.workspaces';
    export const GROUP_PROJECT = 'ddk.view.projects.groupProject';
    export const CONFIGURATION = 'ddk.view.configuration';
  }

  export namespace STATUS_BAR {
    export const COMPILER = 'ddk.statusBar.projects.compiler';
    export const COMPILATION = 'ddk.statusBar.projects.compilation';
  }

  export namespace SCHEME {
    export const DEFAULT = 'ddk';
    export const SELECTED = `${DEFAULT}.selected`;
    export const MISSING = `${DEFAULT}.missing`;
    export const COMPILING = `${DEFAULT}.compiling`;
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

  export namespace LANGUAGES {
    export const COMPILER = 'ddk.compiler';
  }
}

export namespace DFM {
  export enum Commands {
    SWAP_DFM_PAS = 'ddk.dfm.swapToDfmPas'
  }
}

export namespace COMMANDS {
  export const EXPORT_PROJECTS = 'ddk.exportProjects';
  export const IMPORT_PROJECTS = 'ddk.importProjects';
  export const EXPORT_COMPILERS = 'ddk.exportCompilers';
  export const IMPORT_COMPILERS = 'ddk.importCompilers';
  export const EDIT_COMPILER_CONFIGURATIONS = 'ddk.editCompilerConfigurations';
  export const RESET_COMPILER_CONFIGURATIONS = 'ddk.resetCompilerConfigurations';
  export const EDIT_PROJECTS_DATA = 'ddk.editProjectsData';
  export const QUICK_PICK_PROJECT = 'ddk.quickPickProject';
}

export namespace FORMAT {
  export const KEY = 'ddk.formatter';
  export namespace CONFIG {
    export const ENABLE   = `enable`;
    export const PATH     = `path`;
    export const ARGS     = `args`;
    export const ON_SAVE  = `formatOnSave`;
  }
  export namespace COMMAND {
    export const EDIT_FORMATTER_CONFIG = `${FORMAT.KEY}.editConfig`;
    export const RESET_FORMATTER_CONFIG = `${FORMAT.KEY}.resetConfig`;
  }
}
