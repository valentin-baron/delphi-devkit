import {
  OutputChannel,
  ProcessExecution,
  Task,
  TaskPanelKind,
  TaskRevealKind,
  TaskScope,
  Uri,
  tasks,
  window,
  workspace
} from 'vscode';
import { ChildProcess, spawn, spawnSync } from 'child_process';
import { dirname } from 'path';
import { PROJECTS } from '../constants';
import { Option } from '../types';
import { launchExecutable, splitCommandLineArgs } from '../utils';
import { Runtime } from '../runtime';

/**
 * Runs a project's run target inside VS Code rather than as a detached process.
 * A terminal by default: a program coloring through the Windows console API
 * emits those colors only with a real console screen buffer, which ConPTY then
 * re-encodes as escape sequences. `runIn: "output"` pipes into an output channel
 * instead — searchable text, but the colors are never written to the stream and
 * no reader can recover them.
 */
export namespace ProjectRunner {
  /** Runs whose process is still alive. Output is only labelled while more than one is running. */
  const active = new Set<ChildProcess>();

  export function run(target: string, parameters: Option<string>, label: string): void {
    const config = workspace.getConfiguration(PROJECTS.CONFIG.KEY);
    const args = parameters ? splitCommandLineArgs(parameters) : [];
    try {
      switch (config.get<string>(PROJECTS.CONFIG.RUN_IN, 'terminal')) {
        case 'output':
          spawnIntoOutputChannel(
            target,
            args,
            label,
            config.get<string>(PROJECTS.CONFIG.RUN_OUTPUT_ENCODING, 'ansi'),
            config.get<string>(PROJECTS.CONFIG.RUN_REVEAL_OUTPUT, 'onOutput')
          );
          window.showInformationMessage(`Running: ${target}`);
          break;
        case 'detached':
          launchExecutable(target, parameters);
          window.showInformationMessage(`Running: ${target}`);
          break;
        default:
          // The terminal itself announces the run, so no notification here.
          runInTerminal(target, args, label);
      }
    } catch (error) {
      window.showErrorMessage(`Failed to launch executable: ${error}`);
    }
  }

  /**
   * A task, not `createTerminal`: a terminal whose own process is the executable
   * is disposed the moment that process ends, taking a short-lived program's
   * output with it, while a task terminal stays open on the finished output.
   * `Dedicated` gives each project its own terminal, cleared on re-run.
   */
  function runInTerminal(target: string, args: string[], label: string): void {
    const task = new Task(
      { type: PROJECTS.TASK.RUN, project: label },
      workspace.getWorkspaceFolder(Uri.file(target)) ?? TaskScope.Workspace,
      label,
      PROJECTS.TASK.RUN_SOURCE,
      // ProcessExecution, not ShellExecution: no shell to quote or expand the arguments.
      new ProcessExecution(target, args, { cwd: dirname(target) })
    );
    task.presentationOptions = {
      reveal: TaskRevealKind.Always,
      panel: TaskPanelKind.Dedicated,
      echo: true,
      clear: true,
      focus: false,
      showReuseMessage: true,
      close: false
    };
    // Nothing here parses the output into diagnostics — compiling does that.
    task.problemMatchers = [];
    tasks.executeTask(task).then(undefined, (error) => window.showErrorMessage(`Failed to launch executable: ${error}`));
  }

  function spawnIntoOutputChannel(target: string, args: string[], label: string, encoding: string, reveal: string): void {
    const channel = Runtime.runOutputChannel;
    const started = Date.now();

    // Never clear while another run is still writing into the channel.
    if (active.size === 0) channel.clear();
    else channel.appendLine('');

    const writer = new RunWriter(channel, decoderFor(encoding), () => (active.size > 1 ? `[${label}] ` : ''), reveal === 'onOutput');
    if (reveal === 'always') channel.show(true);

    channel.appendLine(`▶ ${label} · ${timestamp()}`);
    channel.appendLine(`▷ ${[target, ...args].map(quoted).join(' ')}`);

    // Nothing may come between the spawn and its listeners below: a failing
    // spawn would emit an unhandled `error` event.
    // `windowsHide` suppresses the console window a console application would
    // otherwise pop up; a VCL form stays visible, as the VCL ignores the hidden
    // show-command passed along to it. stdin is unconnected: an output channel
    // cannot forward keystrokes, so a program reading input sees EOF rather
    // than hanging invisibly. Not detached, or the child's stdout and stderr
    // could not be piped here at all.
    const child = spawn(target, args, {
      cwd: dirname(target),
      stdio: ['ignore', 'pipe', 'pipe'],
      windowsHide: true
    });
    active.add(child);

    child.stdout?.on('data', (chunk: Buffer) => writer.write(chunk));
    child.stderr?.on('data', (chunk: Buffer) => writer.write(chunk));

    // A failed spawn emits both `error` and `close`, so the block is closed by
    // whichever comes first.
    let closed = false;
    const close = (footer: string) => {
      active.delete(child);
      if (closed) return;
      closed = true;
      writer.finish(footer);
    };

    child.on('error', (error) => {
      close(`✖ ${label} failed to launch: ${error.message}`);
      channel.show(true);
      window.showErrorMessage(`Failed to launch executable: ${error.message}`);
    });

    // `close` rather than `exit`: it fires once the pipes are drained, so no
    // output can arrive after the footer line.
    child.on('close', (code, signal) => {
      close(`■ ${label} ${signal ? `was terminated (${signal})` : `exited with code ${code}`} · ${elapsed(started)}`);
    });
  }

  function quoted(arg: string): string {
    return /\s/.test(arg) ? `"${arg}"` : arg;
  }

  /** OSC sequence (a window title and friends), terminated by BEL or ST. */
  const ANSI_OSC = /\u001b\][^\u0007\u001b]*(?:\u0007|\u001b\\)?/g;
  /** CSI sequence (colors, cursor movement, erases), a two-character Fe escape, or a charset designation such as ESC ( B. */
  const ANSI_CSI = /\u001b\[[0-?]*[ -\/]*[@-~]|\u001b[@-Z\\-_]|\u001b[ -\/]*[0-~]/g;
  /** Control characters left over afterwards: tab survives, the newlines are handled by the caller. */
  const CONTROL_CHARS = /[\u0000-\u0008\u000b-\u001f\u007f]/g;

  /**
   * Removes what an output channel cannot render but a console would act on: an
   * output channel has no ANSI support, so escape sequences would show up as
   * `←[32m` litter, and a self-rewriting progress line keeps only the state
   * after its last carriage return, the way it would look on screen.
   */
  function sanitize(text: string): string {
    const plain = text.replace(ANSI_OSC, '').replace(ANSI_CSI, '');
    const overwrites = plain.split('\r').filter((segment) => segment.length > 0);
    const visible = overwrites.length > 0 ? overwrites[overwrites.length - 1] : '';
    return visible.replace(CONTROL_CHARS, '');
  }

  function timestamp(): string {
    return new Date().toLocaleTimeString();
  }

  function elapsed(started: number): string {
    const seconds = (Date.now() - started) / 1000;
    return seconds < 60 ? `${seconds.toFixed(1)}s` : `${Math.floor(seconds / 60)}m ${(seconds % 60).toFixed(0)}s`;
  }

  /**
   * Buffers raw process bytes and writes whole lines to the channel. Splitting
   * on newlines *before* decoding keeps a multi-byte character straddling two
   * chunks intact and lets every line be decoded on its own, which is what makes
   * per-line `auto` detection possible. An unterminated line (a prompt, a
   * progress counter) is flushed once the process goes quiet.
   */
  class RunWriter {
    private static readonly IDLE_FLUSH_MS = 400;
    private static readonly LF = 0x0a;
    private static readonly CR = 0x0d;

    private pending: Buffer = Buffer.alloc(0);
    /** A line was written without its newline yet — the next write continues it, and must not repeat the prefix. */
    private partial = false;
    private idle: Option<NodeJS.Timeout>;
    private revealed = false;

    constructor(
      private readonly channel: OutputChannel,
      private readonly decode: Decoder,
      private readonly prefix: () => string,
      private readonly revealOnOutput: boolean
    ) {}

    public write(chunk: Buffer): void {
      if (this.revealOnOutput && !this.revealed) {
        this.revealed = true;
        this.channel.show(true);
      }
      this.pending = this.pending.length ? Buffer.concat([this.pending, chunk]) : chunk;
      let index: number;
      while ((index = this.pending.indexOf(RunWriter.LF)) >= 0) {
        const line = this.pending.subarray(0, index);
        this.pending = this.pending.subarray(index + 1);
        this.emit(line.length > 0 && line[line.length - 1] === RunWriter.CR ? line.subarray(0, line.length - 1) : line, true);
      }
      this.scheduleIdleFlush();
    }

    public finish(footer: string): void {
      this.clearIdleFlush();
      if (this.pending.length > 0) {
        this.emit(this.pending, true);
        this.pending = Buffer.alloc(0);
      } else if (this.partial) {
        this.channel.appendLine('');
        this.partial = false;
      }
      this.channel.appendLine(footer);
    }

    private emit(bytes: Buffer, complete: boolean): void {
      const text = sanitize(this.decode(bytes));
      const prefix = this.partial ? '' : this.prefix();
      if (prefix) this.channel.append(prefix);
      if (complete) {
        this.channel.appendLine(text);
        this.partial = false;
      } else {
        this.channel.append(text);
        this.partial = true;
      }
    }

    private scheduleIdleFlush(): void {
      this.clearIdleFlush();
      if (this.pending.length === 0) return;
      this.idle = setTimeout(() => {
        this.idle = undefined;
        if (this.pending.length === 0) return;
        this.emit(this.pending, false);
        this.pending = Buffer.alloc(0);
      }, RunWriter.IDLE_FLUSH_MS);
    }

    private clearIdleFlush(): void {
      if (!this.idle) return;
      clearTimeout(this.idle);
      this.idle = undefined;
    }
  }

  type Decoder = (bytes: Buffer) => string;

  /**
   * Upper half (0x80-0xFF) of the DOS/OEM codepages a Delphi console application
   * writes on a Western/Central European or Cyrillic Windows. `TextDecoder`
   * decodes none of these, so they come from a table.
   */
  const OEM_UPPER_HALF: Record<number, string> = {
    437: 'ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■ ',
    850: 'ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜø£Ø×ƒáíóúñÑªº¿®¬½¼¡«»░▒▓│┤ÁÂÀ©╣║╗╝¢¥┐└┴┬├─┼ãÃ╚╔╩╦╠═╬¤ðÐÊËÈıÍÎÏ┘┌█▄¦Ì▀ÓßÔÒõÕµþÞÚÛÙýÝ¯´­±‗¾¶§÷¸°¨·¹³²■ ',
    852: 'ÇüéâäůćçłëŐőîŹÄĆÉĹĺôöĽľŚśÖÜŤťŁ×čáíóúĄąŽžĘę¬źČş«»░▒▓│┤ÁÂĚŞ╣║╗╝Żż┐└┴┬├─┼Ăă╚╔╩╦╠═╬¤đĐĎËďŇÍÎě┘┌█▄ŢŮ▀ÓßÔŃńňŠšŔÚŕŰýÝţ´­˝˛ˇ˘§÷¸°¨˙űŘř■ '
  };

  /** WHATWG label of the codepages `TextDecoder` can decode itself. */
  const TEXT_DECODER_CODE_PAGES: Record<number, string> = {
    866: 'ibm866',
    874: 'windows-874',
    932: 'shift_jis',
    936: 'gbk',
    949: 'euc-kr',
    950: 'big5',
    1200: 'utf-16le',
    1250: 'windows-1250',
    1251: 'windows-1251',
    1252: 'windows-1252',
    1253: 'windows-1253',
    1254: 'windows-1254',
    1255: 'windows-1255',
    1256: 'windows-1256',
    1257: 'windows-1257',
    1258: 'windows-1258',
    20866: 'koi8-r',
    21866: 'koi8-u',
    28591: 'iso-8859-1',
    28592: 'iso-8859-2',
    28605: 'iso-8859-15',
    65001: 'utf-8'
  };

  function labelDecoder(label: string): Decoder {
    try {
      const decoder = new TextDecoder(label);
      return (bytes) => decoder.decode(bytes);
    } catch {
      window.showWarningMessage(`Unsupported ${PROJECTS.CONFIG.full(PROJECTS.CONFIG.RUN_OUTPUT_ENCODING)} value "${label}" – decoding run output as UTF-8.`);
      return labelDecoder('utf-8');
    }
  }

  function tableDecoder(upperHalf: string): Decoder {
    return (bytes) => {
      let text = '';
      for (const byte of bytes) text += byte < 0x80 ? String.fromCharCode(byte) : upperHalf[byte - 0x80];
      return text;
    };
  }

  function codePageDecoder(codePage: number): Decoder {
    const upperHalf = OEM_UPPER_HALF[codePage];
    if (upperHalf) return tableDecoder(upperHalf);
    const label = TEXT_DECODER_CODE_PAGES[codePage];
    // An unknown OEM codepage still shares ASCII and most Western accents with CP437.
    return label ? labelDecoder(label) : tableDecoder(OEM_UPPER_HALF[437]);
  }

  /**
   * `ansi` (the default) is what a program writing to a redirected handle
   * normally produces; `oem` is the console codepage it would get writing to a
   * real console; `auto` takes a line as UTF-8 when it is valid UTF-8 and as the
   * ANSI codepage otherwise.
   */
  function decoderFor(encoding: string): Decoder {
    const value = encoding.trim().toLowerCase();
    if (value === 'ansi') return codePageDecoder(systemCodePage());
    if (value === 'oem') return codePageDecoder(consoleCodePage());
    if (value === 'auto') {
      const strictUtf8 = new TextDecoder('utf-8', { fatal: true });
      const fallback = codePageDecoder(systemCodePage());
      return (bytes) => {
        try {
          return strictUtf8.decode(bytes);
        } catch {
          return fallback(bytes);
        }
      };
    }
    const codePage = /^(?:cp|ibm|oem)?(\d{3,5})$/.exec(value);
    if (codePage) return codePageDecoder(Number(codePage[1]));
    return labelDecoder(value === 'utf8' ? 'utf-8' : value);
  }

  let cachedSystemCodePage: Option<number>;

  /**
   * Windows' system ANSI codepage (`GetACP`), read once from the registry value
   * the API itself is backed by. CP1252 when that fails or off Windows.
   */
  function systemCodePage(): number {
    if (cachedSystemCodePage !== undefined) return cachedSystemCodePage!;
    cachedSystemCodePage = 1252;
    if (process.platform === 'win32')
      try {
        const output = spawnSync('reg.exe', ['query', 'HKLM\\SYSTEM\\CurrentControlSet\\Control\\Nls\\CodePage', '/v', 'ACP'], {
          encoding: 'ascii',
          windowsHide: true
        }).stdout ?? '';
        const match = /ACP\s+REG_SZ\s+(\d{3,5})/i.exec(output);
        if (match) cachedSystemCodePage = Number(match[1]);
      } catch {}
    return cachedSystemCodePage!;
  }

  let cachedConsoleCodePage: Option<number>;

  /**
   * The codepage a spawned console process writes in: the extension host has no
   * console of its own, so a child gets a fresh one at the system OEM codepage,
   * which is what `chcp` reports when asked the same way. Queried once; CP437
   * when the query fails or off Windows.
   */
  function consoleCodePage(): number {
    if (cachedConsoleCodePage !== undefined) return cachedConsoleCodePage!;
    cachedConsoleCodePage = 437;
    if (process.platform === 'win32')
      try {
        const output = spawnSync('cmd.exe', ['/d', '/s', '/c', 'chcp'], { encoding: 'ascii', windowsHide: true }).stdout ?? '';
        const match = /(\d{3,5})/.exec(output);
        if (match) cachedConsoleCodePage = Number(match[1]);
      } catch {}
    return cachedConsoleCodePage!;
  }
}
