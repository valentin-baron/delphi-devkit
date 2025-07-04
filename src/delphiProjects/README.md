# Delphi Projects Explorer

This folder contains the implementation of the Delphi Projects explorer for VS Code. The explorer provides a unified project-based view for Delphi development files.

## Architecture

### Main Components

- **DelphiProjectTreeItem**: Base class for all tree items in the explorer
- **DelphiProject**: Represents a unified project node containing related files
- **DelphiProjectsProvider**: Tree data provider that manages the project hierarchy
- **DelphiProjectUtils**: Utility functions for project file operations
- **DelphiProjectContextMenuCommands**: Context menu commands for all project items

### File Types

- **DprFile**: Delphi program source files (.dpr)
- **DprojFile**: Delphi project files (.dproj)
- **DpkFile**: Delphi package files (.dpk)
- **ExecutableFile**: Compiled executable files (.exe)
- **IniFile**: Configuration files (.ini)

### Project Structure

Projects are grouped as unified nodes with the following hierarchy:

```text
Project Name (with appropriate icon based on type)
├── ProjectName.dproj (if exists)
├── ProjectName.dpr (if exists)
├── ProjectName.dpk (if exists, for packages)
├── ProjectName.exe (if exists)
│   └── ProjectName.ini (if exists, nested under executable)
```

### Context Menu Actions

All context menu actions work across any item type by resolving to the appropriate project files:

- **Compile/Recreate**: Build the project using the DPROJ file
- **Run**: Execute the compiled application
- **Show in Explorer**: Reveal file in VS Code explorer
- **Open in File Explorer**: Open containing folder in system explorer
- **Configure/Create .ini**: Create or edit INI configuration file

### Configuration

The explorer supports configuration via `delphi-utils.delphiProjects.excludePatterns` for excluding files/folders from the project search.

#### Exclude Patterns

You can use standard glob patterns to exclude directories:

- `**/3rd_Party/**` - Excludes all files in any 3rd_Party directory
- `**/__history/**` - Excludes version history directories

#### Negative Globbing

You can use negative patterns (prefixed with `!`) to include specific subdirectories back:

- `**/3rd_Party/**` + `!**/3rd_Party/be/**` - Excludes 3rd_Party but keeps the 'be' subdirectory
- `**/temp/**` + `!**/temp/important/**` - Excludes temp directories but keeps important files

Example configuration:

```json
{
  "delphi-utils.delphiProjects.excludePatterns": [
    "**/__history/**",
    "**/3rd_Party/**",
    "!**/3rd_Party/be/**",
    "**/backup/**"
  ]
}
```

### Future Enhancements

The cache structure includes versioning to support future .groupproj (solution) file support.
