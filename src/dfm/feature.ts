import { commands, languages } from "vscode";
import { Feature } from "../types";
import { DfmLanguageProvider } from "../dfmLanguageSupport/provider";
import { Runtime } from "../runtime";
import { dfmSwap } from "../dfmSwap/command";
import { DFM } from "../constants";


export class DfmFeature implements Feature {
    public async initialize(): Promise<void> {
        const swapCommand = commands.registerCommand(DFM.Commands.SwapToDfmPas, dfmSwap);
        const definitionProvider = languages.registerDefinitionProvider(
            { language: 'delphi-devkit.dfm', scheme: 'file' }, new DfmLanguageProvider());

        Runtime.extension.subscriptions.push(
            swapCommand,
            definitionProvider,
        );
    }
}