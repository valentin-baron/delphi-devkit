import { languages } from "vscode";
import { Feature } from "../types";
import { DfmLanguageProvider } from "./language";
import { Runtime } from "../runtime";
import { DfmCommands } from "./commands";


export class DfmFeature implements Feature {
  public async initialize(): Promise<void> {
    Runtime.extension.subscriptions.push(
      ...DfmCommands.registers,
      languages.registerDefinitionProvider(
        { language: 'ddk.dfm', scheme: 'file' },
        new DfmLanguageProvider()
      ),
    );
  }
}