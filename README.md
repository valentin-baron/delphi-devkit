# Delphi DevKit README

Utilities for developing in Delphi using VS Code.

This project is not affiliated, associated, authorized, endorsed by, or in any way officially connected with Embarcadero Technologies, or any of its subsidiaries or its affiliates. The official Embarcadero Technologies website can be found at [https://www.embarcadero.com/](https://www.embarcadero.com/).

This extension does not include any proprietary Embarcadero code, libraries or binaries. To build Delphi projects, you must have a valid Delphi installation and the necessary environment variables set up.

## Features

* **File Navigation**: .pas <-> .dfm swapping with Alt+F12 hotkey
* **Smart Navigation**: .dfm -> .pas jumps with Ctrl+click
* **Dual Project Views**: Two separate project management approaches:
  - **Self-Defined Workspaces**: User-customizable project workspaces with drag & drop support
  - **Loaded Group Project**: Load and manage Delphi group projects (.groupproj) - readonly view
* **Multi-Compiler Support**: Configure and switch between multiple Delphi versions
* **Project Management**: Compile, recreate, run, and manage Delphi projects with keyboard shortcuts
* **Workspace Management**: Create, rename, remove workspaces and move projects between them
* **File System Integration**: Show projects in Explorer, open in File Explorer
* **Configuration Management**: Create and configure .ini files for executables
* **Visual Indicators**: File icons for Delphi files and missing file indicators
* **Configuration Import/Export**: Backup and restore your entire DDK configuration
* **Database-Driven**: Internal database for persistent project and workspace management

## Commands

### File Navigation
* `Delphi Utils: Swap .DFM/.PAS` - Switch between form and unit files (Alt+F12)

### Project Management
* `Select Delphi Compiler for Group Projects` - Choose the active compiler configuration for .groupproj files
* `Pick Group Project` - Load a Delphi group project (.groupproj)
* `Unload Group Project` - Unload the currently loaded group project
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

### Project Actions (Available via context menu and keyboard shortcuts)
* `Compile Selected Project` - Compile the selected project (Ctrl+F9)
* `Recreate Selected Project` - Clean and rebuild the selected project (Shift+F9)
* `Run Selected Project` - Execute the selected project (F9)
* `Configure/Create .ini` - Create or edit INI configuration files

## Extension Settings

### Compiler Configurations

* `ddk.compiler.configurations`: Array of Delphi compiler configurations

Each compiler configuration includes:

* `name`: Display name for the Delphi version
* `rsVarsPath`: Path to RSVars.bat file
* `msBuildPath`: Path to MSBuild.exe
* `buildArguments`: Default build arguments

#### Example Configuration

```json
{
  "ddk.compiler.configurations": [
    {
      "name": "Delphi 12",
      "rsVarsPath": "C:\\Program Files (x86)\\Embarcadero\\Studio\\23.0\\bin\\rsvars.bat",
      "msBuildPath": "C:\\Windows\\Microsoft.NET\\Framework\\v4.0.30319\\MSBuild.exe",
      "buildArguments": [
        "/verbosity:minimal",
        "/p:DCC_DebugInformation=1",
        "/p:Configuration=Debug"
      ]
    }
  ]
}
```

## Project Views

### Self-Defined Workspaces
- **Customizable**: Create and organize your own project workspaces
- **Drag & Drop**: Move projects within and between workspaces
- **Compiler Assignment**: Each workspace has a predefined compiler
- **Persistent**: Projects and workspaces are stored in the internal database

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

## Known Issues

None so far.

## Release Notes

Users appreciate release notes as you update your extension.

### 1.0.0

- Complete rewrite with dual project view system
- Added Self-Defined Workspaces with drag & drop support
- Added Loaded Group Project view for .groupproj files
- New internal database system for persistent storage
- Configuration import/export functionality
- Enhanced visual indicators and file icons
- Improved compiler management and project actions
- Added keyboard shortcuts for common operations
