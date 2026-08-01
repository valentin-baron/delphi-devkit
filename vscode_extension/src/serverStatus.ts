import { StatusBarAlignment, StatusBarItem, window } from 'vscode';
import { PROJECTS } from './constants';

/**
 * The wire shape of a `ddk/serverStatus` notification — the mirror of the
 * server's `ServerStatusParams` (`server/src/status.rs`), field-for-field.
 *
 * `state` is one of the five variant names the server serializes; `detail` /
 * `current` / `total` are populated per state (a file for `Analyzing`, the unit
 * and N/M counters for `Indexing`/`Bootstrapping`, absent for a bare `Ready` /
 * `Initializing`).
 */
export interface ServerStatusParams {
  state: 'Initializing' | 'Ready' | 'Analyzing' | 'Indexing' | 'Bootstrapping';
  detail?: string | null;
  current?: number | null;
  total?: number | null;
}

/**
 * The PERSISTENT language-server status item (Task 24).
 *
 * Shown for the whole lifetime the language client runs, reflecting ddk-server's
 * overall state driven by the best-effort `ddk/serverStatus` notification:
 *
 * - Initializing → `$(loading~spin) DDK: starting…`
 * - Ready        → `$(check) DDK`
 * - Analyzing    → `$(sync~spin) DDK: <file>`
 * - Indexing     → `$(sync~spin) DDK: Indexing N/M`   (tooltip = the unit)
 * - Bootstrapping→ `$(sync~spin) DDK: Bootstrapping N/M` (tooltip = the unit)
 *
 * This is COMPLEMENTARY to task-17's transient `workDoneProgress` spinner and to
 * the compiler status items — a separate item (its own id, see
 * `PROJECTS.STATUS_BAR.SERVER_STATUS`) with a clear "DDK" label, so it never
 * collides with them.
 *
 * The item starts in `Initializing` at construction (before the server's first
 * `Ready`) and is disposed on client stop / extension deactivate.
 */
export class ServerStatusBar {
  private readonly item: StatusBarItem;

  /**
   * @param revealCommand a command id to invoke on click (e.g. reveal the "DDK
   *   Server" output channel), or `undefined` for a non-clickable item.
   */
  constructor(revealCommand?: string) {
    // Right-aligned, low priority so it sits unobtrusively at the far end and
    // does not push the compiler/compilation items (Left-aligned) around.
    this.item = window.createStatusBarItem(
      PROJECTS.STATUS_BAR.SERVER_STATUS,
      StatusBarAlignment.Right,
      -100
    );
    this.item.name = 'DDK Server';
    if (revealCommand) this.item.command = revealCommand;
    // Show an honest "starting…" until the server pushes its first Ready. The
    // item is visible for the whole time the client runs.
    this.render({ state: 'Initializing' });
    this.item.show();
  }

  /**
   * Apply a `ddk/serverStatus` payload to the item. Best-effort telemetry on the
   * server side means updates may be sparse; each update fully re-renders the
   * item so there is never a stale label. Only `Indexing`/`Bootstrapping` show
   * N/M and a unit tooltip; everything else clears the tooltip so a prior unit
   * name never lingers.
   */
  public render(status: ServerStatusParams): void {
    switch (status.state) {
      case 'Initializing':
        this.item.text = '$(loading~spin) DDK: starting…';
        this.item.tooltip = 'DDK language server is starting';
        break;

      case 'Ready':
        this.item.text = '$(check) DDK';
        this.item.tooltip = 'DDK language server: ready';
        break;

      case 'Analyzing': {
        const file = status.detail ?? '';
        this.item.text = file
          ? `$(sync~spin) DDK: ${file}`
          : '$(sync~spin) DDK: analyzing';
        this.item.tooltip = file
          ? `DDK language server: analyzing ${file}`
          : 'DDK language server: analyzing';
        break;
      }

      case 'Indexing':
        this.item.text = `$(sync~spin) DDK: Indexing ${this.counter(status)}`;
        this.item.tooltip = this.unitTooltip('Indexing', status);
        break;

      case 'Bootstrapping':
        this.item.text = `$(sync~spin) DDK: Bootstrapping ${this.counter(status)}`;
        this.item.tooltip = this.unitTooltip('Bootstrapping', status);
        break;
    }
  }

  public dispose(): void {
    this.item.dispose();
  }

  // -------------------------------------------------------------------------

  /** `N/M` from the payload counters, or an empty string when either is absent
   *  (a counted state should always carry both, but never render `undefined`). */
  private counter(status: ServerStatusParams): string {
    const current = status.current;
    const total = status.total;
    if (current === null || current === undefined) return '';
    if (total === null || total === undefined) return '';
    return `${current}/${total}`;
  }

  /** Tooltip for a counted state: the operation, N/M, and the current unit. */
  private unitTooltip(operation: string, status: ServerStatusParams): string {
    const counter = this.counter(status);
    const unit = status.detail ? ` — ${status.detail}` : '';
    return `DDK language server: ${operation} ${counter}${unit}`.trimEnd();
  }
}
