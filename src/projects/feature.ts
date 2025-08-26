import { window } from "vscode";
import { Feature } from "../types";
import { Compiler } from "./compiler/compiler";
import { CompilerPicker } from "./compiler/statusBar";
import { DelphiProjectsTreeView } from "./treeItems/treeView";
import { Projects } from "../constants";
import { Runtime } from "../runtime";
import { ProjectsCommands } from "./commands";

export class ProjectsFeature implements Feature {
    public treeView: DelphiProjectsTreeView;
    public compiler: Compiler;
    public compilerStatusBarItem: CompilerPicker;

    public async initialize(): Promise<void> {
        this.treeView = new DelphiProjectsTreeView();
        this.compiler = new Compiler();
        this.compilerStatusBarItem = new CompilerPicker();
        Runtime.extension.subscriptions.push(
            window.createTreeView(Projects.View.Main, {
                treeDataProvider: this.treeView,
                dragAndDropController: this.treeView.dragAndDropController
            })
        );
        ProjectsCommands.register();
    }
}