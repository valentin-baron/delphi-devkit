import { Uri } from "vscode";
import { dirname } from "path";
import { basenameNoExt, fileExists, findIniFromExecutable } from '../../utils';
import { Entities } from "../../db/entities";
import { DprojParser } from "./dprojParser";
import { GroupProjParser } from "./groupProjParser";

class ProjectFiles {
  public constructor(
    public readonly dproj?: Uri,
    public readonly dpr?: Uri,
    public readonly dpk?: Uri,
    public readonly exe?: Uri,
    public readonly ini?: Uri,
  ) {}

  public get isEmpty(): boolean {
    return !this.dproj && !this.dpr && !this.dpk && !this.exe && !this.ini;
  }
}

export class ProjectFileDiscovery {
  public async findFiles(basePath: Uri, projectName: string): Promise<ProjectFiles> {
    const dprojPath = Uri.joinPath(basePath, `${projectName}.dproj`);
    const dprPath = Uri.joinPath(basePath, `${projectName}.dpr`);
    const dpkPath = Uri.joinPath(basePath, `${projectName}.dpk`);
    const dproj = fileExists(dprojPath) ? dprojPath : undefined;
    const dpr = fileExists(dprPath) ? dprPath : undefined;
    const dpk = fileExists(dpkPath) ? dpkPath : undefined;
    const dprojParser = new DprojParser();
    let exe: Uri | undefined = undefined;
    let ini: Uri | undefined = undefined;
    if (dproj && dpr && !dpk) { // dproj + dpr exist and dpk does not
      exe = await dprojParser.findExecutable(dproj);
    }
    if (exe) {
      ini = await findIniFromExecutable(exe.fsPath);
    }
    return new ProjectFiles(dproj, dpr, dpk, exe, ini);
  }

  public async findFilesFromGroupProj(groupProjPath: Uri): Promise<Entities.Project[]> {
    const dprojs = await new GroupProjParser().getDprojs(groupProjPath);

    return (
      await Promise.all(
        dprojs.map(async (dproj) => {
          const basePath = Uri.file(dirname(dproj.fsPath));
          const projectName = basenameNoExt(dproj.fsPath);
          const files = await this.findFiles(basePath, projectName);
          if (!files.isEmpty) {
            const project = new Entities.Project();
            project.name = projectName;
            project.path = basePath.fsPath;
            project.dproj = files.dproj?.fsPath || null;
            project.dpr = files.dpr?.fsPath || null;
            project.dpk = files.dpk?.fsPath || null;
            project.exe = files.exe?.fsPath || null;
            project.ini = files.ini?.fsPath || null;
            return project;
          }
          return undefined;
        })
      )
    ).filter((project) => project !== undefined);

  }
}
