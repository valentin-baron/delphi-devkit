import { window } from "vscode";
import { Feature } from "../types";
import { Compiler } from "./compiler/compiler";
import { CompilerPicker } from "./compiler/statusBar";
import { GroupProjectTreeView, WorkspacesTreeView } from "./treeItems/treeView";
import { PROJECTS } from "../constants";
import { Runtime } from "../runtime";
import { ProjectsCommands } from "./commands";
import { GroupProjectPicker } from './pickers/groupProjPicker';

export class ProjectsFeature implements Feature {
    public workspacesTreeView: WorkspacesTreeView = new WorkspacesTreeView();
    public groupProjectsTreeView: GroupProjectTreeView = new GroupProjectTreeView();
    public compiler: Compiler = new Compiler();
    public compilerStatusBarItem: CompilerPicker = new CompilerPicker();
    public groupProjectPicker: GroupProjectPicker = new GroupProjectPicker();

    public async initialize(): Promise<void> {
        Runtime.extension.subscriptions.push(
            window.createTreeView(PROJECTS.VIEW.WORKSPACES, {
                treeDataProvider: this.workspacesTreeView,
                dragAndDropController: undefined //TODO: this.workspacesTreeView.dragAndDropController
            }),
            window.createTreeView(PROJECTS.VIEW.GROUP_PROJECT, {
                treeDataProvider: this.groupProjectsTreeView,
                dragAndDropController: undefined //TODO: this.groupProjectsTreeView.dragAndDropController
            })
        );
        ProjectsCommands.register();
    }
}