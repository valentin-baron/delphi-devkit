import {
  TreeDragAndDropController,
  DataTransfer,
  DataTransferItem,
  window
} from "vscode";
import { DelphiProjectTreeItem } from "./delphiProjectTreeItem";
import { Runtime } from "../../runtime";
import { LexoSorter } from "../../utils/lexoSorter";
import { DelphiProject } from "./delphiProject";
import { Projects } from "../../constants";

export class DelphiProjectsDragAndDropController
  implements TreeDragAndDropController<DelphiProjectTreeItem>
{
  readonly dragMimeTypes = ["application/vnd.code.tree.projects"];
  readonly dropMimeTypes = ["application/vnd.code.tree.projects"];
  public groupCustomOrder: string[] | undefined;

  async handleDrag(
    source: DelphiProjectTreeItem[],
    dataTransfer: DataTransfer
  ): Promise<void> {
    if (Runtime.extension.workspaceState.get(Projects.Variables.IsGroupProjectView)) {
      window.showErrorMessage("Drag and drop is not supported in group project view.");
      return; 
    }
    dataTransfer.set(
      "application/vnd.code.tree.projects",
      new DataTransferItem(source.map((item) => item.projectUri.fsPath))
    );
  }

  async handleDrop(
    target: DelphiProjectTreeItem | undefined,
    dataTransfer: DataTransfer
  ): Promise<void> {
    const raw = dataTransfer.get("application/vnd.code.tree.projects");
    if (!raw) { return; }
    const draggedKeys: string[] = raw.value;
    if (!Array.isArray(draggedKeys) || !draggedKeys.length) {
      return;
    }
    let treeItems = (await Runtime.projectsProvider.getChildren()).filter((item): item is DelphiProject => item instanceof DelphiProject);
    let sorter = new LexoSorter<DelphiProject>(treeItems);
    let dragItem = treeItems.find((item) => item.projectUri.fsPath === draggedKeys[0]);
    if (!dragItem) { return; }
    
    // Determine the target item for insertion
    let beforeItem: DelphiProject | null = null;
    if (target instanceof DelphiProject) {
      beforeItem = target;
    } else if (target?.project instanceof DelphiProject) {
      beforeItem = target.project;
    }
    
    const newOrder = sorter.reorder(dragItem, beforeItem);
    await Runtime.projectsProvider.save(newOrder);
    Runtime.projectsProvider.refreshTreeView();
  }
}
