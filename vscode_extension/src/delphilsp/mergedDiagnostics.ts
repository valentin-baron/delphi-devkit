import { Diagnostic, languages, Uri, workspace } from 'vscode';
import { DELPHILSP } from '../constants';
import { Runtime } from '../runtime';

/**
 * Merges DDK's compile diagnostics with DelphiLSP's live Error Insight, which
 * both publish as standard diagnostics, so after a failed compile the same
 * error shows up twice — and DDK's copy, refreshed only on the next compile,
 * goes stale as soon as the user fixes the code.
 *
 * DDK's go into a collection it owns, and a DDK entry matching a live one by
 * (line, code) is deleted rather than hidden: when DelphiLSP later clears the
 * live entry, no stale copy resurfaces. Entries DelphiLSP knows nothing about
 * (link errors, unvalidated files) stay until the next compile. Without
 * DelphiLSP installed nothing is ever deleted.
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

  /** Wired as the language client's `handleDiagnostics` middleware. */
  export function publish(uri: Uri, diagnostics: Diagnostic[]): void {
    for (const diagnostic of diagnostics)
      if (diagnostic.source)
        ownSources.add(diagnostic.source);
    collection.set(uri, diagnostics);
  }

  function isMergeEnabled(): boolean {
    if (!Runtime.delphilsp?.isDelphiLspExtensionAvailable) return false;
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

  /** Same line and same compiler error code (`E2003`, …). Without a structured
   *  code on both sides, fall back to DDK's code inside the other message and
   *  then to severity: a live entry of equal severity on the same line
   *  supersedes the stale compile entry anyway. */
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
