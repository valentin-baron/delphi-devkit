pub mod projects;
pub mod lexorank;
pub mod lsp_types;
pub mod files;
pub mod utils;
pub mod format;

use anyhow::Result;
use tokio::io::{stdin, stdout};
use tower_lsp::{Client, async_trait, jsonrpc};
use tower_lsp::{LanguageServer, LspService, Server};
use tower_lsp::lsp_types::*;

pub(crate) use lsp_types::*;
use projects::*;

use crate::format::Formatter;

#[derive(Debug, Clone)]
struct DelphiLsp {
    client: Client,
}

impl DelphiLsp {
    pub fn new(client: Client) -> Self {
        return DelphiLsp { client }
    }

    async fn projects_compile(
        &self,
        params: CompileProjectParams,
    ) -> tower_lsp::jsonrpc::Result<()> {
        if let Err(e) = Compiler::new(self.client.clone(), params).compile().await {
            NotifyError::notify(&self.client, format!("Failed to compile project: {}", e), None).await;
        }
        Ok(())
    }

    async fn projects_compile_cancel(
        &self,
        _params: CancelCompilationParams,
    ) -> tower_lsp::jsonrpc::Result<()> {
        CANCEL_COMPILATION.store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    async fn configuration_fetch(
        &self,
        _params: serde_json::Value,
    ) -> tower_lsp::jsonrpc::Result<ConfigurationFetchResponse> {
        Ok(ConfigurationFetchResponse {
            projects: ProjectsData::new(),
            compilers: CompilerConfigurations::new(),
        })
    }
}

#[async_trait]
impl LanguageServer for DelphiLsp {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        ProjectsUpdate::notify(&self.client).await;
        CompilersUpdate::notify(&self.client).await;
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

    async fn formatting(
        &self,
        params: DocumentFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let url = params.text_document.uri.clone();
        let formatter = Formatter::new(url).map_err(|e| {
            jsonrpc::Error::invalid_params(format!(
                "Failed to initialize formatter for file {}: {}",
                params.text_document.uri,
                e
            ))
        })?;
        let formatted_content = formatter.execute(None).map_err(|e| {
            jsonrpc::Error::invalid_params(format!(
                "Failed to format file {}: {}",
                params.text_document.uri,
                e
            ))
        })?;

        return Ok(Some(vec![TextEdit {
            range: Range {
                start: Position {
                    line: 0,
                    character: 0,
                },
                end: Position {
                    line: u32::MAX,
                    character: u32::MAX,
                },
            },
            new_text: formatted_content,
        }]));
    }

    async fn range_formatting(
        &self,
        params: DocumentRangeFormattingParams,
    ) -> jsonrpc::Result<Option<Vec<TextEdit>>> {
        let url = params.text_document.uri.clone();
        let formatter = Formatter::new(url).map_err(|e| {
            jsonrpc::Error::invalid_params(format!(
                "Failed to initialize formatter for file {}: {}",
                params.text_document.uri,
                e
            ))
        })?;
        let formatted_content = formatter.execute(Some(params.range)).map_err(|e| {
            jsonrpc::Error::invalid_params(format!(
                "Failed to format file {}: {}",
                params.text_document.uri,
                e
            ))
        })?;
        return Ok(Some(vec![TextEdit {
            range: params.range,
            new_text: formatted_content,
        }]));
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let client = self.client.clone();
        let settings = params.settings.clone();
        if let Err(error) = projects::update(settings.clone(), client).await {
            NotifyError::notify_json(&self.client, format!("Failed to apply configuration changes: {}", error), &settings).await;
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let (service, socket) = LspService::build(|client| {
        let watcher_client = client.clone();
        tokio::spawn(async move {
            let _ = ProjectsData::initialize()
                .expect("Failed to initialize projects data");
            let _ = CompilerConfigurations::initialize()
                .expect("Failed to initialize compiler configuration");
            if let Err(e) = start_file_watchers(watcher_client) {
                eprintln!("File watcher error: {}", e);
            }
        });
        DelphiLsp::new(client)
    }).custom_method("projects/compile", DelphiLsp::projects_compile)
    .custom_method("configuration/fetch", DelphiLsp::configuration_fetch)
    .custom_method("projects/compile-cancel", DelphiLsp::projects_compile_cancel)
        .finish();

    Server::new(stdin(), stdout(), socket).serve(service).await;

    return Ok(())
}