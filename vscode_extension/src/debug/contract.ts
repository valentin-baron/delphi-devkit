/**
 * What the extension exchanges with `ddk-server` and with debugger extensions,
 * and the decisions depending only on those values. Nothing here touches the
 * `vscode` API, so all of it is testable as plain functions.
 */

/** The reply to `projects/compile`: the build's outcome, once it has run. */
export interface CompileOutcome {
  /** Every project of the build compiled. */
  success: boolean;
  /** The user cancelled the build; `success` is then false. */
  cancelled: boolean;
}

/**
 * Mirrors `ddk_core::debug_target::DebugTarget`, the reply to `debug/target`;
 * both sides are checked against `core/tests/fixtures/debug_target.sample.json`.
 * Every path but `executable` is a file that exists. `warnings` are problems
 * (empty means ready to debug), `notes` are information.
 */
export interface DebugTarget {
  project_id: number | null;
  project: string;
  project_file: string;
  main_source: string | null;
  kind: 'program' | 'package' | 'library';
  executable: string;
  host_application: string | null;
  compiler: string;
  config: string;
  platform: string;
  bitness: number | null;
  symbols: { map: string | null; rsm: string | null };
  source_root: string;
  source_search_paths: string[];
  modules: { name: string; binary: string | null; map: string | null; rsm: string | null; dcp: string | null }[];
  args: string[];
  warnings: string[];
  notes: string[];
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isStringOrNull(value: unknown): value is string | null {
  return value === null || typeof value === 'string';
}

function isStringArray(value: unknown): value is string[] {
  return Array.isArray(value) && value.every((entry) => typeof entry === 'string');
}

/**
 * `undefined` when the reply carries no outcome — a `ddk-server` older than
 * this extension answers `null`. The caller decides what that means.
 */
export function compileOutcomeOf(reply: unknown): CompileOutcome | undefined {
  if (!isRecord(reply) || typeof reply.success !== 'boolean') return undefined;
  return { success: reply.success, cancelled: reply.cancelled === true };
}

export function isDebugTarget(value: unknown): value is DebugTarget {
  if (!isRecord(value)) return false;
  const kinds = ['program', 'package', 'library'];
  const symbols = value.symbols;
  const modules = value.modules;
  return (
    (value.project_id === null || typeof value.project_id === 'number') &&
    typeof value.project === 'string' &&
    typeof value.project_file === 'string' &&
    isStringOrNull(value.main_source) &&
    typeof value.kind === 'string' &&
    kinds.includes(value.kind) &&
    typeof value.executable === 'string' &&
    isStringOrNull(value.host_application) &&
    typeof value.compiler === 'string' &&
    typeof value.config === 'string' &&
    typeof value.platform === 'string' &&
    (value.bitness === null || typeof value.bitness === 'number') &&
    isRecord(symbols) &&
    isStringOrNull(symbols.map) &&
    isStringOrNull(symbols.rsm) &&
    typeof value.source_root === 'string' &&
    isStringArray(value.source_search_paths) &&
    Array.isArray(modules) &&
    modules.every(
      (module) =>
        isRecord(module) &&
        typeof module.name === 'string' &&
        [module.binary, module.map, module.rsm, module.dcp].every(isStringOrNull)
    ) &&
    isStringArray(value.args) &&
    isStringArray(value.warnings) &&
    isStringArray(value.notes)
  );
}

/**
 * The manifest belongs to another extension: any shape other than the expected
 * one means "no", never an exception.
 */
export function contributesDebugger(packageJson: unknown, type: string): boolean {
  if (!isRecord(packageJson) || !isRecord(packageJson.contributes)) return false;
  const debuggers = packageJson.contributes.debuggers;
  if (!Array.isArray(debuggers)) return false;
  return debuggers.some((contribution) => isRecord(contribution) && contribution.type === type);
}

/** What the functions below need to know of a project. */
export interface NamedProject {
  id: number;
  name: string;
}

function hasNamesake(project: NamedProject, all: readonly NamedProject[]): boolean {
  return all.some((other) => other.id !== project.id && other.name.toLowerCase() === project.name.toLowerCase());
}

/**
 * By name when that is unique among `all`, else by id — both resolve through
 * `ddk.debug.getDebugTarget`. A name survives a reset of DDK's project list.
 */
export function projectReference(project: NamedProject, all: readonly NamedProject[]): string {
  return hasNamesake(project, all) ? String(project.id) : project.name;
}

/**
 * The label a project's debug configuration is listed under. Projects sharing
 * a name are told apart by their id, so no label appears twice.
 */
export function configurationName(request: 'launch' | 'attach', project: NamedProject, all: readonly NamedProject[]): string {
  const verb = request === 'launch' ? 'Debug' : 'Attach to';
  const label = hasNamesake(project, all) ? `${project.name} #${project.id}` : project.name;
  return `${verb} ${label} (DDK)`;
}

/**
 * By id when the reference is a number that is one, else by name when exactly
 * one project bears it. `undefined` when none or several do — which DDK's own
 * configurations never produce, but a hand-written one can.
 */
export function projectReferredTo<P extends NamedProject>(reference: unknown, all: readonly P[]): P | undefined {
  if (typeof reference !== 'string' && typeof reference !== 'number') return undefined;
  const text = String(reference).trim();
  if (text === '') return undefined;
  const byId = /^\d+$/.test(text) ? all.find((project) => project.id === Number(text)) : undefined;
  if (byId) return byId;
  const byName = all.filter((project) => project.name.toLowerCase() === text.toLowerCase());
  return byName.length === 1 ? byName[0] : undefined;
}

/** What the function below needs to know of a project link. */
export interface IdentifiedLink {
  id: number;
}

/**
 * The link a project is compiled through: the one the user acted on when it is
 * among the project's links, so a project linked in two workspaces builds with
 * the compiler of the workspace it was picked in; else the first link, as the
 * *Selected Project* commands do. `undefined` means it cannot be compiled.
 */
export function linkToCompile<L extends IdentifiedLink>(links: readonly L[], pickedLinkId?: number): L | undefined {
  return links.find((link) => link.id === pickedLinkId) ?? links[0];
}
