# Delphi DevKit

[![GitHub Release](https://img.shields.io/github/v/release/valentin-baron/delphi-devkit?include_prereleases&label=latest)](https://github.com/valentin-baron/delphi-devkit/releases)

Utilities for developing in Delphi using VS Code.

This project is not affiliated, associated, authorized, endorsed by, or in any way officially connected with Embarcadero Technologies, or any of its subsidiaries or its affiliates. The official Embarcadero Technologies website can be found at [https://www.embarcadero.com/](https://www.embarcadero.com/).

This extension does not include any proprietary Embarcadero code, libraries or binaries. To build Delphi projects, you must have a valid Delphi installation and the necessary environment variables set up. This extension does not work with the Delphi Community Edition due to its limitations on command-line compilation.

This extension is currently developed in my free time, and any feedback is welcome.

## Features

* **Dual Project Views**: Two separate project management approaches:
  - **Self-Defined Workspaces**: User-customizable project workspaces with drag & drop support
  - **Loaded Group Project**: Load and manage Delphi group projects (.groupproj) - readonly view
* **Multi-Compiler Support**: Configure and switch between multiple Delphi versions (Delphi 2007 → Delphi 13.0 Florence built-in)
* **Project Management**: Compile, recreate, run, and manage Delphi projects with keyboard shortcuts
* **Bulk Compilation**: Compile or recreate all projects in a workspace or group project at once; cancellable at any time
* **Workspace Management**: Create, rename, remove workspaces and move projects between them
* **File System Integration**: Show projects in Explorer, open in File Explorer
* **Configuration Management**: Create and configure .ini files for executables
* **Visual Indicators**: File icons for Delphi files and missing file indicators
* **Configuration Import/Export**: Backup and restore your entire DDK configuration
* **File-Based Persistence**: Project and compiler data stored as RON files in `%APPDATA%/ddk/`
* **File Navigation**: .pas <-> .dfm swapping with Alt+F12 hotkey
* **Smart Navigation**: .dfm -> .pas jumps with Ctrl+click
* **Compiler Output Enhancements**: Timestamps, clickable file links, and diagnostics published to the Problems panel
* **Formatter Support**: Configurable Delphi code formatter.
* **LSP Server**: Bundled `ddk-server` (Rust) handles all project state, compilation, and formatting
* **MCP Server**: Bundled `ddk-mcp-server` exposes project and compiler management as MCP tools for AI assistants (VS Code Copilot, Claude Desktop, etc.)
* **CLI** (`ddk`): Standalone command-line interface for managing projects and compilers outside of VS Code. Install via WinGet or use the bundled binary.

## Installation

### VS Code Extension
Install from the [Visual Studio Marketplace](https://marketplace.visualstudio.com/items?itemName=Snowcaloid.delphi-devkit). The extension bundles all Rust binaries — no extra setup needed.

### CLI (standalone)
```
winget install ValentinBaron.DDK
```
Or download `ddk-windows-x86_64.zip` from the [latest release](https://github.com/valentin-baron/delphi-devkit/releases) and add `ddk.exe` to your PATH.

### CLI Usage
```
ddk project list                       # List all known projects
ddk project select <ID>                # Select a project by ID
ddk projects add <PATH> <WORKSPACE>    # Add a .dproj/.dpr/.dpk to a workspace (by name)
ddk projects add_workspace <NAME> <COMPILER>  # Create a workspace bound to a compiler
ddk compiler list                      # List available compiler configurations
ddk compiler set <KEY>                 # Set the group project compiler
ddk compile                            # Compile the active project
ddk compile <ID|NAME>                  # Compile a project by ID or name (= -p; lists candidates if ambiguous)
ddk compile -p <ID|NAME>               # Same, explicit flag form
ddk compile --rebuild -p <ID>          # Rebuild a specific project by ID
ddk compile <PATH>                     # Compile a .dproj/.dpr/.dpk (uses the owning project if managed, else ad-hoc)
ddk compile <PATH> -c "Delphi 12"      # ...choosing the compiler (key or product name)
ddk compile <PATH> --config Release --platform Win64   # ...with build overrides
ddk compile --show-warnings            # Include warnings verbatim
ddk compile --show-hints               # Include hints verbatim
ddk compile --summarize-diagnostics    # Append `<file>: X warn, Y hint` per project
ddk run                                # Run the active project's executable
ddk run <ID|NAME>                      # Run a project by ID or name (= -p; lists candidates if ambiguous)
ddk run <PATH>                         # Run a .exe directly, or a .dproj/.dpr/.dpk's owning project
ddk run -a "-flag \"value with spaces\""  # Override the project's saved Start Parameters for this run
ddk env                                # Show active project & compiler info
ddk info                               # Print the DDK README
ddk --json <command>                   # Output as JSON
```

`ddk compile <PATH>` compiles a `.dproj`/`.dpr`/`.dpk`. If the file already
belongs to a managed project it is compiled as that project (and a file shared
by several projects lists the candidates instead of compiling); otherwise it is
compiled ad-hoc without adding it to the saved project list (handy for one-off
builds). For the ad-hoc case `--compiler`/`-c` selects the compiler by exact key
(`12.0`) or product name (`Delphi 12`), defaulting to the newest installed
compiler. The same is exposed to AI tooling via the MCP `delphi_compile_file`,
`delphi_add_project`, and `delphi_add_workspace` tools.

Compile output for the CLI and the MCP `delphi_compile_project` tool is
trimmed for AI / token-efficient consumption: the decorative banner box is
stripped and warning / hint lines are hidden by default. Errors and the final
status line are always shown.

`ddk run <PATH>` dispatches on the file extension: a `.exe` is launched
directly; a `.dproj`/`.dpr`/`.dpk` must already belong to a managed project
(its stored executable is run, same resolution as by name) — it is never
compiled or run ad-hoc. `--args`/`-a` overrides the project's saved Start
Parameters (see `Set Start Parameters` below) for that one run; the process
is launched detached, so the CLI/MCP call returns immediately without
waiting for it to exit. The same is exposed to AI tooling via the MCP
`delphi_run_project` and `delphi_run_file` tools.

## Demos

### Add a Workspace and drag in a Project
![Add workspace and drag project](vscode_extension/assets/add_workspace_drag_project.gif)

### Add a Project via dialog
![Add project dialog](vscode_extension/assets/add_project_dialog.gif)

### Load a Group Project
![Select group project](vscode_extension/assets/select_groupproj.gif)

### Compile a Project
![Compile selected project](vscode_extension/assets/compile_selected_project.gif)

### Transfer Group Project to Workspace
![Transfer group project to workspace](vscode_extension/assets/transfer_groupproj_to_ws.gif)

### Format Delphi Source
![Formatter](vscode_extension/assets/formatter.gif)

## Commands

### File Navigation
* `Swap .DFM/.PAS` - Switch between form and unit files (Alt+F12)

### Project Management
* `Select Delphi Compiler for Group Projects` - Choose the active compiler configuration for .groupproj files
* `Pick Group Project` - Load a Delphi group project (.groupproj)
* `Unload Group Project` - Unload the currently loaded group project
* `Transfer Group Project to Workspace` - Convert the loaded group project into a self-defined workspace
* `Refresh` - Refresh the projects view and discover file paths

### Workspace Management
* `Add Workspace` - Create a new self-defined workspace
* `Rename Workspace` - Rename an existing workspace
* `Remove Workspace` - Delete a workspace and its projects
* `Add Project` - Add projects to a workspace
* `Remove Project` - Remove projects from a workspace

### Configuration
* `Import Configuration` - Import DDK configuration from JSON file
* `Export Configuration` - Export DDK configuration to JSON file
* `Edit Default .ini` - Edit the default INI template file
* `Edit Formatter Config` - Edit the Delphi formatter configuration file
* `Reset Formatter Config` - Reset the formatter configuration to defaults
* `Edit Compiler Configurations` - Open the compiler configurations RON file directly
* `Edit Projects Data` - Open the projects data RON file directly

### Project Actions (Available via context menu and keyboard shortcuts)
* `Compile Selected Project` - Compile the selected project (Ctrl+F9)
* `Recreate Selected Project` - Clean and rebuild the selected project (Shift+F9)
* `Compile All in Workspace` - Compile all projects in a workspace
* `Recreate All in Workspace` - Clean and rebuild all projects in a workspace
* `Compile All in Group Project` - Compile all projects in the loaded group project
* `Recreate All in Group Project` - Clean and rebuild all projects in the loaded group project
* `Cancel Compilation` - Cancel the active compilation (Ctrl+F2)
* `Run Selected Project` - Execute the selected project (F9)
* `Set Start Parameters` - Configure command-line arguments passed to the executable when run
* `Configure/Create .ini` - Create or edit INI configuration files
* `Set Manual Path` - Manually set the .dproj path for a project

## Extension Settings

* `ddk.compiler.encoding`: Character encoding used to decode MSBuild output (`oem` by default, use `utf8` if your paths contain non-ASCII characters).

## Compiler Configurations

Compiler configurations are stored in `%APPDATA%\ddk\compilers.ron` and managed by the DDK server. The extension ships with built-in presets for all Delphi versions from **Delphi 2007** to **Delphi 13.0 Florence**.

You can view and edit them directly via the `Edit Compiler Configurations` command. Each entry includes:

* `product_name` / `compiler_version` / `package_version` — version identifiers
* `installation_path` — root path of your Delphi installation
* `build_arguments` — MSBuild arguments passed during compilation
* `condition` — optional expression to enable/disable the entry

The first entry whose `Formatter.exe` is found in its installation path is used for formatting.

## Project Views

### Self-Defined Workspaces
- **Customizable**: Create and organize your own project workspaces
- **Drag & Drop**: Move projects within and between workspaces
- **Compiler Assignment**: Each workspace has a predefined compiler
- **Persistent**: Projects and workspaces are stored in `%APPDATA%\ddk\projects.ron`

### Loaded Group Project
- **Read-Only**: View projects from .groupproj files
- **Compiler Selection**: Use the compiler picker for group project compilation
- **Cross-Copy**: Drag projects from group projects to self-defined workspaces

## Visual Indicators

* **Selected Project**: Shows `←S` indicator for the currently selected project
* **Missing Files**: Shows `!` indicator for files that don't exist
* **File Type Icons**: Custom icons for .pas, .dfm, .dpr, .dpk, .dproj files

## Keyboard Shortcuts

* `Alt+F12` - Swap between .PAS and .DFM files
* `Ctrl+F9` - Compile selected project (when project is selected)
* `Shift+F9` - Recreate selected project (when project is selected)
* `F9` - Run selected project (when project has executable)
* `Ctrl+F2` - Cancel active compilation

## Known Issues

None so far.
