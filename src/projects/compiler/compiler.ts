import { Uri, workspace, window, languages, OutputChannel } from 'vscode';
import { basename, dirname, join } from 'path';
import { spawn } from 'child_process';
import { Runtime } from '../../runtime';
import { assertError, fileExists } from '../../utils';
import { ProjectLinkType } from '../../types';
import { CompilerOutputDefinitionProvider } from './language';
import { PROJECTS } from '../../constants';
import { Entities } from '../../db/entities';

export interface CompilerConfiguration {
  name: string;
  rsVarsPath: string;
  msBuildPath: string;
  buildArguments: string[];
}

export class Compiler {
  private outputChannel: OutputChannel = window.createOutputChannel('Delphi Compiler', PROJECTS.LANGUAGES.COMPILER);
  private linkProvider: CompilerOutputDefinitionProvider = new CompilerOutputDefinitionProvider();

  constructor() {
    Runtime.extension.subscriptions.push(
      ...[
        this.outputChannel,
        languages.registerDocumentLinkProvider({ language: PROJECTS.LANGUAGES.COMPILER }, this.linkProvider),
      ]
    );
  }

  public async compileWorkspaceItem(link: Entities.ProjectLink, recreate: boolean = false): Promise<void> {
    const path = link.project.dproj || link.project.dpr || link.project.dpk;
    if (!assertError(path, 'No suitable project file (DPROJ, DPR, DPK) found to compile.')) return;

    const fileUri = Uri.file(path!);
    if (!assertError(link.linkType === ProjectLinkType.Workspace, 'Project does not belong to a workspace.')) return;

    const ws = link.workspaceSafe;
    if (!assertError(ws, 'Cannot determine workspace for project.')) return;

    await this.compile(fileUri, ws!.compiler, recreate);
  }

  public async compileGroupProjectItem(link: Entities.ProjectLink, recreate: boolean = false): Promise<void> {
    const path = link.project.dproj || link.project.dpr || link.project.dpk;
    if (!path) {
      window.showErrorMessage('No suitable project file (DPROJ, DPR, DPK) found to compile.');
      return;
    }
    if (!assertError(link.linkType === ProjectLinkType.GroupProject, 'Project does not belong to a group project.')) return;

    const fileUri = Uri.file(path);
    const config = Runtime.configEntity;
    if (!assertError(config.groupProjectsCompiler, 'No compiler configuration set for group projects. Please select one.')) return;

    await this.compile(fileUri, config.groupProjectsCompiler!, recreate);
  }

  private async compile(file: Uri, configName: string, recreate: boolean = false): Promise<void> {
    // Use OutputChannel and diagnostics
    try {
      if (!fileExists(file)) {
        window.showErrorMessage(`Project file not found: ${file.fsPath}`);
        return;
      }
      const cfg = Runtime.compilerConfigurations.find((cfg) => cfg.name === configName);
      if (cfg === undefined) {
        window.showErrorMessage(`Compiler configuration not found: ${configName}`);
        return;
      }
      const config: CompilerConfiguration = cfg!;
      const fileName = basename(file.fsPath);
      const projectDir = dirname(file.fsPath);
      const relativePath = workspace.asRelativePath(projectDir);
      const pathDescription = relativePath === projectDir ? projectDir : relativePath;
      const actionDescription = recreate ? 'recreate (clean + build)' : 'compile (clean + make)';
      const buildTarget = recreate ? 'Build' : 'Make';
      const buildArguments = [`/t:Clean,${buildTarget}`, ...config.buildArguments];
      // Use extension path to find the script
      const scriptPath = Runtime.extension.asAbsolutePath(join('dist', 'compile.ps1'));
      const buildArgumentsString = buildArguments.join(' ');
      let psArgs = [
        '-ExecutionPolicy',
        'Bypass',
        '-File',
        scriptPath,
        '-ProjectPath',
        file.fsPath,
        '-RSVarsPath',
        config.rsVarsPath,
        '-MSBuildPath',
        config.msBuildPath,
        '-FileName',
        fileName,
        '-ActionDescription',
        actionDescription,
        '-PathDescription',
        pathDescription,
        '-BuildArguments',
        buildArgumentsString,
        '-CompilerName',
        config.name
      ];

      const resetSmartScroll = await Runtime.overrideConfiguration('output.smartScroll', 'enabled', false);
      await workspace.getConfiguration('output.smartScroll').update('enabled', false);
      this.linkProvider.compilerIsActive = true;
      window.showInformationMessage(`Starting ${actionDescription} for ${fileName} using ${config.name}...`);
      this.outputChannel.clear();
      this.outputChannel.show(true);
      // Run PowerShell script and capture output
      const proc = spawn('powershell.exe', psArgs, {
        stdio: ['pipe', 'pipe', 'pipe'], // Explicit stdio configuration
        windowsHide: true // Hide PowerShell window
      });
      let output = '';
      const handleIO = (data: Buffer) => {
        const text = data.toString('utf8');
        this.outputChannel.append(text);
        output += text;
      };
      proc.stdout.on('data', handleIO);
      proc.stderr.on('data', handleIO);
      proc.on('close', async (code: number) => {
        this.linkProvider.compilerIsActive = false;
        this.outputChannel.show(true);
        await resetSmartScroll();
        if (code === 0) window.showInformationMessage('Build succeeded');
        else window.showErrorMessage('Build failed');
      });
    } catch (error) {
      window.showErrorMessage(`Failed to ${recreate ? 'recreate' : 'compile'} project: ${error}`);
    }
  }
}
