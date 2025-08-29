import {
  Uri,
  workspace,
  window,
  Diagnostic,
  DiagnosticSeverity,
  Range,
  languages,
  OutputChannel,
  DiagnosticCollection,
} from "vscode";
import { basename, dirname, join } from "path";
import { spawn } from "child_process";
import { Runtime } from "../../runtime";
import { Entities } from "../../db/entities";
import { PROBLEMMATCHER_REGEX } from ".";
import { PROJECTS } from "../../constants";
import { DelphiProject } from "../treeItems/delphiProject";
import { fileExists } from "../../utils";

export interface CompilerConfiguration {
  name: string;
  rsVarsPath: string;
  msBuildPath: string;
  buildArguments: string[];
  usePrettyFormat?: boolean;
}

const DIAGNOSTIC_SEVERITY = {
  hint: DiagnosticSeverity.Hint,
  warn: DiagnosticSeverity.Warning,
  error: DiagnosticSeverity.Error,
  h: DiagnosticSeverity.Hint,
  w: DiagnosticSeverity.Warning,
  e: DiagnosticSeverity.Error,
  f: DiagnosticSeverity.Error,
};

export class Compiler {
  private outputChannel: OutputChannel = window.createOutputChannel("Delphi Compiler", "ddk.compilerOutput");
  private diagnosticCollection: DiagnosticCollection = languages.createDiagnosticCollection("ddk.compiler");

  constructor() {
    Runtime.extension.subscriptions.push(...[
      this.outputChannel, this.diagnosticCollection
    ]);
  }

  public get availableConfigurations(): CompilerConfiguration[] {
    const config = workspace.getConfiguration(PROJECTS.CONFIG.KEY);
    return config.get<CompilerConfiguration[]>(PROJECTS.CONFIG.COMPILER.CONFIGURATIONS, []);
  }

  public async compileWorkspaceItem(project: DelphiProject, recreate: boolean = false): Promise<void> {
    const path = project.entity.dproj || project.entity.dpr || project.entity.dpk;
    if (!path) {
      window.showErrorMessage("No suitable project file (DPROJ, DPR, DPK) found to compile.");
      return;
    }
    const fileUri = Uri.file(path);
    if (!(project.link.owner instanceof Entities.Workspace)) {
      window.showErrorMessage("Project does not belong to a workspace.");
      return;
    }
    await this.compile(fileUri, project.link.owner.compiler, recreate);
  }

  public async compileGroupProjectItem(project: DelphiProject, recreate: boolean = false): Promise<void> {
    const path = project.entity.dproj || project.entity.dpr || project.entity.dpk;
    if (!path) {
      window.showErrorMessage("No suitable project file (DPROJ, DPR, DPK) found to compile.");
      return;
    }
    if (!(project.link.owner instanceof Entities.GroupProject)) {
      window.showErrorMessage("Project does not belong to a group project.");
      return;
    }
    const fileUri = Uri.file(path);
    const config = await Runtime.db.getConfiguration();
    if (!config.groupProjectsCompiler) {
      window.showErrorMessage("No compiler configuration set for group projects. Please select one.");
      return;
    }
    await this.compile(fileUri, config.groupProjectsCompiler, recreate);
  }

  private async compile(file: Uri, configName: string, recreate: boolean = false): Promise<void> {
    // Use OutputChannel and diagnostics
    try {
      if (!fileExists(file)) {
        window.showErrorMessage(`Project file not found: ${file.fsPath}`);
        return;
      }
      const cfg = this.availableConfigurations.find((cfg) => cfg.name === configName);
      if (cfg === undefined) {
        window.showErrorMessage(`Compiler configuration not found: ${configName}`);
        return;
      }
      const config: CompilerConfiguration = cfg!;
      const fileName = basename(file.fsPath);
      const projectDir = dirname(file.fsPath);
      const relativePath = workspace.asRelativePath(projectDir);
      const pathDescription =
        relativePath === projectDir ? projectDir : relativePath;
      const actionDescription = recreate
        ? "recreate (clean + build)"
        : "compile (clean + make)";
      const buildTarget = recreate ? "Build" : "Make";
      const buildArguments = [
        `/t:Clean,${buildTarget}`,
        ...config.buildArguments,
      ];
      // Use extension path to find the script
      const scriptPath = Runtime.extension.asAbsolutePath(join("dist", "compile.ps1"));
      const buildArgumentsString = buildArguments.join(" ");
      let psArgs = [
        "-ExecutionPolicy", "Bypass",
        "-File", scriptPath,
        "-ProjectPath", file.fsPath,
        "-RSVarsPath", config.rsVarsPath,
        "-MSBuildPath", config.msBuildPath,
        "-FileName", fileName,
        "-ActionDescription", actionDescription,
        "-PathDescription", pathDescription,
        "-BuildArguments", buildArgumentsString,
        "-CompilerName", config.name,
      ];
      if (config.usePrettyFormat) {
        psArgs.push("-UsePrettyFormat");
      }
      window.showInformationMessage(
        `Starting ${actionDescription} for ${fileName} using ${config.name}...`
      );
      this.outputChannel.clear();
      this.outputChannel.show(true);
      // Run PowerShell script and capture output
      const proc = spawn("powershell.exe", psArgs, {
        stdio: ['pipe', 'pipe', 'pipe'],  // Explicit stdio configuration
        windowsHide: true  // Hide PowerShell window
      });
      let output = "";
      proc.stdout.on("data", (data: Buffer) => {
        const text = data.toString('utf8');
        this.outputChannel.append(text);
        output += text;
      });

      proc.stderr.on("data", (data: Buffer) => {
        const text = data.toString('utf8');
        this.outputChannel.append(text);
        output += text;
      });
      proc.on("close", async (code: number) => {
        // Parse and publish diagnostics
        const problemRegex = PROBLEMMATCHER_REGEX[+!!config.usePrettyFormat];
        const lines = output.split(/\r?\n/);
        const batch = await Promise.all(
          lines.map(async (line) => {
            const match = problemRegex.exec(line);
            if (match) {
              let filePath: string;
              let diagnostic: Diagnostic;
              if (config.usePrettyFormat) {
                filePath = match[3];
                const lineNum = parseInt(match[4], 10) - 1;
                const message = match[5];
                const severity =
                  DIAGNOSTIC_SEVERITY[
                    match[1].toLowerCase() as keyof typeof DIAGNOSTIC_SEVERITY
                  ] || DiagnosticSeverity.Information;
                diagnostic = new Diagnostic(
                  new Range(lineNum, 0, lineNum, 1000),
                  message,
                  severity
                );
              } else {
                filePath = match[1];
                const lineNum = parseInt(match[2], 10) - 1;
                const message = match[6];
                const severity =
                  DIAGNOSTIC_SEVERITY[
                    match[4].toLowerCase()[0] as keyof typeof DIAGNOSTIC_SEVERITY
                  ] || DiagnosticSeverity.Information;
                diagnostic = new Diagnostic(
                  new Range(lineNum, 0, lineNum, 1000),
                  message,
                  severity
                );
              }
              return [filePath, diagnostic] as [string, Diagnostic];
            }
          })
        );
        this.diagnosticCollection.clear();
        const diagnosticsArray: [string, Diagnostic[]][] = batch
          .filter((item): item is [string, Diagnostic] => item !== undefined)
          .reduce((acc, [filePath, diagnostic]) => {
            const existing = acc.find(([path]) => path === filePath);
            if (existing) {
              existing[1].push(diagnostic);
            } else {
              acc.push([filePath, [diagnostic]]);
            }
            return acc;
          }, [] as [string, Diagnostic[]][]);
        await Promise.all(
          diagnosticsArray.map(async ([filePath, diagnostics]) => {
            this.diagnosticCollection.set(Uri.file(filePath), diagnostics);
          })
        );
        if (code === 0) {
          window.showInformationMessage("Build succeeded");
        } else {
          window.showErrorMessage("Build failed");
        }
      });
    } catch (error) {
      window.showErrorMessage(
        `Failed to ${recreate ? "recreate" : "compile"} project: ${error}`
      );
    }
  }
}
