pub mod projects;
pub mod lexorank;
pub mod lsp_types;
pub mod files;
pub mod utils;

use anyhow::Result;
use serde_json::Value;
use tokio::io::{stdin, stdout};
use tower_lsp::{Client, async_trait, jsonrpc};
use tower_lsp::lsp_types::request::*;
use tower_lsp::lsp_types::*;
use tower_lsp::{LanguageServer, LspService, Server};

pub(crate) use lsp_types::*;
use projects::*;

struct DelphiLsp {
    client: Client,
}

impl DelphiLsp {
    pub fn new(client: Client) -> Self {
        return DelphiLsp { client }
    }
}

#[async_trait]
impl LanguageServer for DelphiLsp {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        if let Some(_init_options) = params.initialization_options {
            return Ok(InitializeResult {
                capabilities: ServerCapabilities::default(), // none
                server_info: Some(ServerInfo {
                    name: "DDK - Delphi Server".to_string(),
                    version: Some("0.1.0".to_string()),
                }),
            });
        }

        return Ok(InitializeResult::default());
    }

    async fn initialized(&self, _params: InitializedParams) {
        self.client.log_message(MessageType::INFO, "Delphi LSP Relay server initialized").await;
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        return Ok(())
    }

    async fn did_open(&self, _params: DidOpenTextDocumentParams) {
    }

    async fn did_change(&self, _params: DidChangeTextDocumentParams) {
    }

    async fn did_save(&self, _params: DidSaveTextDocumentParams) {
    }

    async fn did_close(&self, _params: DidCloseTextDocumentParams) {
    }

    async fn hover(&self, _params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        return Ok(None);
    }

    async fn completion(
        &self,
        _params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        return Ok(None);
    }

    async fn completion_resolve(&self, params: CompletionItem) -> jsonrpc::Result<CompletionItem> {
        return Ok(params);
    }

    async fn goto_definition(
        &self,
        _params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        return Ok(None);
    }

    async fn goto_declaration(
        &self,
        _params: GotoDeclarationParams,
    ) -> jsonrpc::Result<Option<GotoDeclarationResponse>> {
        return Ok(None);
    }

    async fn goto_type_definition(
        &self,
        _params: GotoTypeDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoTypeDefinitionResponse>> {
        return Ok(None);
    }

    async fn goto_implementation(
        &self,
        _params: GotoImplementationParams,
    ) -> jsonrpc::Result<Option<GotoImplementationResponse>> {
        return Ok(None);
    }

    async fn references(&self, _params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        return Ok(None);
    }

    async fn document_highlight(
        &self,
        _params: DocumentHighlightParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentHighlight>>> {
        return Ok(None);
    }

    async fn document_symbol(
        &self,
        _params: DocumentSymbolParams,
    ) -> jsonrpc::Result<Option<DocumentSymbolResponse>> {
        return Ok(None);
    }

    async fn code_action(
        &self,
        _params: CodeActionParams,
    ) -> jsonrpc::Result<Option<CodeActionResponse>> {
        return Ok(None);
    }

    async fn code_lens(&self, _params: CodeLensParams) -> jsonrpc::Result<Option<Vec<CodeLens>>> {
        return Ok(None);
    }

    async fn code_lens_resolve(&self, params: CodeLens) -> jsonrpc::Result<CodeLens> {
        return Ok(params);
    }

    async fn document_link(
        &self,
        _params: DocumentLinkParams,
    ) -> jsonrpc::Result<Option<Vec<DocumentLink>>> {
        return Ok(None)
    }

    async fn document_link_resolve(&self, params: DocumentLink) -> jsonrpc::Result<DocumentLink> {
        return Ok(params);
    }

    async fn document_color(
        &self,
        _params: DocumentColorParams,
    ) -> jsonrpc::Result<Vec<ColorInformation>> {
        return Ok(vec![]);
    }

    async fn color_presentation(
        &self,
        _params: ColorPresentationParams,
    ) -> jsonrpc::Result<Vec<ColorPresentation>> {
        return Ok(vec![]);
    }

    async fn formatting(
        &self,
        _params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        return Ok(None);
    }

    async fn range_formatting(
        &self,
        _params: DocumentRangeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        return Ok(None);
    }

    async fn on_type_formatting(
        &self,
        _params: DocumentOnTypeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        return Ok(None);
    }

    async fn rename(&self, _params: RenameParams) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        return Ok(None);
    }

    async fn prepare_rename(
        &self,
        _params: TextDocumentPositionParams,
    ) -> jsonrpc::Result<Option<PrepareRenameResponse>> {
        return Ok(None);
    }

    async fn folding_range(
        &self,
        _params: FoldingRangeParams,
    ) -> jsonrpc::Result<Option<Vec<FoldingRange>>> {
        return Ok(None);
    }

    async fn selection_range(
        &self,
        _params: SelectionRangeParams,
    ) -> jsonrpc::Result<Option<Vec<SelectionRange>>> {
        return Ok(None);
    }

    async fn signature_help(
        &self,
        _params: SignatureHelpParams,
    ) -> jsonrpc::Result<Option<SignatureHelp>> {
        return Ok(None);
    }

    async fn semantic_tokens_full(
        &self,
        _params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        return Ok(None);
    }

    async fn semantic_tokens_range(
        &self,
        _params: SemanticTokensRangeParams,
    ) -> jsonrpc::Result<Option<SemanticTokensRangeResult>> {
        return Ok(None);
    }

    async fn inlay_hint(&self, _params: InlayHintParams) -> jsonrpc::Result<Option<Vec<InlayHint>>> {
        return Ok(None);
    }

    async fn inline_value(
        &self,
        _params: InlineValueParams,
    ) -> jsonrpc::Result<Option<Vec<InlineValue>>> {
        return Ok(None);
    }

    async fn moniker(&self, _params: MonikerParams) -> jsonrpc::Result<Option<Vec<Moniker>>> {
        return Ok(None);
    }

    async fn prepare_call_hierarchy(
        &self,
        _params: CallHierarchyPrepareParams,
    ) -> jsonrpc::Result<Option<Vec<CallHierarchyItem>>> {
        return Ok(None);
    }

    async fn incoming_calls(
        &self,
        _params: CallHierarchyIncomingCallsParams,
    ) -> jsonrpc::Result<Option<Vec<CallHierarchyIncomingCall>>> {
        return Ok(None);
    }

    async fn outgoing_calls(
        &self,
        _params: CallHierarchyOutgoingCallsParams,
    ) -> jsonrpc::Result<Option<Vec<CallHierarchyOutgoingCall>>> {
        return Ok(None);
    }

    async fn prepare_type_hierarchy(
        &self,
        _params: TypeHierarchyPrepareParams,
    ) -> jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        return Ok(None);
    }

    async fn supertypes(
        &self,
        _params: TypeHierarchySupertypesParams,
    ) -> jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        return Ok(None);
    }

    async fn subtypes(
        &self,
        _params: TypeHierarchySubtypesParams,
    ) -> jsonrpc::Result<Option<Vec<TypeHierarchyItem>>> {
        return Ok(None);
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let client = self.client.clone();
        let settings = params.settings.clone();
        if let Err(error) = projects::update(settings.clone(), client).await {
            NotifyError::notify_json(&self.client, format!("Failed to apply configuration changes: {}", error), &settings).await;
        }
    }

    async fn did_change_workspace_folders(&self, _params: DidChangeWorkspaceFoldersParams) {

    }

    async fn did_change_watched_files(&self, _params: DidChangeWatchedFilesParams) {

    }

    async fn execute_command(
        &self,
        _params: ExecuteCommandParams,
    ) -> jsonrpc::Result<Option<Value>> {
        return Ok(None);
    }

    async fn will_rename_files(
        &self,
        _params: RenameFilesParams,
    ) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        return Ok(None);
    }

    async fn did_rename_files(&self, _params: RenameFilesParams) {
    }

    async fn did_create_files(&self, _params: CreateFilesParams) {
    }

    async fn did_delete_files(&self, _params: DeleteFilesParams) {
    }

    async fn symbol(
        &self,
        _params: WorkspaceSymbolParams,
    ) -> jsonrpc::Result<Option<Vec<SymbolInformation>>> {
        return Ok(None);
    }

    async fn inlay_hint_resolve(&self, params: InlayHint) -> jsonrpc::Result<InlayHint> {
        return Ok(params);
    }

    async fn will_create_files(
        &self,
        _params: CreateFilesParams,
    ) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        return Ok(None);
    }

    async fn will_delete_files(
        &self,
        _params: DeleteFilesParams,
    ) -> jsonrpc::Result<Option<WorkspaceEdit>> {
        return Ok(None);
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (service, socket) =
        LspService::build(|client| {
            let watcher_client = client.clone();
            tokio::spawn(async move {
                if let Err(e) = start_file_watchers(watcher_client) {
                    eprintln!("File watcher error: {}", e);
                }
            });
            DelphiLsp::new(client)
    }).finish();

    Server::new(stdin(), stdout(), socket).serve(service).await;

    return Ok(())
}