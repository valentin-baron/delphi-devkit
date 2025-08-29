# Delphi DevKit README

Utilities for developing in Delphi using VS Code.

This project is not affiliated, associated, authorized, endorsed by, or in any way officially connected with Embarcadero Technologies, or any of its subsidiaries or its affiliates. The official Embarcadero Technologies website can be found at [https://www.embarcadero.com/](https://www.embarcadero.com/).

This extension does not include any proprietary Embarcadero code, libraries or binaries. To build Delphi projects, you must have a valid Delphi installation and the necessary environment variables set up.

## Features

* **File Navigation**: .pas <-> .dfm swapping with Alt+F12 hotkey
* **Smart Navigation**: .dfm -> .pas jumps with Ctrl+click
* **Project Explorer**: Delphi Projects tree view with drag & drop support
* **Multi-Compiler Support**: Configure and switch between multiple Delphi versions
* **Project Management**: Compile, recreate, run, and manage Delphi projects
* **Group Project Support**: Load and manage Delphi group projects (.groupproj)
* **File System Integration**: Show projects in Explorer, open in File Explorer
* **Configuration Management**: Create and configure .ini files for executables

## Commands

* `Delphi Utils: Swap .DFM/.PAS` - Switch between form and unit files (Alt+F12)
* `Select Delphi Compiler` - Choose the active compiler configuration
* `Pick Group Project` - Load a Delphi group project
* `Refresh` - Refresh the projects view

## Extension Settings

### Compiler Configurations

* `ddk.compiler.configurations`: Array of Delphi compiler configurations

Each compiler configuration includes:

* `name`: Display name for the Delphi version
* `rsVarsPath`: Path to RSVars.bat file
* `msBuildPath`: Path to MSBuild.exe
* `buildArguments`: Default build arguments
* `usePrettyFormat`: Use formatted build output (default: true)

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
      ],
      "usePrettyFormat": true
    }
  ]
}
```

## Known Issues

None so far.

## Release Notes

Users appreciate release notes as you update your extension.

### 1.0.0

Initial release
