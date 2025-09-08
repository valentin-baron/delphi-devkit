import { languages, workspace } from "vscode";
import { Runtime } from "../runtime";
import { Feature } from "../types";
import { FullFileFormattingProvider, RangeFormattingProvider } from "./formatter";
import { FORMAT } from "../constants";
import { FormatterCommands } from "./commands";

export class Formatter implements Feature {
    private readonly fullFileProvider: FullFileFormattingProvider = new FullFileFormattingProvider();
    private readonly rangeProvider: RangeFormattingProvider = new RangeFormattingProvider();


    public async initialize(): Promise<void> {
        Runtime.extension.subscriptions.push(
            languages.registerDocumentFormattingEditProvider(
                { pattern: '**​/*.{pas,dpk,dpr,inc}', scheme: 'file' },
                this.fullFileProvider
            ),
            languages.registerDocumentRangeFormattingEditProvider(
                { pattern: '**​/*.{pas,dpk,dpr,inc}', scheme: 'file' },
                this.rangeProvider
            ),
            workspace.onWillSaveTextDocument(async (e) => {
                // Optional auto-run even if user disabled formatOnSave: guard by custom setting if desired.
                const auto = workspace.getConfiguration(FORMAT.KEY).get<boolean>(FORMAT.CONFIG.ON_SAVE);
                if (!auto) return;
                if (!e.document.fileName.match(/\.(pas|dpk|dpr|inc)$/i)) return;
                const edits = await this.fullFileProvider.provideDocumentFormattingEdits(e.document);
                if (edits.length)
                    e.waitUntil(Promise.resolve(edits));
            }),
            ...FormatterCommands.registers
        );
    }
}