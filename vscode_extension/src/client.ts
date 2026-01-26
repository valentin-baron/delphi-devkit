import {
    LanguageClient, LanguageClientOptions, ServerOptions, TransportKind
} from 'vscode-languageclient/node';
import { window, workspace } from 'vscode';
import * as path from 'path';
import { Runtime } from './runtime';
import { Entities } from './projects/entities';
import { UUID } from 'crypto';

export type Change =
    | { type: 'NewProject', file_path: string, workspace_id: number }
    | { type: 'AddProject', project_id: number, workspace_id: number }
    | { type: 'RemoveProject', project_link_id: number }
    | { type: 'MoveProject', project_link_id: number, drop_target: number }
    | { type: 'RefreshProject', project_id: number }
    | { type: 'UpdateProject', project_id: number, data: Partial<Entities.Project> }
    | { type: 'SelectProject', project_id: number }
    | { type: 'AddWorkspace', name: string, compiler: string }
    | { type: 'RemoveWorkspace', workspace_id: number }
    | { type: 'MoveWorkspace', workspace_id: number, drop_target: number }
    | { type: 'UpdateWorkspace', workspace_id: number, data: { name?: string; compiler?: string; } }
    | { type: 'AddCompiler', key: string, config: Entities.CompilerConfiguration }
    | { type: 'RemoveCompiler', compiler: string }
    | { type: 'UpdateCompiler', key: string, data: Partial<Entities.CompilerConfiguration> }
    | { type: 'SetGroupProject', groupproj_path: string }
    | { type: 'RemoveGroupProject' }
    | { type: 'SetGroupProjectCompiler', compiler: string };


export interface Changes {
    changes: Change[];
    event_id: UUID;
}

export function newChanges(changes: Change[], timeout: number = 5000): Changes {
    const id = Runtime.addEvent(timeout);
    return { changes: changes, event_id: id };
}

export type CompilerProgressParams = {
    type: 'Start',
    lines: string[],
} | {
    type: 'Stdout' | 'Stderr',
    line: string,
} | {
    type: 'Completed',
    success: boolean,
    code: number,
    lines: string[],
} | {
    type: 'SingleProjectCompleted',
    project_id: number,
    success: boolean,
    code: number,
    lines: string[],
} | never;

interface ConfigurationData {
    projects: Entities.ProjectsData;
    compilers: Entities.CompilerConfigurations;
}

export class DDK_Client {
    private client: LanguageClient;

    public async initialize(): Promise<void> {
        const serverPath = 'C:/workspaces/vscode/delphi-devkit/server/target/debug/deps/ddk_server.exe';
        const serverOptions: ServerOptions = {
            run: { command: serverPath, transport: TransportKind.stdio },
            debug: { command: serverPath, transport: TransportKind.stdio }
        };
        const clientOptions: LanguageClientOptions = {};
        clientOptions.outputChannelName = 'DDK Server';
        this.client = new LanguageClient(
            'ddk_server',
            'DDK Server',
            serverOptions,
            clientOptions
        );
        this.client.onNotification(
            'notifications/projects/update',
            async (it: { projects: Entities.ProjectsData }) => {
                Runtime.projectsData = it.projects;
                await Runtime.projects.workspacesTreeView.refresh();
                await Runtime.projects.groupProjectTreeView.refresh();
            }
        );
        this.client.onNotification(
            'notifications/compilers/update',
            async (it: { compilers: Entities.CompilerConfigurations }) => {
                Runtime.compilerConfigurations = it.compilers;
                await Runtime.projects.compilerStatusBarItem.updateDisplay();
            }
        );
        this.client.onNotification(
            'notifications/error',
            async (it: { message: string, event_id?: string }) => {
                if (it.event_id) Runtime.finishEvent(it.event_id);
                window.showErrorMessage(`DDK Server Error: ${it.message}`);
            }
        );
        this.client.onNotification(
            'notifications/event/done',
            async (it: { event_id: string }) => {
                Runtime.finishEvent(it.event_id);
            }
        );
        this.client.onNotification(
            'notifications/compiler/progress',
            this.onCompilerProgress.bind(this)
        );
        await this.client.start();
        try {
            const data: ConfigurationData = await this.client.sendRequest('configuration/fetch', {});
            Runtime.projectsData = data.projects;
            Runtime.compilerConfigurations = data.compilers;
        } catch (e) {
            window.showErrorMessage(`Failed to fetch initial configuration from DDK Server: ${e}`);
        }
    }

    public async projectsDataOverride(data: Entities.ProjectsData): Promise<void> {
        const id = Runtime.addEvent();
        Runtime.projectsData = data;
        await this.client.sendNotification('workspace/didChangeConfiguration', {
            settings: {
                projectsData: data,
                event_id: id
            }
        });
        await Runtime.waitForEvent(id);
    }

    public async compilersOverride(data: Entities.CompilerConfigurations): Promise<void> {
        const id = Runtime.addEvent();
        Runtime.compilerConfigurations = data;
        await this.client.sendNotification('workspace/didChangeConfiguration', {
            settings: {
                compilerConfigurations: data,
                event_id: id
            }
        });
        await Runtime.waitForEvent(id);
    }

    public async applyChanges(changesArray: Change[]): Promise<void> {
        const changes = newChanges(changesArray);
        await this.client.sendNotification('workspace/didChangeConfiguration', {
            settings: {
                changeSet: changes
            }
        });
        await Runtime.waitForEvent(changes.event_id);
    }

    public async compileProject(rebuild: boolean, projectId: number, projectLinkId?: number): Promise<void> {
        await this.client.sendNotification('projects/compile', {
            type: 'Project',
            project_id: projectId,
            project_link_id: projectLinkId,
            rebuild: rebuild,
        });
    }

    public async compileAllInWorkspace(rebuild: boolean, workspaceId: number): Promise<void> {
        await this.client.sendNotification('projects/compile', {
            type: 'AllInWorkspace',
            workspace_id: workspaceId,
            rebuild: rebuild,
        });
    }

    public async compileAllInGroupProject(rebuild: boolean): Promise<void> {
        await this.client.sendNotification('projects/compile', {
            type: 'AllInGroupProject',
            rebuild: rebuild,
        });
    }

    public async compileFromLink(rebuild: boolean, linkId: number): Promise<void> {
        await this.client.sendNotification('projects/compile', {
            type: 'FromLink',
            link_id: linkId,
            rebuild: rebuild,
        });
    }

    public async onCompilerProgress(params: CompilerProgressParams): Promise<void> {
        switch (params.type) {
            case 'Start':
                await workspace.getConfiguration('output.smartScroll').update('enabled', false);
                Runtime.compilerOutputChannel.clear();
                Runtime.compilerOutputChannel.show(true);
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                break;
            case 'Stdout':
            case 'Stderr':
                Runtime.compilerOutputChannel.appendLine(params.line);
                break;
            case 'Completed':
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                if (params.success)
                    window.showInformationMessage('Compilation completed successfully.');
                else
                    window.showErrorMessage(`Compilation failed with exit code ${params.code}.`);
                break;
            case 'SingleProjectCompleted':
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                const project = (await Runtime.getProjectsData())?.projects.find((p) => p.id === params.project_id);
                if (params.success && project)
                    window.showInformationMessage(`Compilation of project ${project.name} completed successfully.`);
                else if (project)
                    window.showErrorMessage(`Compilation of project ${project.name} failed with exit code ${params.code}.`);
                break;
        }
    }
}