import { promises as fs } from 'fs';
import { dirname, join, resolve } from 'path';
import { workspace } from 'vscode';
import { Runtime } from '../runtime';
import { DELPHILSP } from '../constants';
import { Option } from '../types';

/**
 * Keeps generated `.delphilsp.json` files out of version control through the
 * repository's `.git/info/exclude`, the local counterpart of `.gitignore`.
 * Only the entry DDK wrote (recognised by its marker comment) is ever touched,
 * and nothing is added when the pattern is already excluded.
 */
export namespace DelphiLspGitExclude {
  const MARKER = '# Managed by Delphi DevKit (ddk.delphilsp.autoIgnoreDelphiLspFiles)';
  const PATTERN = '*.delphilsp.json';

  function isAutoIgnoreEnabled(): boolean {
    return workspace.getConfiguration(DELPHILSP.CONFIG.KEY).get<boolean>(DELPHILSP.CONFIG.AUTO_IGNORE, true);
  }

  /** A `.git` *file* (linked worktree or submodule) points at the real git dir,
   *  and a worktree's `commondir` at the shared one, where `info/exclude` lives. */
  async function findGitInfoExcludePath(startDir: string): Promise<Option<string>> {
    let dir = startDir;
    for (;;) {
      const dotGit = join(dir, '.git');
      try {
        const stat = await fs.stat(dotGit);
        if (stat.isDirectory()) return join(dotGit, 'info', 'exclude');
        const gitDirReference = /^gitdir:\s*(.+)$/m.exec(await fs.readFile(dotGit, 'utf8'));
        if (!gitDirReference) return undefined;
        let gitDir = resolve(dir, gitDirReference[1].trim());
        try {
          const commonDir = (await fs.readFile(join(gitDir, 'commondir'), 'utf8')).trim();
          gitDir = resolve(gitDir, commonDir);
        } catch {
          // No `commondir` — a plain git dir already.
        }
        return join(gitDir, 'info', 'exclude');
      } catch {
        // No `.git` at this level — keep walking up.
      }
      const parent = dirname(dir);
      if (parent === dir) return undefined;
      dir = parent;
    }
  }

  function hasPattern(lines: string[]): boolean {
    return lines.some((line) => line.trim() === PATTERN);
  }

  async function appendManagedEntry(excludePath: string): Promise<void> {
    let content = '';
    try {
      content = await fs.readFile(excludePath, 'utf8');
    } catch {
      // Missing `info/exclude` (or even `info/`) — created below.
    }
    if (hasPattern(content.split('\n'))) return;
    await fs.mkdir(dirname(excludePath), { recursive: true });
    const separator = content.length === 0 || content.endsWith('\n') ? '' : '\n';
    await fs.writeFile(excludePath, `${content}${separator}${MARKER}\n${PATTERN}\n`);
  }

  async function removeManagedEntry(excludePath: string): Promise<void> {
    let content: string;
    try {
      content = await fs.readFile(excludePath, 'utf8');
    } catch {
      return;
    }
    const lines = content.split('\n');
    const kept: string[] = [];
    for (let i = 0; i < lines.length; i++)
      if (lines[i].trim() === MARKER && lines[i + 1]?.trim() === PATTERN)
        i++; // Skip the marker and its pattern line — everything else survives.
      else
        kept.push(lines[i]);
    const stripped = kept.join('\n');
    if (stripped !== content) await fs.writeFile(excludePath, stripped);
  }

  /** Called after every settings-file generation or sync. */
  export async function ensureExcludedFor(generatedFilePath: string): Promise<void> {
    if (!isAutoIgnoreEnabled()) return;
    try {
      const excludePath = await findGitInfoExcludePath(dirname(generatedFilePath));
      if (excludePath) await appendManagedEntry(excludePath);
    } catch (error) {
      console.error(`[DDK] Failed to update .git/info/exclude for ${generatedFilePath}: ${error}`);
    }
  }

  /** Applies the setting to the repository of every managed project. */
  export async function onSettingChanged(): Promise<void> {
    const enabled = isAutoIgnoreEnabled();
    const projectDirs = (Runtime.projectsData?.projects ?? [])
      .map((project) => project.dproj || project.dpr || project.dpk)
      .filter((source): source is string => !!source)
      .map((source) => dirname(source));

    const excludePaths = new Set<string>();
    for (const dir of projectDirs)
      try {
        const excludePath = await findGitInfoExcludePath(dir);
        if (excludePath) excludePaths.add(excludePath);
      } catch {
        // Unreachable directory — nothing to manage there.
      }

    for (const excludePath of excludePaths)
      try {
        if (enabled) await appendManagedEntry(excludePath);
        else await removeManagedEntry(excludePath);
      } catch (error) {
        console.error(`[DDK] Failed to update ${excludePath}: ${error}`);
      }
  }
}
