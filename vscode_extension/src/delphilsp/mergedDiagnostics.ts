import { Diagnostic, languages, Uri, workspace } from 'vscode';
import { DELPHILSP } from '../constants';
import { Runtime } from '../runtime';

/**
 * Merges DDK's compile diagnostics with DelphiLSP's live Error Insight.
 *
 * Both publish standard diagnostics, so after a failed compile the same error
 * shows up twice: once from DDK (compiler output, 1-character range) and once
 * from DelphiLSP (live validation, precise token range) — and DDK's copy goes
 * stale the moment the user fixes the code, since it only refreshes on the
 * next compile.
 *
 * The server's diagnostics are therefore routed into a collection DDK owns
 * (via the language client's `handleDiagnostics` middleware), and whenever
 * DelphiLSP publishes for a file, every DDK entry matching one of its live
 * entries by (line, code) is **deleted** — not hidden — so the more precise
 * live entry is the only one shown, and when DelphiLSP later clears it
 * because the user fixed the error (without recompiling), no stale DDK copy
 * resurfaces. DDK entries DelphiLSP knows nothing about (link errors, files
 * it has not validated) stay until the next compile.
 *
 * Without DelphiLSP installed nothing is ever deleted: compile diagnostics
 * keep their current lifetime, and this module is only the rendering route.
 */
export namespace MergedDiagnostics {
  const collection = languages.createDiagnosticCollection('DDK Compiler');
  /** `source` labels DDK itself has published (compiler display names) —
   *  anything else in a file's diagnostics belongs to another extension. */
  const ownSources = new Set<string>();

  export function initialize(): void {
    Runtime.extension.subscriptions.push(
      collection,
      languages.onDidChangeDiagnostics((event) => {
        for (const uri of event.uris)
          dedupeAgainstLiveDiagnostics(uri);
      })
    );
  }

  /** Rendering route for the server's `publishDiagnostics` (wired as the
   *  language client's `handleDiagnostics` middleware), kept in an own
   *  collection so single entries can be deleted later. */
  export function publish(uri: Uri, diagnostics: Diagnostic[]): void {
    for (const diagnostic of diagnostics)
      if (diagnostic.source)
        ownSources.add(diagnostic.source);
    collection.set(uri, diagnostics);
  }

  function isMergeEnabled(): boolean {
    if (!Runtime.delphilsp?.isAvailable) return false;
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.MERGE_DIAGNOSTICS, true);
  }

  function dedupeAgainstLiveDiagnostics(uri: Uri): void {
    if (!isMergeEnabled()) return;
    const ours = collection.get(uri);
    if (!ours || ours.length === 0) return;

    const foreign = languages.getDiagnostics(uri).filter((entry) => !entry.source || !ownSources.has(entry.source));
    if (foreign.length === 0) return;

    const remaining = ours.filter((mine) => !foreign.some((theirs) => reportSameError(mine, theirs)));
    if (remaining.length !== ours.length)
      collection.set(uri, remaining);
  }

  /** Same line + same compiler error code (`E2003`, …). When one side lacks
   *  a structured code, fall back to finding DDK's code inside the other
   *  message, then to severity — a live entry of equal severity on the same
   *  line supersedes the stale compile entry anyway. */
  function reportSameError(mine: Diagnostic, theirs: Diagnostic): boolean {
    if (mine.range.start.line !== theirs.range.start.line) return false;
    const mineCode = codeText(mine);
    const theirsCode = codeText(theirs);
    if (mineCode && theirsCode) return mineCode === theirsCode;
    if (mineCode) return theirs.message.includes(mineCode) || mine.severity === theirs.severity;
    return mine.severity === theirs.severity;
  }

  function codeText(diagnostic: Diagnostic): string | undefined {
    const code = diagnostic.code;
    if (code === undefined || code === null) return undefined;
    if (typeof code === 'object') return String(code.value);
    return String(code);
  }
}
