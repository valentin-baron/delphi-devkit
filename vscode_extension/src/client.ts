import {
    LanguageClient, LanguageClientOptions, ServerOptions, State, TransportKind
} from 'vscode-languageclient/node';
import { commands, Disposable, DocumentFormattingEditProvider, DocumentRangeFormattingEditProvider, ExtensionMode, languages, Range, TextDocument, TextEdit, window, workspace } from 'vscode';
import { Runtime } from './runtime';
import { Entities } from './projects/entities';
import { UUID } from 'crypto';
import { join } from 'path';
import { existsSync } from 'fs';
import { CompilerOutputDefinitionProvider } from './projects/compiler/language';
import { PROJECTS } from './constants';
import { ServerStatusBar, ServerStatusParams } from './serverStatus';

/** Command id that reveals the "DDK Server" output channel — the click action of
 *  the persistent server-status item. */
const REVEAL_SERVER_OUTPUT = 'ddk.serverStatus.revealOutput';

/**
 * The fields `UpdateProject` accepts server-side — the mirror of Rust's
 * `ProjectUpdateData` in `core/src/projects/changes.rs`, field-for-field.
 * Deliberately narrower than `Entities.Project`: read-only/derived fields
 * (`id`, `dproj_run_params`, `dproj_host_application`, ...) are not updatable
 * and would be silently dropped by the server.
 */
export interface ProjectUpdateData {
    name?: string;
    directory?: string;
    dproj?: string;
    dpr?: string;
    dpk?: string;
    exe?: string;
    ini?: string;
    start_parameters?: string;
    host_application?: string;
}

export type Change =
    | { type: 'NewProject', file_path: string, workspace_id: number }
    | { type: 'AddProject', project_id: number, workspace_id: number }
    | { type: 'RemoveProject', project_link_id: number }
    | { type: 'MoveProject', project_link_id: number, drop_target: number }
    | { type: 'RefreshProject', project_id: number }
    | { type: 'UpdateProject', project_id: number, data: ProjectUpdateData }
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
    | { type: 'SetGroupProjectCompiler', compiler: string }
    | { type: 'SetProjectConfiguration', project_id: number, config: string | null }
    | { type: 'SetProjectPlatform', project_id: number, platform: string | null }
    | { type: 'SetWorkspaceConfiguration', workspace_id: number, config: string | null }
    | { type: 'SetWorkspacePlatform', workspace_id: number, platform: string | null }
    | { type: 'SetGroupProjectConfiguration', config: string | null }
    | { type: 'SetGroupProjectPlatform', platform: string | null }
    | { type: 'TransferGroupProject', name: string, compiler: string };


export interface Changes {
    changes: Change[];
}

export interface ChangeSet {
    changeSet: Changes;
    event_id: UUID;
}

export function newChanges(changes: Change[], timeout: number = 5000): ChangeSet {
    const id = Runtime.addEvent(timeout);
    return { changeSet: { changes: changes }, event_id: id };
}

export type CompilerProgressParams = {
    kind: 'Start',
    lines: string[],
} | {
    kind: 'Stdout' | 'Stderr',
    line: string,
} | {
    kind: 'Completed',
    success: boolean,
    cancelled: boolean,
    code: number,
    lines: string[],
} | {
    kind: 'SingleProjectStarted',
    project_id: number,
    lines: string[],
} | {
    kind: 'SingleProjectCompleted',
    project_id: number,
    success: boolean,
    cancelled: boolean,
    code: number,
    lines: string[],
} | never;

interface ConfigurationData {
    projects: Entities.ProjectsData;
    compilers: Entities.CompilerConfigurations;
}

export interface DprojMetadata {
    configurations: string[];
    platforms: string[];
    active_configuration: string;
    active_platform: string;
}

export class DDK_Client {
    private client: LanguageClient;
    private compilerLinkProvider = new CompilerOutputDefinitionProvider();
    private compilerProgressListeners = new Set<(progressParams: CompilerProgressParams) => void>();
    /** The persistent language-server status item (Task 24), created in
     *  `initialize` once the client exists and disposed on client stop /
     *  deactivate. */
    private serverStatusBar?: ServerStatusBar;

    public addCompilerProgressListener(callback: (progressParams: CompilerProgressParams) => void): void {
        this.compilerProgressListeners.add(callback);
    }

    public removeCompilerProgressListener(callback: (progressParams: CompilerProgressParams) => void): void {
        this.compilerProgressListeners.delete(callback);
    }

    public async initialize(): Promise<void> {
        const serverPath = this.resolveServerPath();
        const serverOptions: ServerOptions = {
            run: { command: serverPath, transport: TransportKind.stdio },
            debug: { command: serverPath, transport: TransportKind.stdio }
        };
        const clientOptions: LanguageClientOptions = {
            // Route Object Pascal source documents to ddk-server for the LSP
            // language features (diagnostics, definition, hover, references,
            // completion, signature help, semantic tokens). The `.dfm`/compiler
            // languages are handled through their own providers, not the LSP.
            documentSelector: [
                { scheme: 'file', language: 'objectpascal' }
            ],
            initializationOptions: {
                encoding: workspace.getConfiguration(PROJECTS.SETTINGS.SECTION).get<string>(PROJECTS.SETTINGS.COMPILER_ENCODING, 'oem')
            }
        };
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
                Runtime.updateProjectContexts();
                await Runtime.projects.workspacesTreeView.refresh();
                await Runtime.projects.groupProjectTreeView.refresh();
                await Runtime.projects.compilerStatusBarItem.updateDisplay();
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
        // Persistent language-server status view (Task 24). A dedicated status bar
        // item, separate from the compiler items, driven by the best-effort
        // `ddk/serverStatus` notification the server pushes. Complementary to the
        // task-17 transient progress spinner — this shows the standing state
        // (Ready / Analyzing <file> / Indexing N/M).
        Runtime.extension.subscriptions.push(
            commands.registerCommand(REVEAL_SERVER_OUTPUT, () => {
                // Reveal the "DDK Server" output channel on click.
                this.client.outputChannel.show(true);
            })
        );
        this.serverStatusBar = new ServerStatusBar(REVEAL_SERVER_OUTPUT);
        Runtime.extension.subscriptions.push(this.serverStatusBar);
        this.client.onNotification(
            'ddk/serverStatus',
            (status: ServerStatusParams) => {
                this.serverStatusBar?.render(status);
            }
        );
        // Dispose the status item when the client stops (server exited/crashed) so
        // it never lingers showing a stale "Ready" after the server is gone. The
        // subscription above is the deactivate-time safety net; this handles a
        // mid-session stop. Registered before `start()` so no state change is
        // missed.
        this.client.onDidChangeState((event) => {
            if (event.newState === State.Stopped) {
                this.serverStatusBar?.dispose();
                this.serverStatusBar = undefined;
            }
        });
        await this.client.start();
        await this.refresh();
        Runtime.extension.subscriptions.push(
            ...this.createFormattingProvider(),
            languages.registerDocumentLinkProvider(
                { language: PROJECTS.LANGUAGES.COMPILER },
                this.compilerLinkProvider
            ),
            workspace.onDidChangeConfiguration(e => {
                if (e.affectsConfiguration(`${PROJECTS.SETTINGS.SECTION}.${PROJECTS.SETTINGS.COMPILER_ENCODING}`)) {
                    const encoding = workspace.getConfiguration(PROJECTS.SETTINGS.SECTION)
                        .get<string>(PROJECTS.SETTINGS.COMPILER_ENCODING, 'oem');
                    this.client.sendNotification('notifications/settings/encoding', { encoding });
                }
            })
        );
    }

    public async refresh(): Promise<void> {
        try {
            const data: ConfigurationData = await this.client.sendRequest('configuration/fetch', {});
            Runtime.projectsData = data.projects;
            Runtime.compilerConfigurations = data.compilers;
            Runtime.updateProjectContexts();
        } catch (e) {
            window.showErrorMessage(`Failed to fetch configuration from DDK Server: ${e}`);
        }
    }

    private createFormattingProvider(): Disposable[] {
        return [
            languages.registerDocumentFormattingEditProvider(
                {
                    scheme: 'file',
                    pattern: '**/*.{dpr,dpk,pas,inc}',
                },
                new DelphiFormattingProvider(this.client)
            ),
            languages.registerDocumentRangeFormattingEditProvider(
                {
                    scheme: 'file',
                    pattern: '**/*.{dpr,dpk,pas,inc}',
                },
                new DelphiFormattingProvider(this.client)
            )
        ];
    }

    public async applyChanges(changesArray: Change[]): Promise<boolean> {
        const changes = newChanges(changesArray);
        await this.client.sendNotification('workspace/didChangeConfiguration', {
            settings: changes
        });
        return await Runtime.waitForEvent(changes.event_id);
    }

    public async compileProject(rebuild: boolean, projectId: number, projectLinkId?: number): Promise<boolean> {
        const event = Runtime.addEvent(0);
        await this.client.sendRequest('projects/compile', {
            type: 'Project',
            project_id: projectId,
            project_link_id: projectLinkId,
            rebuild: rebuild,
            event_id: event,
        });
        return await Runtime.waitForEvent(event);
    }

    public async compileAllInWorkspace(rebuild: boolean, workspaceId: number): Promise<boolean> {
        const event = Runtime.addEvent(0);
        await this.client.sendRequest('projects/compile', {
            type: 'AllInWorkspace',
            workspace_id: workspaceId,
            rebuild: rebuild,
            event_id: event,
        });
        return await Runtime.waitForEvent(event);
    }

    public async compileAllInGroupProject(rebuild: boolean): Promise<boolean> {
        const event = Runtime.addEvent(0);
        await this.client.sendRequest('projects/compile', {
            type: 'AllInGroupProject',
            rebuild: rebuild,
            event_id: event,
        });
        return await Runtime.waitForEvent(event);
    }

    public async compileFromLink(rebuild: boolean, linkId: number): Promise<boolean> {
        const event = Runtime.addEvent(0);
        await this.client.sendRequest('projects/compile', {
            type: 'FromLink',
            project_link_id: linkId,
            rebuild: rebuild,
            event_id: event
        });
        return await Runtime.waitForEvent(event);
    }

    public async cancelCompilation(): Promise<void> {
        await this.client.sendRequest('projects/compile-cancel', {});
    }

    public async dprojMetadata(projectId: number): Promise<DprojMetadata> {
        return await this.client.sendRequest('dproj/metadata', { project_id: projectId });
    }

    public onCompilerProgress(params: CompilerProgressParams) {
        for (const listener of this.compilerProgressListeners) listener(params);
        switch (params.kind) {
            case 'Start':
                this.compilerLinkProvider.compilerIsActive = true;
                Runtime.setContext(PROJECTS.CONTEXT.IS_COMPILING, true);
                // generally, we need smart scroll to be enabled so that the output channel
                // scrolls to the end when new lines are added. We do not re-enable it because
                // we are likely the only extension that actually really cares about the setting.
                workspace.getConfiguration('output.smartScroll').update('enabled', false);
                Runtime.compilerOutputChannel.clear();
                Runtime.compilerOutputChannel.show(true);
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                break;
            case 'Stdout':
            case 'Stderr':
                Runtime.compilerOutputChannel.appendLine(params.line);
                break;
            case 'SingleProjectStarted':
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                break;
            case 'Completed':
                this.compilerLinkProvider.compilerIsActive = false;
                Runtime.setContext(PROJECTS.CONTEXT.IS_COMPILING, false);
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                if (params.cancelled)
                    window.showWarningMessage('Compilation was cancelled.');
                else if (params.success)
                    window.showInformationMessage('Compilation completed successfully.');
                else
                    window.showErrorMessage(`Compilation failed with exit code ${params.code}.`);
                break;
            case 'SingleProjectCompleted':
                for (const line of params.lines)
                    Runtime.compilerOutputChannel.appendLine(line);
                const project = Runtime.projectsData?.projects.find((p) => p.id === params.project_id);
                if (params.cancelled && project)
                    window.showWarningMessage(`Compilation of project ${project.name} was cancelled.`);
                else if (params.success && project)
                    window.showInformationMessage(`Compilation of project ${project.name} completed successfully.`);
                else if (project)
                    window.showErrorMessage(`Compilation of project ${project.name} failed with exit code ${params.code}.`);
                break;
        }
    }

    private resolveServerPath(): string {
        const extensionDir = Runtime.extension.extensionUri.fsPath;
        const isDev = Runtime.extension.extensionMode !== ExtensionMode.Production;
        const serverPath = isDev
            ? join(extensionDir, '..', 'target', 'debug', 'ddk-server.exe')
            : join(extensionDir, 'server', 'ddk-server.exe');

        if (!existsSync(serverPath)) {
            const mode = isDev ? 'Development' : 'Production';
            throw new Error(
                `DDK server binary not found at: ${serverPath} (${mode} mode). ` +
                (isDev
                    ? 'Run `cargo build` in the repository root.'
                    : 'The extension package may be incomplete.')
            );
        }
        return serverPath;
    }
}

/** Reply from `custom/document/format`: replace `[start, end)` (UTF-16 offsets
 *  into the document) with `newText`. */
interface DocumentFormatEdit {
    start: number;
    end: number;
    newText: string;
}

class DelphiFormattingProvider implements DocumentFormattingEditProvider, DocumentRangeFormattingEditProvider {
    constructor(private readonly client: LanguageClient) { }

    async provideDocumentRangeFormattingEdits(
        document: TextDocument,
        range: Range,
    ): Promise<TextEdit[]> {
        return this.format(document, range);
    }

    async provideDocumentFormattingEdits(
        document: TextDocument,
    ): Promise<TextEdit[]> {
        return this.format(document, undefined);
    }

    // Always send the whole document, even for a range request: the formatter
    // needs full context. The server maps the selection back onto the result.
    private async format(document: TextDocument, range: Range | undefined): Promise<TextEdit[]> {
        const edit: DocumentFormatEdit = await this.client.sendRequest('custom/document/format', {
            content: document.getText(),
            range: range
                ? { start: document.offsetAt(range.start), end: document.offsetAt(range.end) }
                : null,
        });
        return [
            new TextEdit(
                new Range(document.positionAt(edit.start), document.positionAt(edit.end)),
                edit.newText,
            ),
        ];
    }
}