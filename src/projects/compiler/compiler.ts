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
import { WorkspaceEntity } from "../../db/entities";
import { Restorable } from "../../db/restorable";
import { FindOneOptions } from "typeorm";
import { Coroutine } from "../../typings";
import { PROBLEMMATCHER_REGEX } from ".";
import { Projects } from "../../constants";

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

export class Compiler extends Restorable<WorkspaceEntity> {
  private outputChannel: OutputChannel = window.createOutputChannel("Delphi Compiler", "delphi-devkit.compilerOutput");
  private diagnosticCollection: DiagnosticCollection = languages.createDiagnosticCollection("delphi-devkit.compiler");

  constructor() {
    super(WorkspaceEntity);
    Runtime.extension.subscriptions.push(...[
      this.outputChannel, this.diagnosticCollection
    ]);
  }

  public loadOptions(): FindOneOptions<WorkspaceEntity> {
    return {
      where: { hash: Runtime.workspaceHash}
    };
  };

  public createCallback(): Coroutine<WorkspaceEntity> | undefined {
    return async () => Runtime.db.initializeWorkspace();
  }

  public async restore(entity: WorkspaceEntity): Promise<void> {
    this.configuration = entity.compiler;
  }

  public get availableConfigurations(): CompilerConfiguration[] {
    const config = workspace.getConfiguration(Projects.Config.Key);
    return config.get<CompilerConfiguration[]>(Projects.Config.Compiler.Configurations, []);
  }

  public set configuration(configurationName: string | undefined) {
    if (!configurationName) { return; }
    const config = workspace.getConfiguration(Projects.Config.Key);
    config.update(Projects.Config.Compiler.CurrentConfiguration, configurationName, false);
    Runtime.db.modify(async (ws) => ws.compiler = configurationName).then(() => {
      Runtime.projects.compilerStatusBarItem.updateDisplay();
    });
    window.showInformationMessage(
      `Compiler configuration set to: ${configurationName}`
    );
  }

  public async getConfiguration(canUseCache: boolean = true): Promise<CompilerConfiguration> {
    let currentConfigName: string | undefined = undefined;
    if (canUseCache) {
      const ws = await Runtime.db.getWorkspace();
      currentConfigName = ws?.compiler;
    }
    if (!currentConfigName) {
      const config = workspace.getConfiguration(Projects.Config.Key);
      currentConfigName = config.get(
        Projects.Config.Compiler.CurrentConfiguration,
        Projects.Config.Compiler.Value_DefaultConfiguration
      );
    }
    const currentConfig = this.availableConfigurations.find(
      (cfg) => cfg.name === currentConfigName
    );
    if (!currentConfig) {
      throw new Error(
        `Compiler configuration '${currentConfigName}' not found.`
      );
    }
    currentConfig.usePrettyFormat = currentConfig.usePrettyFormat ?? true;
    return currentConfig;
  }

  public async compile(file: Uri, recreate: boolean = false): Promise<void> {
    // Use OutputChannel and diagnostics
    try {
      const fileName = basename(file.fsPath);
      const projectDir = dirname(file.fsPath);
      const relativePath = workspace.asRelativePath(projectDir);
      const pathDescription =
        relativePath === projectDir ? projectDir : relativePath;
      const actionDescription = recreate
        ? "recreate (clean + build)"
        : "compile (clean + make)";
      const buildTarget = recreate ? "Build" : "Make";
      const config = await this.getConfiguration();
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
