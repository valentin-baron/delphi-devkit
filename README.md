# delphi-utils README

Delphi programming language utilities

## Features

* .pas <-> .dfm swapping
* .dfm -> .pas jumps with ctrl+click
* Delphi Projects explorer with project tree view
* Compiler support with multiple configurations

## Extension Settings

This extension contributes the following settings:

### Delphi Projects

* `delphi-utils.delphiProjects.projectPaths`: Array of glob patterns specifying where to search for Delphi projects
  * Default: `["**"]` (searches everywhere)
  * Example: `["src/**", "projects/**"]` (searches only in src and projects directories)

* `delphi-utils.delphiProjects.excludePatterns`: Array of glob patterns for paths to exclude from search
  * Default: `["**/__history/**", "**/.history/**"]`
  * Example: `["**/temp/**", "**/backup/**", "**/bin/**"]`

### Compiler Configurations

* `delphi-utils.compiler.configurations`: Array of Delphi compiler configurations
* `delphi-utils.compiler.currentConfiguration`: Currently selected compiler configuration

#### Example Configuration

```json
{
  "delphi-utils.delphiProjects.projectPaths": ["src/**", "projects/**"],
  "delphi-utils.delphiProjects.excludePatterns": ["**/temp/**", "**/__history/**", "**/backup/**"]
}
```

## Known Issues

None so far.

## Release Notes

Users appreciate release notes as you update your extension.

### 1.0.0

Initial release
