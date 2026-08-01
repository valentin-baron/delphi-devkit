mod documents;
mod positions;

use std::sync::Arc;

use anyhow::Result;
use tokio::io::{stdin, stdout};
use tokio::sync::Mutex;
use tower_lsp::{Client, async_trait, jsonrpc};
use tower_lsp::{LanguageServer, LspService, Server};
use tower_lsp::lsp_types::*;

use documents::DocumentStore;

use ddk_core::lsp_types::*;
use ddk_core::projects::*;
use ddk_core::state::*;
use ddk_core::format::Formatter;
use ddk_core::files::dproj as dproj_cache;
use ddk_core::try_finish_event;

#[derive(Clone)]
struct DelphiLsp {
    client: Client,
    /// Open-document store (editor buffers). Behind an async mutex: every
    /// access is a short critical section (insert/replace/read text) that never
    /// spans a blocking parse — the parse runs on a `spawn_blocking` task AFTER
    /// the text has been copied out, so the lock is never held across `.await`.
    documents: Arc<Mutex<DocumentStore>>,
}

impl DelphiLsp {
    pub fn new(client: Client) -> Self {
        DelphiLsp {
            client,
            documents: Arc::new(Mutex::new(DocumentStore::new())),
        }
    }

    /// Analyze an open document and publish its diagnostics. The parse/session
    /// wiring is added in a later step; for now this is the single hook the
    /// lifecycle handlers call so publishing is centralized.
    async fn analyze(&self, _uri: Url, _version: i32) {
        // Session-backed parse + publishDiagnostics wired in a subsequent step.
    }

    async fn projects_compile(
        &self,
        params: CompileProjectParams,
    ) -> tower_lsp::jsonrpc::Result<()> {
        if let Err(e) = Compiler::new(self.client.clone(), &params).await.compile().await {
            lsp_error!(self.client, "Failed to compile project: {}", e);
            NotifyError::notify(&self.client, format!("Failed to compile project: {}", e), None).await;
        }
        try_finish_event!(self.client, params);
    }

    async fn projects_compile_cancel(
        &self,
        _params: CancelCompilationParams,
    ) -> tower_lsp::jsonrpc::Result<()> {
        ddk_core::projects::compiler_state::cancel();
        try_finish_event!(self.client, "compilation cancelled");
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

    async fn settings_encoding(
        &self,
        params: serde_json::Value,
    ) -> tower_lsp::jsonrpc::Result<()> {
        if let Some(enc) = params.get("encoding").and_then(|v| v.as_str()) {
            ddk_core::encoding::set_encoding(enc);
            lsp_info!(self.client, "Compiler encoding changed to: {}", enc);
        }
        Ok(())
    }

    async fn custom_document_format(
        &self,
        params: CustomDocumentFormat,
    ) -> tower_lsp::jsonrpc::Result<DocumentFormatEdit> {
        // The formatter always runs over the whole document — it needs the full
        // context to indent and lay out correctly. For a range request we then
        // map the selection onto the formatted text (see `format::range`).
        let original = params.content.clone();
        let formatter = Formatter::new(params.content)
            .map_err(|error| {
                lsp_error!(self.client, "Failed to initialize formatter: {}", error);
                jsonrpc::Error::invalid_params(format!(
                    "Failed to initialize formatter: {}",
                    error
                ))
            })?;
        let formatted = formatter.execute().await.map_err(|error| {
            lsp_error!(self.client, "Failed to format document: {}", error);
            jsonrpc::Error::invalid_params(format!(
                "Failed to format document: {}",
                error
            ))
        })?;

        // A range request maps the selection onto the formatted text; a
        // whole-document request replaces everything.
        if let Some(range) = params.range {
            let edit =
                ddk_core::format::range::map_range(&original, &formatted, range.start, range.end);
            return Ok(DocumentFormatEdit {
                start: edit.start,
                end: edit.end,
                new_text: edit.new_text,
            });
        }

        Ok(DocumentFormatEdit {
            start: 0,
            end: original.encode_utf16().count(),
            new_text: formatted,
        })
    }

    async fn dproj_metadata(
        &self,
        params: DprojMetadataParams,
    ) -> tower_lsp::jsonrpc::Result<DprojMetadataResponse> {
        let projects_data = PROJECTS_DATA.read().await;
        let project = projects_data
            .get_project(params.project_id)
            .ok_or_else(|| {
                jsonrpc::Error::invalid_params(format!(
                    "Project with id {} not found",
                    params.project_id
                ))
            })?;
        // A bare `.dpr`/`.dpk` has no `.dproj` to enumerate configurations or
        // platforms from. Such projects are compiled directly with dcc32/dcc64,
        // so DevKit offers a synthetic set the command-line compiler supports.
        // The user can still pick a platform; the choice is stored as the
        // project's `active_platform` override and honoured at compile time.
        let Some(dproj_path) = project.dproj.as_ref() else {
            return Ok(DprojMetadataResponse {
                configurations: ddk_core::projects::BARE_CONFIGURATIONS
                    .iter().map(|s| s.to_string()).collect(),
                platforms: ddk_core::projects::BARE_PLATFORMS
                    .iter().map(|s| s.to_string()).collect(),
                active_configuration: project.active_configuration.clone()
                    .unwrap_or_else(|| ddk_core::projects::BARE_DEFAULT_CONFIGURATION.to_string()),
                active_platform: project.active_platform.clone()
                    .unwrap_or_else(|| ddk_core::projects::BARE_DEFAULT_PLATFORM.to_string()),
            });
        };
        let path = std::path::PathBuf::from(dproj_path);
        let dproj_obj = dproj_cache::get_or_load(project.id, &path).map_err(|e| {
            jsonrpc::Error::invalid_params(format!("Failed to load .dproj: {}", e))
        })?;
        let configurations: Vec<String> = dproj_obj.configurations().iter().map(|s| s.to_string()).collect();
        let platforms: Vec<String> = dproj_obj.platforms().iter().map(|(s, _)| s.to_string()).collect();
        let (active_configuration, active_platform) =
            project.effective_config_platform(&dproj_obj);
        Ok(DprojMetadataResponse {
            configurations,
            platforms,
            active_configuration,
            active_platform,
        })
    }
}

#[macro_export]
macro_rules! lsp_debug {
    ($client:expr, $($arg:tt)*) => {
        let inner = $client.clone();
        let inner_message = format!($($arg)*);
        tokio::spawn(async move {
            inner.log_message(tower_lsp::lsp_types::MessageType::LOG, inner_message).await;
        });
    };
}

#[macro_export]
macro_rules! lsp_info {
    ($client:expr, $($arg:tt)*) => {
        let inner = $client.clone();
        let inner_message = format!($($arg)*);
        tokio::spawn(async move {
            inner.log_message(tower_lsp::lsp_types::MessageType::INFO, inner_message).await;
        });
    };
}

#[macro_export]
macro_rules! lsp_error {
    ($client:expr, $($arg:tt)*) => {
        let inner = $client.clone();
        let inner_message = format!($($arg)*);
        tokio::spawn(async move {
            inner.log_message(tower_lsp::lsp_types::MessageType::ERROR, inner_message).await;
        });
    };
}

#[async_trait]
impl LanguageServer for DelphiLsp {
    async fn initialize(&self, params: InitializeParams) -> jsonrpc::Result<InitializeResult> {
        if let Some(opts) = params.initialization_options {
            if let Some(enc) = opts.get("encoding").and_then(|v| v.as_str()) {
                ddk_core::encoding::set_encoding(enc);
            }
        }
        // Advertise ONLY what this task backs: incremental text document sync
        // (so the editor streams open/change/close of buffers) plus pushed
        // diagnostics (publishDiagnostics needs no capability flag). Every
        // feature provider — definition, completion, references, hover, rename,
        // signatureHelp, semanticTokens — stays OFF; those are later tasks and
        // must not be claimed before they are implemented.
        return Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                ..ServerCapabilities::default()
            },
            server_info: Some(ServerInfo {
                name: "DDK - Delphi Server".to_string(),
                version: Some("0.1.0".to_string()),
            }),
        });
    }

    async fn initialized(&self, _params: InitializedParams) {
        lsp_info!(self.client, "Delphi LSP server initialized");
    }

    async fn shutdown(&self) -> jsonrpc::Result<()> {
        ddk_core::projects::compiler_state::cancel();
        return Ok(())
    }

    async fn did_change_configuration(&self, params: DidChangeConfigurationParams) {
        let client = self.client.clone();
        let settings = params.settings.clone();
        if let Err(error) = update(settings.clone(), client).await {
            lsp_error!(self.client, "Failed to apply configuration changes: {}", error);
            NotifyError::notify_json(&self.client, format!("Failed to apply configuration changes: {}", error), &settings).await;
        }
        try_finish_event!(self.client, settings, ());
    }

    // ─── Text document lifecycle ────────────────────────────────────────
    //
    // The store is the authoritative text for open buffers (unsaved edits live
    // only here). Each handler takes the store lock for a short critical
    // section, updates the buffer, and drops the lock. Analysis (parse →
    // diagnostics) is wired on top in a later step; the lock is never held
    // across the (blocking) parse.

    async fn did_open(&self, params: DidOpenTextDocumentParams) {
        let document = params.text_document;
        {
            let mut store = self.documents.lock().await;
            store.open(document.uri.clone(), document.version, document.text.clone());
        }
        self.analyze(document.uri, document.version).await;
    }

    async fn did_change(&self, params: DidChangeTextDocumentParams) {
        let uri = params.text_document.uri;
        let version = params.text_document.version;
        let updated = {
            let mut store = self.documents.lock().await;
            store.apply_change(&uri, version, params.content_changes)
        };
        // Only re-analyze when the change actually updated the buffer (an
        // unopened document or a stale-version change yields `None`).
        if updated.is_some() {
            self.analyze(uri, version).await;
        }
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        {
            let mut store = self.documents.lock().await;
            store.close(&uri);
        }
        // Clear diagnostics for the closed document: the editor keeps showing
        // the last published set until we send an empty one.
        self.client
            .publish_diagnostics(uri, Vec::new(), None)
            .await;
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
    })
        .custom_method("projects/compile", DelphiLsp::projects_compile)
        .custom_method("configuration/fetch", DelphiLsp::configuration_fetch)
        .custom_method("projects/compile-cancel", DelphiLsp::projects_compile_cancel)
        .custom_method("custom/document/format", DelphiLsp::custom_document_format)
        .custom_method("notifications/settings/encoding", DelphiLsp::settings_encoding)
        .custom_method("dproj/metadata", DelphiLsp::dproj_metadata)
        .finish();

    Server::new(stdin(), stdout(), socket).serve(service).await;

    return Ok(())
}
