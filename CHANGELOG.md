# Change Log

All notable changes to the "delphi-devkit" extension will be documented in this file.

Check [Keep a Changelog](http://keepachangelog.com/) for recommendations on how to structure this file.

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

# Feature Roadmap

- [x] Fixing Diagnostics to show correct types of issues.
- [x] Linking the compiler output to files.
- [ ] Add timestamps to compiler output lines.
- [ ] Delphi Formatter
- [ ] selected Project doesnt work when the tree is collapsed.
- [ ] Support for compiling / recreating all projects in a workspace / group project.
- [ ] Support for commandline execution of unit tests (DUnit).
- [ ] Integrate Delphi Language Server with background compiler. For now, you can use the [OmniPascal extension](https://marketplace.visualstudio.com/items?itemName=Wosi.omnipascal).