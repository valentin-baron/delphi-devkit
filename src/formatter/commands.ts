import { commands, Disposable, workspace } from "vscode";
import { FORMAT } from "../constants";
import { accessSync, mkdirSync, readFileSync, writeFileSync } from "fs";
import { join } from "path";
import { Runtime } from "../runtime";

export class FormatterCommands {
    public static get registers(): Disposable[] {
        return [
            commands.registerCommand(FORMAT.COMMAND.EDIT_FORMATTER_CONFIG, this.editFormatterConfig.bind(this))
        ];
    }

    private static async editFormatterConfig(): Promise<void> {
        workspace.openTextDocument(this.formatterPath);
    }

    private static get formatterPath(): string {
        const path = Runtime.extension.globalStorageUri.fsPath;
        try {
            accessSync(path);
        } catch {
            mkdirSync(path, { recursive: true });
        }
        const configPath = join(path, 'ddk_formatter.config');
        try {
            accessSync(configPath);
        } catch {
            writeFileSync(
                configPath,
                readFileSync(Runtime.extension.asAbsolutePath('ddk_formatter.config'))
            );
        }
        return join(path, 'ddk_formatter.config');
    }
}