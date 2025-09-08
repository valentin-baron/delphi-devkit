import { Feature, ProjectLinkType } from '../types';
import { Compiler } from './compiler/compiler';
import { CompilerPicker } from './compiler/statusBar';
import { GroupProjectTreeView, WorkspacesTreeView } from './trees/treeView';
import { ProjectsCommands } from './commands';
import { GroupProjectPicker } from './pickers/groupProjPicker';
import { ProjectItem } from './trees/items/project';
import { Entities } from '../db/entities';

export class ProjectsFeature implements Feature {
  public workspacesTreeView: WorkspacesTreeView = new WorkspacesTreeView();
  public groupProjectTreeView: GroupProjectTreeView = new GroupProjectTreeView();
  public compiler: Compiler = new Compiler();
  public compilerStatusBarItem: CompilerPicker = new CompilerPicker();
  public groupProjectPicker: GroupProjectPicker = new GroupProjectPicker();

  public async initialize(): Promise<void> {
    ProjectsCommands.register();
  }

  public getDelphiProjectByLinkId(id: number, linkType: ProjectLinkType): ProjectItem | undefined {
    switch (linkType) {
      case ProjectLinkType.Workspace:
        return this.workspacesTreeView.projects.find((proj) => proj.link.id === id);
      case ProjectLinkType.GroupProject:
        return this.groupProjectTreeView.projects.find((proj) => proj.link.id === id);
    }
  }

  public isCurrentlyCompiling(project: Entities.Project): boolean {
    return this.compiler.currentlyCompilingProjectId === project.id;
  }
}
