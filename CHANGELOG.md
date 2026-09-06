# Change Log

All notable changes to the "delphi-devkit" extension will be documented in this file.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

## [Unreleased]

### Added

- **Debug target** (`ddk debug-target`, MCP `delphi_get_debug_target`, LSP `debug/target`): a debugger-agnostic description of what debugging a project means — the executable to launch or attach to (the program, or the Host Application loading a package/DLL), the `.map`/`.rsm` next to it, the project's own `.bpl`/`.dll` module with its symbols and `.dcp` (found where the IDE puts them), the source search paths (project directory, dproj unit/include paths, the IDE's Library and Browsing Paths, the compiler's `source` tree; macros expanded through `rsvars.bat` and the IDE's environment-variable overrides; existing directories only), the run arguments as `Run` passes them, config/platform/bitness, and warnings about missing or stale artefacts. Resolves like `ddk compile` (id, name or path; ad-hoc for an unmanaged path). It is the contract a debugger integration builds its launch/attach configuration from, so DevKit never has to know a debugger's own configuration format.

- **Debug / Attach Debugger (VS Code)**: with an extension contributing the `delphi` debug type installed, every project gets *Debug* and *Attach Debugger* actions (context menu, command palette, Ctrl+Alt+F9 for the selected project) and one dynamic entry per project in the debug dropdown. They start the two-line configuration `{ "type": "delphi", "request": "launch" | "attach", "ddkProject": "<name or id>" }` — usable verbatim in a hand-written `launch.json` — which the debugger extension resolves by asking DDK for the project's debug target through the new `ddk.debug.getDebugTarget` command. A launch first compiles the project for debugging (`ddk.debug.compileBeforeDebug`, on by default); an attach never compiles. Nothing appears while no such debugger is installed, and DDK never writes a `launch.json`.
- **Debug-info builds** (`ddk compile --debug-info`, MCP `debug_info: true` on `delphi_compile_project`/`delphi_compile_file`, VS Code *Compile for Debugging* on a project, a workspace, the group project and the selected project): compile with the full debug artefact set a debugger needs — optimizations off, TD32 debug info in the binary, the `.rsm` remote-debug symbols and a detailed `.map` — regardless of what the selected build configuration says. The overrides travel as MSBuild global properties (`/p:DCC_Optimize=false`, `/p:DCC_DebugInfoInExe=true`, `/p:DCC_RemoteDebug=true`, `/p:DCC_MapFile=3`, …) or as extra `dcc` switches (`-$O- -V -VN -VR -GD`) for a bare `.dpr`/`.dpk`; the dproj is never modified. Off by default: a plain compile is unchanged.

## [2.6.0] - 2026-08-03

### Added

- **DelphiLSP settings generation** (`ddk delphilsp-config`, MCP `delphi_generate_delphilsp_config`, LSP `delphilsp/generate`): write the `<project>.delphilsp.json` settings file Embarcadero's DelphiLSP VS Code extension needs for code insight, without ever opening RAD Studio. The file is reconstructed from the project's `.dproj` (evaluated for the effective configuration/platform), the compiler's `rsvars.bat`, and the IDE's global Library Path / Browsing Path / environment-variable overrides read from the registry. Works on managed projects (ID/name) and ad-hoc `.dproj` paths (`-c` picks the compiler, `-o` overrides the destination). Files DDK writes carry a `"generatedBy": "delphi-devkit"` marker; IDE-generated files are never touched.
- **DelphiLSP auto-sync (VS Code)**: when the DelphiLSP extension is installed, DDK keeps its `delphiLsp.settingsFile` pointed at the DDK active project — generating the settings file on the fly when missing (or stale and DDK-owned) — and re-validates every open Delphi file under the new project context after each switch (something DelphiLSP's own *Select project settings* command does not do). New **Generate DelphiLSP Config** project context-menu action, and `ddk.delphilsp.autoSync` / `ddk.delphilsp.revalidateOpenFilesOnSwitch` settings (both on by default). Nothing appears when DelphiLSP is not installed.
- **Merged diagnostics (VS Code)**: with DelphiLSP installed, the Problems view no longer shows the same error twice (once from DDK's last compile, once from DelphiLSP's live analysis). DDK compile diagnostics matching a live DelphiLSP entry (same line, same error code) are removed in favor of the live entry's precise token range — and once DelphiLSP reports the error as fixed in the editor, the compile copy stays gone even without recompiling. Compile diagnostics DelphiLSP has no opinion on (unvalidated files, link errors) keep their normal lifetime; without DelphiLSP nothing is ever removed. `ddk.delphilsp.mergeDiagnostics` setting (on by default).

## [2.5.1] - 2026-07-31

### Added

- **Fully machine-coded `ddk compile --json` (and MCP compile tools)**: the compile result is now entirely structured — the header banner is split into fields (`project`, `project_path`, `compiler`, `config`, `platform`, `action`) and all recognised compiler messages live in a `diagnostics` object grouped into `errors`/`warnings`/`hints`, each entry `{code, file, line, message}` (absolute path, numeric line). Consumers read fields directly instead of scraping log text. Diagnostics obey the `--show-warnings`/`--show-hints` filters (errors always present; warnings/hints only with their flag), so a slimmed compile stays slim. The MCP `delphi_compile_project`/`delphi_compile_file` tools now return this same JSON (previously a human-readable text blob). **Breaking:** the old `--json` shape (`project_name`, free-text `lines[]`, `cancelled`) is replaced — the raw `lines[]` array and the `cancelled` field are gone (a one-shot CLI/MCP compile cannot be cancelled), and `project_name` is now `project`. The human (non-`--json`) output is unchanged.
- **`-e`/`--encoding` for `ddk compile`**: force the encoding used to decode compiler output (e.g. `windows-1252`, `utf-8`, `oem`), mirroring `ddk format`. The `DDK_COMPILER_ENCODING` environment variable is honoured as a fallback. Defaults to `oem`, which now auto-detects the active console output codepage.
- **`--fail-on-error` for `ddk compile`**: opt-in flag that makes the process exit with the compiler's exit code when a compile fails, instead of always exiting `0`. Left off by default so existing callers that only parse the JSON are unaffected; JSON output is unchanged.

### Fixed

- **Compiler output decoded with the wrong codepage**: `ddk compile` always decoded compiler output as the system OEM codepage (`GetOEMCP`). When the console codepage differed — e.g. under `chcp 1252` or `chcp 65001` — the child compiler wrote its bytes in the console codepage, so non-ASCII characters (umlauts in messages and file paths) were corrupted in the bytes DDK itself wrote to stdout, which a consumer could not repair. DDK now decodes compiler output using the active console output codepage (`GetConsoleOutputCP`, what `chcp` sets), falling back to the system OEM codepage when no console is attached (e.g. a detached LSP server, so its behaviour is unchanged). File reads used by `ddk format` still use the system OEM codepage.
- **Correct selection (range) formatting**: formatting a selection previously sent only the selected text to the DDK formatter, which formatted it out of context — a fragment is not valid on its own, so the result lost the enclosing indentation and mis-laid-out anything that depends on surrounding blocks. DevKit now formats the **whole document** and maps the selection back onto the formatted result, so a selection formats exactly as it would as part of the full file. The applied edit still touches only the selected code (the rest of the file is left untouched); a selection that starts or ends in whitespace snaps to the enclosing code; the first selected line's indentation is reformatted too (so a mis-indented leading comment is corrected, not left in place); and a whitespace-only selection is a no-op. The mapping is anchored on non-whitespace characters, so formatter changes that only affect whitespace, line endings (LF ↔ CRLF), or letter case never misalign it, and multibyte text (UTF-16 ↔ UTF-8 offsets) is handled correctly.

## [2.5.0] - 2026-07-30

### Added

- **Extra MSBuild arguments for `ddk compile`**: anything after a `--` separator on the `ddk compile` command line is now passed verbatim to MSBuild, e.g. `ddk compile be -- /p:DCC_Define=FOO /m`. The extra arguments are appended after DDK's built-in `/p:Config`/`/p:Configuration`/`/p:Platform` args, so a `/p:` override supplied this way wins (MSBuild takes the last value for a duplicated property). They have no effect on a bare `.dpr`/`.dpk` target — which is compiled with the command-line compiler (`dcc32`/`dcc64`) rather than MSBuild — and DDK prints a note listing the ignored arguments in that case. The MCP compile tools are unchanged.
- **Host Application support for packages and DLLs**: a project without an own executable (e.g. a `.dpk` runtime package) is now runnable. DevKit reads the `Debugger_HostApplication` set via Project > Options > Debugger in the Delphi IDE from the project's active `.dproj` property group (resolved for the effective configuration/platform, with `$(ProjectDir)`/`$(ProjectName)`/`$(Platform)`/`$(Config)` expanded and relative paths resolved against the project directory), and a new **"Set Host Application"** project context-menu command stores a DevKit-side override that wins over the dproj value. Running such a project (context menu "Run", "Run Selected Project" / `F9`, `ddk run`, or the MCP run tools) launches the host executable with the project's usual run parameters — matching the Delphi IDE, where Run on a package starts its Host Application. A package's `Debugger_RunParams` are now discovered too, and `ddk project list` / `delphi_list_projects` show the effective host next to the exe.
- **Run-target visibility in the project tree**: hovering a project now shows a tooltip with its exe, the effective Host Application (marking whether it comes from the DevKit override or the dproj) and the effective run parameters (saved Start Parameters, the dproj's `Debugger_RunParams`, or both fused). A project that runs through a hosting executable also shows an inline `⇢ HostApp.exe` hint next to its name.

## [2.4.1] - 2026-07-29

### Changed

- **Simplified project/workspace ordering (internal)**: the ordering of workspaces and project links is now derived solely from their position in the persisted list, removing the per-item `sort_rank` (LexoRank) field that previously encoded it. The list order was already authoritative — every reorder operation rewrote the ranks to match it — so the field was redundant. The LexoRank module and its now-unused `substring` dependency were removed. No user-facing behavior change; existing saved data with leftover `sort_rank` values still loads (the unknown field is ignored).

## [2.4.0] - 2026-07-14

### Added

- **Start Parameters for RunProgram**: new "Set Start Parameters" context menu item on projects lets you configure extra command-line arguments passed to the executable. Running a project (via the context menu "Run" action or "Run Selected Project" / `F9`) now launches the executable directly with those arguments instead of via the OS shell handler, which previously could not pass arguments at all. By default the project's own `Debugger_RunParams` (Project > Options > Run in the Delphi IDE), read from the active `.dproj` property group for the project's effective configuration/platform, is fused with the saved Start Parameters — dproj first, then the saved value appended — so `Run` behaves like pressing Run in the IDE plus whatever extra parameters were saved on top; disable the new `ddk.projects.useDebuggerRunParams` setting to always use only the saved Start Parameters instead.
- **`ddk run` (CLI + MCP)**: run a project's built executable directly, with the same parameter resolution as `ddk compile` — a project ID, a project name (`-p`/`--project`, listing candidates if ambiguous), a `.dproj`/`.dpr`/`.dpk` path (resolved to its owning managed project), or a `.exe` path (launched directly). `--args`/`-a` overrides the run parameters for that one run (which otherwise fuse the dproj's `Debugger_RunParams` with the project's saved Start Parameters, dproj first). Unlike `compile`, `run` never builds or assembles ad-hoc state — the executable must already exist. Exposed to AI tooling via the MCP `delphi_run_project` and `delphi_run_file` tools. The process is launched detached; the command/tool call returns immediately without waiting for it to exit.

## [2.3.0] - 2026-06-15

### Added

- **Ad-hoc compile by path (CLI + MCP)**: compile a `.dproj`/`.dpr`/`.dpk` that has not been added to any workspace.
  - CLI: `ddk compile <PATH>`, with `--compiler`/`-c` to pick the compiler (exact key like `12.0` or product name like `"Delphi 12"`; defaults to the newest installed), plus optional `--config` / `--platform` build overrides.
  - MCP: new `delphi_compile_file` tool with the same options.
  - If the path already belongs to a managed project (its `.dproj`/`.dpr`/`.dpk` matches one), it is compiled as that project — identical to referencing it by name — and a path shared by several projects lists the candidates instead of compiling. Only a file owned by no project is compiled ad-hoc, against an ephemeral in-memory project (one throw-away workspace bound to the chosen compiler); the persisted project/workspace state is never read or modified.
- **Compile by project name (CLI + MCP)**: the compile target may now be a project **name** as well as a numeric id. CLI `ddk compile -p <NAME>` and the MCP `delphi_compile_project` `project` parameter accept either. The CLI also accepts a bare positional shorthand — `ddk compile <NAME|ID>` is identical to `-p` (a TARGET ending in `.dproj`/`.dpr`/`.dpk` is treated as an ad-hoc file path instead). When a name matches a single project it is compiled; when it matches several, the candidate projects are listed (ID, workspace, path) instead of compiling, e.g.:
  ```
  Project "be" matches multiple projects:
  - ID 123 = Workspace 1 - be (path\to\be.dpr)
  - ID 124 = Workspace 2 - be (other\be.dpr)
  Re-run targeting the specific project ID to compile the correct one.
  ```
- **Add projects and workspaces from CLI + MCP**:
  - CLI: `ddk projects add <PATH> <WORKSPACE>` adds a project file to an existing workspace (workspace resolved by name or numeric id); `ddk projects add_workspace <NAME> <COMPILER>` creates a workspace bound to a compiler. `ddk projects` is an alias of `ddk project`.
  - MCP: new `delphi_add_project` and `delphi_add_workspace` tools.
  - Compiler references accept an exact key or a product-name (sub)string, e.g. `"Delphi 12"`.
- **Compile projects without a `.dproj`**: a project consisting of only a `.dpr` or `.dpk` (no `.dproj`) is now a fully valid, compilable project. Previously MSBuild was handed the bare source file and failed with `MSB4025` ("invalid project file"). DevKit now detects the missing `.dproj` and compiles such projects with the Delphi command-line compiler (`dcc32`/`dcc64`) directly, while projects that have a `.dproj` continue to build through MSBuild as before.
  - **Configuration / platform selection** is offered for bare projects too: since there is no `.dproj` to enumerate, DevKit synthesises `Debug`/`Release` configurations and `Win32`/`Win64` platforms. The selected platform picks the compiler (`Win32` → `dcc32`, `Win64` → `dcc64`); the configuration maps to the relevant `-$` compiler switches. Defaults are `Win32` + `Debug` when nothing is selected.
  - Bare projects now also resolve their executable / INI paths from the source name (a `.dpk` package has no standalone executable), so they no longer fail discovery on add.

## [2.2.0] - 2026-06-02

### Fixed

- **Exe path resolution for projects with dotted names**: projects like `example.test.external.dproj` now correctly resolve their executable path (e.g. `example.test.external.exe`) using `dproj-rs` 0.3.0's improved `ProjectName` and `DCC_ExeOutput` / `DCC_DependencyCheckOutputName` handling.
- **Stale exe paths after DDK upgrade**: previously, persisted project data could retain outdated exe/ini paths from older DDK versions. Developer can rediscover paths for affected projects via the "Discover File Paths" command to refresh them.

## [2.1.3] - 2026-04-28

### Added

- **Compile output filtering for CLI and MCP**: the `ddk compile` CLI command and the MCP `delphi_compile_project` tool now strip the decorative banner box from compiler output and suppress warnings and hints by default to reduce token usage for AI consumers. Errors and the final status line are always shown. New flags (CLI) / parameters (MCP):
  - `--show-warnings` / `show_warnings`: include warning lines verbatim.
  - `--show-hints` / `show_hints`: include hint lines verbatim.
  - `--summarize-diagnostics` / `summarize_diagnostics`: append a per-file `<file>: X warn, Y hint` summary for any warnings or hints that were suppressed.

  The VS Code extension's compiler output (delivered via the LSP server) is unaffected.

### Fixed

- **Configuration override now actually takes effect during compilation**: the per-project / per-workspace / per-group-project configuration override (Debug, Release, …) was silently ignored. Delphi's `.dproj` conditional `PropertyGroup`s are keyed off the `$(Config)` MSBuild property, but the compiler invocation was only setting `$(Configuration)` — so MSBuild fell through to the dproj's default. The compile command now sets both `Config` and `Configuration`, so overrides actually apply.


## [2.1.2] - 2026-03-23

### Added

- **Compilation status bar item**: a new status bar item appears while a build is in progress, showing a spinning icon and the current project name. On completion it briefly displays a success, failure, or cancellation message before disappearing.
- **`ddk.compiler.resultTimeout` setting**: controls how long (in milliseconds) the compilation result is shown in the status bar after a build finishes. Defaults to `5000`. Set to `0` to disable the result display entirely.
- **Quick Pick Project** (`Ctrl+Shift+Alt+F1`): new command that opens a grouped quick-pick list of all projects across workspaces and the loaded group project, with the active project pre-marked. Selecting a project applies the change immediately.

### Changed

- **MCP tool renamed**: `delphi_compile_selected_project` → `delphi_compile_project`. The tool description has been simplified.
- **Compile without side-effects**: `cmd_compile` no longer calls `cmd_select_project` when a `project_id` is provided; the target project is resolved by ID without changing the active project in state.
- **`ProjectSummary` now includes `exe`**: the resolved executable path is included in project list results; the CLI `project list` output now shows it below each project entry.

### Removed

- Removed several unused utility functions: `removeBOM`, `findIniFromExecutable`, `assertWarning`, `assertInfo`, `getCompilerOfWorkspace`, `getGroupProjectOfLink`, and `BaseFileItem.projectUri`.

## [2.0.5] - 2026-03-02

### Added

- **DDK CLI** (`ddk`): new standalone command-line interface for managing Delphi projects and compilers outside of VS Code. Subcommands: `project list`, `project select <ID>`, `compiler list`, `compiler set <KEY>`, `compile [--rebuild] [--project <ID>]`, `env`, `info`. Supports a global `--json` flag for machine-readable output.
- **Shared commands module** (`ddk_core::commands`): business logic extracted from the MCP handler into typed, reusable command functions. Both the MCP server and CLI are now thin wrappers over the same implementation.
- **Transfer Group Project to Workspace**: new command that converts the loaded group project into a self-defined workspace, carrying over all project links and the compiler configuration.
- **WinGet distribution**: `ddk.exe` is published as a GitHub Release asset (`ddk-windows-x86_64.zip`) and submitted to the Windows Package Manager (`winget install ValentinBaron.DDK`). Installs to PATH automatically via WinGet's portable installer support.

### Changed

- **MCP `delphi_compile_project`**: now accepts an optional `project_id` parameter. When provided, the project is selected before compilation — no need to call `delphi_select_project` separately.
- **MCP handler thinned**: all inline business logic replaced with calls to `ddk_core::commands`, reducing the handler from ~450 lines to ~120 lines.
- **Release workflow**: builds all workspace crates (not just `server/`), creates the CLI zip artifact, verifies all three binaries (`ddk-server.exe`, `ddk-mcp-server.exe`, `ddk.exe`) are in the VSIX, and auto-submits WinGet manifest updates on stable releases.

### Fixed

- **Configuration view icon**: corrected to use the new asset path.

## [2.0.4] - 2026-03-02

### Added

- **DDK Configuration panel**: new collapsed-by-default tree view listing all DDK config files (Default INI, Projects Data, Compiler Configurations, Formatter Configuration, Extension Settings). Clicking any item opens the file in the editor, creating it with sensible defaults if it does not yet exist.
- **Formatter config seeding**: `ddk_formatter.config` is now seeded from the bundled default template on first open (instead of being created empty), so it is immediately usable before the first format operation.
- **Run notification**: a status message is now shown when a project executable is launched.

### Changed

- **Import / Export redesign**: the single combined JSON import/export command has been replaced with four separate RON-based commands — Export Projects Data, Import Projects Data, Export Compiler Configurations, Import Compiler Configurations — operating directly on the `.ron` files.
- **Default INI location**: the default INI template is now stored in `%APPDATA%\ddk\default.ini` (previously inside the extension's `dist/` folder), making it easy to customise before applying it to a project.
- **Formatter config location**: `ddk_formatter.config` is now resolved from `%APPDATA%\ddk\` consistently on both the extension side and the Rust server side.

### Fixed

- **Windows `\\?\` path prefix**: paths returned by `dproj-rs` that carry the extended-length path prefix are now stripped before use, preventing "file not found" errors on path operations.
- **Discover File Paths overrides**: `discover_paths()` now correctly forwards per-project `active_configuration` and `active_platform` overrides; partial overrides (only one set) are handled by reading the dproj to fill in the missing default.
- **Discover File Paths on add**: `discover_paths()` is now called immediately when a project is added via `new_project()`, so executable and INI paths are populated without requiring a manual refresh.
- **Keyboard shortcut context keys**: `ddk:isProjectSelected` and `ddk:doesSelectedProjectHaveExe` context keys are now updated on every data notification and refresh, not only during tree rendering, so keybindings work reliably after reloads.
- **INI file open error**: `vscode.open` in `createIniFile` now receives a proper `Uri.file()` object instead of a raw string.
- **Command palette pollution**: 26 context-menu-only commands are now hidden from the Command Palette via the `commandPalette` menu section.
- **esbuild asset path**: the formatter config preset was being copied from the wrong source directory; corrected to `core/src/format/presets/`.

## [2.0.3] - 2026-03-02

### Changed

- **MCP server**: moved into its own standalone binary (`ddk-mcp-server`).
- **DPROJ handling**: simplified file handling and integrated `dproj-rs` for parsing.

## [2.0.2] - 2026-02-27

### Added

- **MCP server tools (BETA)**: `delphi_list_projects`, `delphi_select_project`, `delphi_get_available_compilers`, and `delphi_set_group_projects_compiler` — enabling AI agents to discover, select, and configure projects and compilers.

### Fixed

- README and CHANGELOG are now included in the VSIX package, so the VS Code Marketplace store page displays them correctly.
- Added `repository` field to `package.json` so relative image paths resolve on the Marketplace.

## [2.0.0] - 2026-02-26

### Breaking Changes

- Removed SQLite database in favor of file-based storage (`%APPDATA%\ddk\projects.ron`, `%APPDATA%\ddk\compilers.ron`).
- Previously stored workspaces and projects **will not be migrated**. You will need to re-add them.
- Compiler configurations are no longer in VS Code settings (`ddk.compiler.configurations`); they are now managed via `compilers.ron` and the `Edit Compiler Configurations` command.

### Changed

- Full architectural rewrite: extension now communicates with a bundled Rust LSP server (`ddk-server`) over stdio. All project state, compilation, formatting, and file watching run server-side.
- Repository split into `server/` (Rust crate) and `vscode_extension/` (TypeScript).
- Drag & drop project ordering is now managed server-side.
- Author name in LICENSE/NOTICE changed

### Added

- **DDK Server**: bundled `ddk-server.exe` (Rust, async tower-lsp) handles all backend logic.
- **Preset compiler configurations**: 19 built-in entries covering Delphi 2007 through Delphi 13.0 Florence.
- **Bulk compilation**: compile or recreate all projects in a workspace or group project in a single command.
- **Cancellable compilation**: cancel any active MSBuild run via `Cancel Compilation` (Ctrl+F2); uses `taskkill /F /T` to terminate the entire process tree.
- **Formatter**: format Delphi source files via `GExperts.Formatter.exe`; configuration file editable and resettable via commands.
- **Timestamps in compiler output**: every output line is prefixed with `HH:MM:SS.mmm`.
- **Diagnostics in Problems panel**: MSBuild errors, warnings, and hints are parsed and published as LSP diagnostics.
- **File watchers**: `projects.ron` and `compilers.ron` are watched for external changes; tree views update automatically.
- **New commands**: `Compile All in Workspace`, `Recreate All in Workspace`, `Compile All in Group Project`, `Recreate All in Group Project`, `Cancel Compilation`, `Set Manual Path`, `Edit Compiler Configurations`, `Edit Projects Data`, `Edit Formatter Config`, `Reset Formatter Config`.
- **Compiler output encoding**: configurable via `ddk.compiler.encoding` setting (`oem` default).

### Fixed

- Fixed the issue where the selected project didn't work when the tree was collapsed.
- Fixed the issue where removing workspaces/projects did not work.
- Fixed the issue where Discover File Paths did not do anything.

## [1.1.0] - 2025-08-31

- Fixed the issue where the compiler's diagnostic output was always mapped as information.
- Added error code to diagnostics.
- Added support for hyperlinks in compiler output channel, enabling error/hint/warning codes to link directly to the Embarcadero documentation, and file paths to resolve.

## [1.0.0] - 2025-08-30

- Removed Project Discovery in favor of a more streamlined approach.
- Added File Explorer Icons for better visual identification of Delphi files.
- Split Delphi Projects into 2 separate views:
    - "Self-Defined Workspaces" for user-defined projects. (customizable)
    - "Loaded Group Project" for projects loaded from a .groupproj file. (readonly)
- Compiler picker is now only relevant for "Loaded Group Project" projects, so it has been clarified.
- Completely reworked backend database (you can delete old .cachedb files).
> Note: Old logic used to automatically discover projects and created a .cachedb file for each workspace (VS Code Workspace Folders + Git Status Hashed). That's why you likely can find multiple .cachedb files in the extension storage folder.

### Internal Database

- The internal database now has a root element called Configuration. This can be imported/exported as JSON using commands:
    - Export Configuration
    - Import Configuration

### Workspaces

- Added Self-Defined Workspaces:
    - Workspaces are user-defined tree items that can contain projects.
    - They have a predefined compiler assigned by the user, so all projects within the workspace will use that compiler.
    - You can create multiple workspaces and move them around the tree view as you like.
    - Dragging and dropping projects inside the tree view will move them inside or between workspaces.
    - Dragging and dropping projects from the Loaded Group Project view to a Self-Defined Workspace will copy the project to that workspace.
    - Project files are more clearly shown when missing (e.g. due to git branch variations)
- Added commands to manage Self-Defined Workspaces:
    - Add Workspace
    - Rename Workspace
    - Remove Workspace
    - Add Project
    - Remove Project
    - Discover File Paths (Reinstate the file paths in the project's database entry)

## [Unreleased]

- Initial release

# Bug Roadmap

- [x] Fixing Diagnostics to show correct types of issues.
- [x] Selected Project doesnt work when the tree is collapsed.
- [x] Removing Workspaces/Projects does not work.

# Feature Roadmap

- [x] Linking the compiler output to files.
- [x] Add timestamps to compiler output lines.
- [x] Delphi Formatter
- [x] Support for compiling / recreating all projects in a workspace / group project.
- [ ] Support for commandline execution of unit tests (DUnit).
- [ ] Integrate Delphi Language Server with background compiler. For now, you can use the [OmniPascal extension](https://marketplace.visualstudio.com/items?itemName=Wosi.omnipascal).