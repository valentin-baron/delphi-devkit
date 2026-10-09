import { StatusBarAlignment, StatusBarItem, ThemeColor, window, workspace } from 'vscode';
import { CompilerProgressParams } from '../../client';
import { PROJECTS } from '../../constants';
import { Runtime } from '../../runtime';

/**
 * Status bar item for the current compilation state, hidden while idle.
 * `ddk.compiler.resultTimeout` (ms) is how long a finished build's result stays
 * visible; `0` never shows it.
 */
export class CompilationStatusBar {
  private readonly item: StatusBarItem;
  private hideTimer: ReturnType<typeof setTimeout> | undefined;
  private readonly listener: (p: CompilerProgressParams) => void;

  private static readonly DEFAULT_RESULT_VISIBLE_MS = 5_000;

  private get resultTimeoutMs(): number {
    return workspace
      .getConfiguration(PROJECTS.SETTINGS.SECTION)
      .get<number>(PROJECTS.SETTINGS.COMPILER_RESULT_TIMEOUT, CompilationStatusBar.DEFAULT_RESULT_VISIBLE_MS);
  }

  constructor() {
    // Priority just below the compiler-picker item so it sits to its right.
    this.item = window.createStatusBarItem(
      PROJECTS.STATUS_BAR.COMPILATION,
      StatusBarAlignment.Left,
      -1
    );
    this.item.command = PROJECTS.COMMAND.CANCEL_COMPILATION;

    this.listener = this.onProgress.bind(this);
    Runtime.client.addCompilerProgressListener(this.listener);
    Runtime.extension.subscriptions.push(this.item);
  }

  public dispose(): void {
    Runtime.client.removeCompilerProgressListener(this.listener);
    this.item.dispose();
  }

  private onProgress(params: CompilerProgressParams): void {
    switch (params.kind) {
      case 'Start':
        this.showSpinning('Compiling…');
        break;

      case 'SingleProjectStarted': {
        const project = Runtime.projectsData?.projects.find(
          (p) => p.id === params.project_id
        );
        const label = project ? project.name : `Project ${params.project_id}`;
        this.showSpinning(`Compiling: ${label}`);
        break;
      }

      case 'SingleProjectCompleted':
        // Keep spinning until the overall Completed event clears things up.
        break;

      case 'Completed': {
        const timeout = this.resultTimeoutMs;
        if (timeout === 0) {
          this.item.hide();
          break;
        }
        if (params.cancelled)
          this.showResult('$(circle-slash) Compilation cancelled', 'warning', timeout);
        else if (params.success)
          this.showResult('$(check) Build succeeded', 'success', timeout);
        else
          this.showResult(`$(error) Build failed (exit ${params.code})`, 'error', timeout);
        break;
      }
    }
  }

  private showSpinning(label: string): void {
    this.clearHideTimer();
    this.item.text = `$(sync~spin) ${label}`;
    this.item.tooltip = 'Click to cancel compilation';
    this.item.backgroundColor = undefined;
    this.item.show();
  }

  private showResult(text: string, kind: 'success' | 'error' | 'warning', timeoutMs: number): void {
    this.clearHideTimer();
    this.item.text = text;
    this.item.tooltip = undefined;
    this.item.command = undefined;
    this.item.backgroundColor =
      kind === 'error'
        ? new ThemeColor('statusBarItem.errorBackground')
        : undefined;
    this.item.show();
    this.hideTimer = setTimeout(() => {
      this.item.hide();
      this.item.command = PROJECTS.COMMAND.CANCEL_COMPILATION;
      this.item.backgroundColor = undefined;
    }, timeoutMs);
  }

  private clearHideTimer(): void {
    if (this.hideTimer !== undefined) {
      clearTimeout(this.hideTimer);
      this.hideTimer = undefined;
    }
  }
}
