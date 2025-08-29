import { CancellationToken, FileDecoration, FileDecorationProvider, ProviderResult, ThemeColor, Uri } from "vscode";
import { PROJECTS } from "../../constants";
import { fileExists } from "../../utils";

export class TreeItemDecorator implements FileDecorationProvider {
  public provideFileDecoration(uri: Uri, token: CancellationToken): ProviderResult<FileDecoration> {
    let decoration: FileDecoration;
    switch (uri.scheme) {
      case PROJECTS.SCHEME.SELECTED:
        decoration = new FileDecoration(
          "←S",
          "selected project for compiling shortcuts",
          new ThemeColor('list.focusHighlightForeground')
        );
        decoration.propagate = false;
        return decoration;
      case "file":
        if (!fileExists(uri)) {
          decoration = new FileDecoration(
            "!",
            "file does not exist",
            new ThemeColor('errorForeground')
          );
          decoration.propagate = false;
          return decoration;
        }
    }
  }
}