import { Uri, workspace, RelativePattern, WorkspaceFolder } from "vscode";
import { basename, dirname } from "path";
import { findIniFromExecutable } from '../../utils';
import { GroupProjectEntity, ProjectEntity } from "../../db/entities";
import { ProjectType } from "../treeItems/delphiProject";
import { DprojParser } from "./dprojParser";
import { GroupProjParser } from "./groupProjParser";
import { Runtime } from "../../runtime";
import { LexoSorter } from "../../utils/lexoSorter";
import { Projects } from "../../constants";

interface FileGroup {
  baseName: string;
  dproj?: Uri;
  dpr?: Uri;
  dpk?: Uri;
}
type FolderName = string;
type FolderLookupMap = Map<FolderName, FileGroup[]>;

export class ProjectDiscovery {
  public async findAllProjects(): Promise<ProjectEntity[]> {
    if (!workspace.workspaceFolders?.length) {
      return [];
    }

    let projects: Array<ProjectEntity> = [];
    const config = workspace.getConfiguration(Projects.Config.Key);
    const projectPaths: string[] = config.get(Projects.Config.Discovery.ProjectPaths, ["**"]);
    const excludePatterns: string[] = config.get(Projects.Config.Discovery.ExcludePatterns, []);

    await Promise.allSettled(
      workspace.workspaceFolders.map(async (folder) => {
        // Create exclude pattern for workspace.findFiles
        const excludeGlob =
          !!excludePatterns.length
            ? `{${excludePatterns.join(",")}}`
            : undefined;

        // Use optimized batch processing approach
        await this.findProjectsInMainWorkspaceFolder(
          folder,
          projectPaths,
          excludeGlob,
          projects
        );
      })
    );
    
    projects = projects.sort((a, b) => a.name.localeCompare(b.name));
    projects = new LexoSorter<ProjectEntity>(projects).items;

    return projects;
  }

  public async findFilesFromGroupProj(uri: Uri): Promise<ProjectEntity[]> {
    if (!workspace.workspaceFolders?.length) {
      return [];
    }

    let projects: Array<ProjectEntity> = [];
    const dprojs = await new GroupProjParser().getDprojs(uri);
    const groupProjEntity = new GroupProjectEntity();
    groupProjEntity.path = uri.fsPath;
    groupProjEntity.name = basename(uri.fsPath);
    
    let filesByDir = await this.createFilesByDirectoryMap(
      dprojs, [], []
    );

    const dprojParser = new DprojParser();
    await Promise.all(
      Array.from(filesByDir.values()).flat().map(async (group) => {
        group.dpk = await dprojParser.findDpk(group.dproj!);
        group.dpr = await dprojParser.findDpr(group.dproj!);
      })
    );
    await this.assemble(filesByDir, projects);
    projects = projects.sort((a, b) => a.name.localeCompare(b.name));
    projects = new LexoSorter<ProjectEntity>(projects).items;
    return projects;
  }

  private async findProjectsInMainWorkspaceFolder(
    mainWorkspaceFolder: WorkspaceFolder,
    projectPaths: string[],
    excludeGlob: string | undefined,
    projects: Array<ProjectEntity>
  ): Promise<void> {
    const dprojPatterns: string[] = [];
    const dprPatterns: string[] = [];
    const dpkPatterns: string[] = [];

    for (const projectPath of projectPaths) {
      dprojPatterns.push(`${projectPath}/**/*.[Dd][Pp][Rr][Oo][Jj]`);
      dprPatterns.push(`${projectPath}/**/*.[Dd][Pp][Rr]`);
      dpkPatterns.push(`${projectPath}/**/*.[Dd][Pp][Kk]`);
    }

    const [dprojResult, dprResult, dpkResult] = await Promise.allSettled([
      this.findAllFilesByPattern(
        mainWorkspaceFolder,
        dprojPatterns,
        excludeGlob
      ),
      this.findAllFilesByPattern(mainWorkspaceFolder, dprPatterns, excludeGlob),
      this.findAllFilesByPattern(mainWorkspaceFolder, dpkPatterns, excludeGlob),
    ]);

    const dprojFiles = dprojResult.status === 'fulfilled' ? dprojResult.value : [];
    const dprFiles = dprResult.status === 'fulfilled' ? dprResult.value : [];
    const dpkFiles = dpkResult.status === 'fulfilled' ? dpkResult.value : [];

    // Create lookup maps for faster file association
    const filesByDir = await this.createFilesByDirectoryMap(
      dprojFiles,
      dprFiles,
      dpkFiles
    );
    await this.assemble(filesByDir, projects);
  }

  private async findAllFilesByPattern(
    mainWorkspaceFolder: WorkspaceFolder,
    patterns: string[],
    excludeGlob: string | undefined
  ): Promise<Uri[]> {
    if (!patterns.length) {
      return [];
    }
    const combinedPattern =
      patterns.length === 1 ? patterns[0] : `{${patterns.join(",")}}`;
    const relativePattern = new RelativePattern(
      mainWorkspaceFolder,
      combinedPattern
    );
    return await workspace.findFiles(relativePattern, excludeGlob);
  }

  private async createFilesByDirectoryMap(
    dprojFiles: Uri[],
    dprFiles: Uri[],
    dpkFiles: Uri[]
  ): Promise<FolderLookupMap> {
    const filesByDir = new Map<FolderName, FileGroup[]>();

    let processFiles = (
      files: Uri[],
      setPropertyCallback: (group: FileGroup, value: Uri) => void
    ) => {
      files.forEach((file) => {
        const dirPath = dirname(file.fsPath);
        const baseName = basename(file.fsPath).replace(/\.[^/.]+$/, "");

        if (!filesByDir.has(dirPath)) {
          filesByDir.set(dirPath, []);
        }

        const dirFiles = filesByDir.get(dirPath)!;
        let group = dirFiles.find((f) => f.baseName === baseName);
        if (!group) {
          group = { baseName };
          dirFiles.push(group);
        }
        setPropertyCallback(group, file);
      });
    };

    processFiles(dprojFiles, (group: FileGroup, value: Uri) => {
      group.dproj = value;
    });

    processFiles(dprFiles, (group: FileGroup, value: Uri) => {
      group.dpr = value;
    });

    processFiles(dpkFiles, (group: FileGroup, value: Uri) => {
      group.dpk = value;
    });

    return filesByDir;
  }

  private async assemble(
    filesByDir: FolderLookupMap,
    projects: Array<ProjectEntity>
  ): Promise<void> {
    const dprojParser = new DprojParser();
    const ws = await Runtime.db.getWorkspace();
    if (!ws) { throw new Error("[Delphi.Projects][ProjectDiscovery] Workspace not found."); }
    await Promise.allSettled(
      Array.from(filesByDir.entries()).map(async ([dirPath, groups]) => {
        await Promise.allSettled(
          groups.map(async (group) => {
            const project = new ProjectEntity();
            projects.push(project);
            project.name = group.baseName;
            project.path = dirPath;
            project.workspace = ws;
            project.dprojPath = group.dproj?.fsPath;
            project.dprPath = group.dpr?.fsPath;
            project.dpkPath = group.dpk?.fsPath;
            project.type = group.dpk ? ProjectType.Package : ProjectType.Application;
            project.exePath = (await dprojParser.findExecutable(group.dproj))?.fsPath;
            project.iniPath = await findIniFromExecutable(project.exePath);
          })
        );
      })
    );
  }
}
