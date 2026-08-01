mod call_context;
mod completion;
mod diagnostics;
mod documents;
mod hover;
mod locations;
mod positions;
mod semantic;
mod session;
mod signature;

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Result;
use tokio::io::{stdin, stdout};
use tokio::sync::Mutex;
use tower_lsp::{Client, async_trait, jsonrpc};
use tower_lsp::{LanguageServer, LspService, Server};
use tower_lsp::lsp_types::*;

use documents::DocumentStore;
use session::SessionManager;

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
    /// The parser session for the open project, behind an async lock. Parses run
    /// on `spawn_blocking`; the lock is never held across `.await`. See
    /// [`session`] for the async/lock model.
    session: Arc<SessionManager>,
    /// Per-URL monotonic "last published diagnostics version". tower-lsp runs
    /// notification handlers with bounded concurrency (`buffer_unordered`), so
    /// two `analyze` calls for the same document can finish OUT OF ORDER: a slow
    /// parse for v2 may complete AFTER v3 has already parsed and published. This
    /// guard makes the publish itself monotonic — a publish whose version is
    /// `<=` the last one already published for that URL is dropped, so newer
    /// diagnostics are never overwritten by staler ones (the never-a-wrong-answer
    /// rule: never show squiggles for an older buffer version).
    published_versions: Arc<Mutex<HashMap<Url, i32>>>,
}

/// The monotonic publish-slot decision, extracted so the out-of-order race is
/// unit-testable without a live LSP `Client`. Returns `true` (and records
/// `version` as the new last-published) iff `version` is strictly newer than the
/// last version published for `uri`; returns `false` (leaving the map unchanged)
/// for a stale-or-duplicate version, which the caller drops.
fn claim_publish_slot(published: &mut HashMap<Url, i32>, uri: &Url, version: i32) -> bool {
    match published.get(uri) {
        Some(&last) if version <= last => false,
        _ => {
            published.insert(uri.clone(), version);
            true
        }
    }
}

/// The result of the blocking parse+diagnostics work in [`DelphiLsp::analyze`],
/// carried out of the `spawn_blocking` task so the async layer can log (needs
/// the `Client`) and publish under the version guard.
enum AnalyzeOutcome {
    /// A normal diagnostics set to publish (an empty vec CLEARS the buffer's
    /// squiggles — no session, non-unit source, or clean parse).
    Publish(Vec<Diagnostic>),
    /// A hard, unrecoverable parse failure: publish `diagnostics` (a single
    /// ERROR finding replacing the stale set) and log `message`.
    ParseFailure {
        diagnostics: Vec<Diagnostic>,
        message: String,
    },
}

impl DelphiLsp {
    pub fn new(client: Client) -> Self {
        DelphiLsp {
            client,
            documents: Arc::new(Mutex::new(DocumentStore::new())),
            session: Arc::new(SessionManager::new()),
            published_versions: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Publish `diagnostics` for `(uri, version)` unless a NEWER (or equal) set
    /// has already been published for that URL. Returns whether the publish went
    /// out. This closes the out-of-order publish race: when a slow parse for an
    /// older version finishes after a newer one has already been published, its
    /// stale set is dropped here instead of overwriting the newer squiggles.
    ///
    /// The `published_versions` lock is a short critical section held only to
    /// read-and-update the last-published version; it is dropped before the
    /// `publish_diagnostics` await so it is never held across `.await`.
    async fn publish_if_newer(&self, uri: Url, diagnostics: Vec<Diagnostic>, version: i32) -> bool {
        {
            let mut published = self.published_versions.lock().await;
            if !claim_publish_slot(&mut published, &uri, version) {
                // A publish for this-or-a-newer version already went out → drop
                // this staler/duplicate one.
                return false;
            }
        }
        self.client
            .publish_diagnostics(uri, diagnostics, Some(version))
            .await;
        true
    }

    /// Analyze an open document and publish its diagnostics: parse the buffer
    /// through the project session (on a blocking task) and map the unified
    /// diagnostics to LSP ranges via the buffer's line index.
    ///
    /// Async/lock discipline: the document lock and session lock are each taken
    /// for a short synchronous section; neither is held across `.await`. The
    /// blocking parse runs inside `spawn_blocking`, so the async executor is
    /// never stalled.
    async fn analyze(&self, uri: Url, version: i32) {
        // Only analyze real file paths (skip untitled:/ and other schemes).
        let Some(path) = session::uri_to_path(&uri) else {
            return;
        };

        // Copy the current buffer text out under the store lock (short section),
        // then release the lock before the parse.
        let text = {
            let store = self.documents.lock().await;
            match store.get(&uri) {
                // A stale notification (a newer version already applied) must
                // not publish diagnostics for old text.
                Some(document) if document.version == version => document.text().to_string(),
                _ => return,
            }
        };

        // Ensure the project session is open (blocking open, off-executor).
        //
        // Sequencing note (finding 4): `ensure_open` releases the session lock
        // before the parse below re-acquires it in a separate `spawn_blocking`.
        // A concurrent `analyze` could, in that window, re-open the session for a
        // different identity. This is safe under the current model: there is ONE
        // active project per process (the parser's interner/arena are process
        // globals, so only one session can exist at a time — see `session`), and
        // every concurrent `analyze` resolves the SAME active-project inputs, so
        // any interleaved re-open targets the identical `(dproj, config,
        // platform)` and is a no-op (`ensure_open` returns early on an unchanged
        // identity). Folding open+parse under a single lock acquisition would
        // require threading the parse closure through the SessionManager; the
        // window is documented rather than restructured because it cannot
        // currently swap in a different-project session mid-parse.
        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        // Parse the buffer and collect LSP diagnostics on a blocking thread.
        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let mut guard = session_handle.blocking_lock();
            let Some(project_session) = guard.as_mut() else {
                // No session (unresolvable + fallback failed) → clear
                // diagnostics rather than leaving a stale set.
                return AnalyzeOutcome::Publish(Vec::new());
            };
            match project_session.parse_buffer(&parse_path, index.text()) {
                Ok((_, Some(meta))) => {
                    let unit_key = meta.name();
                    let buffer_file = meta.ast.name.location.file;
                    let unified = project_session.diagnostics(unit_key);
                    AnalyzeOutcome::Publish(diagnostics::to_lsp_diagnostics(
                        &unified,
                        buffer_file,
                        &index,
                    ))
                }
                // A non-unit source (program/library/package) produces no
                // importable meta and thus no unit-keyed diagnostics here; clear.
                Ok((_, None)) => AnalyzeOutcome::Publish(Vec::new()),
                // A hard, unrecoverable parse failure: the buffer no longer
                // parses at all, so the prior granular set is stale. Do NOT keep
                // it and do NOT stay silent — REPLACE it with a single honest
                // ERROR diagnostic ("failed to parse: <reason>") anchored at the
                // error's actual location when it carries one (else top-of-doc).
                // This is the sole producer of a hard-failure ERROR squiggle.
                Err(error) => {
                    let span = error
                        .location
                        .map(|location| (location.span.start as usize, location.span.end as usize));
                    let diagnostic =
                        diagnostics::parse_failure_diagnostic(&error.message, span, &index);
                    AnalyzeOutcome::ParseFailure {
                        diagnostics: vec![diagnostic],
                        message: error.message,
                    }
                }
            }
        })
        .await;

        let lsp_diagnostics = match result {
            Ok(AnalyzeOutcome::Publish(lsp_diagnostics)) => lsp_diagnostics,
            // Hard parse failure: publish the single ERROR diagnostic (replacing
            // the stale set) AND still log — a failing buffer is now both visible
            // to the user (a squiggle) and recorded in the log.
            Ok(AnalyzeOutcome::ParseFailure { diagnostics, message }) => {
                lsp_error!(self.client, "parse of {} failed: {}", uri, message);
                diagnostics
            }
            Err(join_error) => {
                lsp_error!(self.client, "analyze task failed: {}", join_error);
                return;
            }
        };

        // Re-check the stored version BEFORE publishing: while this parse ran, a
        // newer didChange may have replaced the buffer. Publishing this (now
        // older) set would show squiggles for a version the editor no longer
        // holds. Skip if the document advanced or was closed.
        {
            let store = self.documents.lock().await;
            match store.get(&uri) {
                Some(document) if document.version == version => {}
                _ => return,
            }
        }

        // Publish under the monotonic guard: even if the stored-version recheck
        // passed, an out-of-order completion could still try to publish an older
        // version than one already sent, so `publish_if_newer` is the final gate.
        self.publish_if_newer(uri, lsp_diagnostics, version).await;
    }

    /// Resolve go-to-definition for `(uri, position)`.
    ///
    /// Steps (all the parser work on a single `spawn_blocking` task, the session
    /// lock taken with `blocking_lock()` and never held across `.await`):
    /// 1. copy the open buffer's text out under the store lock; build its
    ///    `LineIndex`; map `position` → a byte offset (the buffer's own text);
    /// 2. clone the document store (cheap — a handful of open buffers) so the
    ///    blocking task can consult it for open TARGET files without holding the
    ///    async store lock across the parse;
    /// 3. parse the buffer to obtain its unit key (`meta.name()`), then
    ///    `symbol_at(offset)` → `definition(...)`;
    /// 4. map each resulting `CodeLocation` to an LSP `Location` via Deliverable
    ///    A, using the TARGET file's own text.
    ///
    /// Empty (no symbol under the cursor, or unresolved) → `None`: never a wrong
    /// jump.
    async fn resolve_definition(
        &self,
        uri: Url,
        position: Position,
    ) -> Option<Vec<Location>> {
        let path = session::uri_to_path(&uri)?;

        // The open buffer's authoritative text (unsaved edits live only here).
        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        // Ensure a session is open for the active project (same as `analyze`).
        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let offset = index.offset_of(position) as u32;
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            // Parse the buffer to get its unit key; a non-unit source (program/
            // library/package) yields no importable meta → no definition here.
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            // Identifier under the cursor → its declaration site(s), each mapped
            // to an LSP Location from the TARGET file's own text. Unresolved or
            // unmappable → None (never a fabricated jump).
            locations::resolve_definition_locations(project_session, unit_key, offset)
        })
        .await;

        match result {
            Ok(mapped) => mapped,
            Err(join_error) => {
                lsp_error!(self.client, "definition task failed: {}", join_error);
                None
            }
        }
    }

    /// Resolve hover for `(uri, position)`: the symbol under the cursor → its
    /// declared facts (kind, type, directives, visibility, owner) → markdown.
    ///
    /// Same async/lock discipline as `resolve_definition`: the buffer text is
    /// copied out under the store lock, the parser query runs on `spawn_blocking`
    /// with the session lock via `blocking_lock()` (never across `.await`). The
    /// hover's highlight range is the OCCURRENCE span under the cursor, mapped
    /// through the requesting document's OWN line index (the occurrence is in
    /// this buffer). No honest facts → `None`, never a fabricated signature.
    async fn resolve_hover(&self, uri: Url, position: Position) -> Option<Hover> {
        let path = session::uri_to_path(&uri)?;

        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let offset = index.offset_of(position) as u32;
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            let info = project_session.hover_info(unit_key, offset)?;
            // The occurrence span is in THIS buffer, so its range maps through
            // this document's own line index (already built as `index`).
            let range = Range {
                start: index.position_of(info.occurrence.span.start as usize),
                end: index.position_of(info.occurrence.span.end as usize),
            };
            let markdown = hover::format_hover(&info);
            Some(Hover {
                contents: HoverContents::Markup(MarkupContent {
                    kind: MarkupKind::Markdown,
                    value: markdown,
                }),
                range: Some(range),
            })
        })
        .await;

        match result {
            Ok(hover) => hover,
            Err(join_error) => {
                lsp_error!(self.client, "hover task failed: {}", join_error);
                None
            }
        }
    }

    /// Resolve `textDocument/references` for `(uri, position)`.
    ///
    /// Same async/lock discipline as `resolve_definition`: the buffer text is
    /// copied out under the store lock, the parser queries run on
    /// `spawn_blocking` with the session lock via `blocking_lock()` (never held
    /// across `.await`). The identifier under the cursor → its folded key →
    /// `session.references(key)`, an OVER-APPROXIMATING candidate set (documented
    /// in `locations::resolve_references`): each occurrence's `Range` is computed
    /// from its OWN file's text, honoring `include_declaration`. No target under
    /// the cursor → `None`; a target with no occurrences → an empty `Vec`. This
    /// is READ-ONLY; the over-approximation is why rename is not advertised.
    async fn resolve_references(
        &self,
        uri: Url,
        position: Position,
        include_declaration: bool,
    ) -> Option<Vec<Location>> {
        let path = session::uri_to_path(&uri)?;

        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let offset = index.offset_of(position) as u32;
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            locations::resolve_references(project_session, unit_key, offset, include_declaration)
        })
        .await;

        match result {
            Ok(mapped) => mapped,
            Err(join_error) => {
                lsp_error!(self.client, "references task failed: {}", join_error);
                None
            }
        }
    }

    /// Resolve `textDocument/completion` for `(uri, position)`.
    ///
    /// Same async/lock discipline as `resolve_definition`: the buffer text is
    /// copied out under the store lock, the parser query runs on
    /// `spawn_blocking` with the session lock via `blocking_lock()` (never held
    /// across `.await`). The parser's context-sensitive `completions` query
    /// guarantees the never-a-wrong-answer contract: a member access after `.`
    /// returns ONLY the receiver type's members (an unresolved receiver → an
    /// empty list, never a wrong member set); any other context → the top-level
    /// set (builtins + own + imports). This handler only TRANSLATES each result
    /// to a `CompletionItem`, so the member-only guarantee is preserved.
    ///
    /// Returns `None` when there is no session/meta (the editor shows nothing);
    /// an empty list is a legitimate answer (unresolved member receiver).
    async fn resolve_completion(
        &self,
        uri: Url,
        position: Position,
    ) -> Option<Vec<CompletionItem>> {
        let path = session::uri_to_path(&uri)?;

        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let offset = index.offset_of(position) as u32;
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            Some(completion::resolve_completions(project_session, unit_key, offset))
        })
        .await;

        match result {
            Ok(items) => items,
            Err(join_error) => {
                lsp_error!(self.client, "completion task failed: {}", join_error);
                None
            }
        }
    }

    /// Resolve `textDocument/signatureHelp` for `(uri, position)`.
    ///
    /// Steps (all parser work on one `spawn_blocking` task, session lock via
    /// `blocking_lock()`, never across `.await`):
    /// 1. copy the buffer text; build its `LineIndex`; map `position` → a byte
    ///    offset;
    /// 2. detect the enclosing call context from the RAW TEXT via
    ///    [`call_context::enclosing_call`] — the callee's byte offset (the dotted
    ///    identifier before the unclosed `(`) and the active parameter index
    ///    (top-level commas, skipping strings/comments/nested parens);
    /// 3. parse the buffer for its unit key, resolve the callee via `symbol_at`
    ///    at the callee offset, then the parser `signature_help` query;
    /// 4. build the LSP `SignatureHelp`.
    ///
    /// NEVER fabricates: no enclosing call, no resolvable callee, or a callee
    /// that is not a routine → `None` (the editor shows nothing).
    async fn resolve_signature_help(
        &self,
        uri: Url,
        position: Position,
    ) -> Option<SignatureHelp> {
        let path = session::uri_to_path(&uri)?;

        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let offset = index.offset_of(position) as u32;
            // Call-context detection on the RAW buffer text (byte offsets),
            // before any parser resolution. No enclosing call → no signature.
            let context = call_context::enclosing_call(index.text(), offset as usize)?;
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            signature::resolve_signature_help(
                project_session,
                unit_key,
                context.callee_offset as u32,
                context.active_parameter,
            )
        })
        .await;

        match result {
            Ok(help) => help,
            Err(join_error) => {
                lsp_error!(self.client, "signatureHelp task failed: {}", join_error);
                None
            }
        }
    }

    /// Resolve `textDocument/semanticTokens/full` for `uri`: the whole buffer's
    /// classified tokens, delta-encoded for LSP.
    ///
    /// Same async/lock discipline as `resolve_definition`: the buffer text is
    /// copied out under the store lock, the parser query + encoding run on
    /// `spawn_blocking` with the session lock via `blocking_lock()` (never held
    /// across `.await`). The parser's `semantic_tokens` query already emits a
    /// token ONLY when the classification is certain (an unresolved identifier is
    /// omitted); this handler only ENCODES — split-per-line + UTF-16 + delta — so
    /// the never-a-wrong-color guarantee is preserved. The tokens' spans are into
    /// THIS buffer's source, so they map through this document's own line index.
    ///
    /// Returns `None` when there is no session/meta (the editor shows nothing);
    /// an empty token list is a legitimate answer (e.g. an empty document).
    async fn resolve_semantic_tokens(&self, uri: Url) -> Option<SemanticTokensResult> {
        let path = session::uri_to_path(&uri)?;

        let text = {
            let store = self.documents.lock().await;
            store.get(&uri)?.text().to_string()
        };

        let inputs = session::resolve_active_project_inputs().await;
        self.session
            .ensure_open(
                inputs.dproj,
                inputs.configuration,
                inputs.platform,
                inputs.profile,
                inputs.standard_source_paths,
            )
            .await;

        let session_handle = self.session.handle();
        let parse_path = session::document_path(&path);
        let result = tokio::task::spawn_blocking(move || {
            let index = positions::LineIndex::new(text);
            let mut guard = session_handle.blocking_lock();
            let project_session = guard.as_mut()?;
            let (_, meta) = project_session.parse_buffer(&parse_path, index.text()).ok()?;
            let unit_key = meta?.name();
            let data = semantic::resolve_semantic_tokens(project_session, unit_key, &index);
            Some(SemanticTokensResult::Tokens(SemanticTokens {
                result_id: None,
                data,
            }))
        })
        .await;

        match result {
            Ok(tokens) => tokens,
            Err(join_error) => {
                lsp_error!(self.client, "semanticTokens task failed: {}", join_error);
                None
            }
        }
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
        // Advertise ONLY what this server backs: incremental text document sync
        // (so the editor streams open/change/close of buffers), pushed
        // diagnostics (publishDiagnostics needs no capability flag), the
        // go-to-definition and hover providers wired in Task 9, and the
        // find-references provider wired in Task 10 (read-only, honest candidate
        // set), and the completion + signatureHelp providers wired in Task 11
        // (both honest: completion is member-only after `.`, signatureHelp never
        // fabricates). The remaining feature providers — rename, semanticTokens —
        // stay OFF; a capability is claimed only once backed. rename is
        // DELIBERATELY not advertised (parser SESSION.md ledger #42): the
        // reference set is scope-unresolved, so no provably correct+complete
        // rename exists yet.
        return Ok(InitializeResult {
            capabilities: ServerCapabilities {
                text_document_sync: Some(TextDocumentSyncCapability::Kind(
                    TextDocumentSyncKind::INCREMENTAL,
                )),
                definition_provider: Some(OneOf::Left(true)),
                hover_provider: Some(HoverProviderCapability::Simple(true)),
                // references is READ-ONLY and honest: it returns an
                // OVER-APPROXIMATING candidate set (the scope-unresolved usage
                // index) the user visually reviews. rename stays OFF: renaming
                // that candidate set could rewrite an unrelated same-named
                // identifier (destructive), and renaming only the provable
                // subset would leave impl-section uses dangling — neither is
                // provably correct+complete without scope resolution (parser
                // SESSION.md ledger #42).
                references_provider: Some(OneOf::Left(true)),
                // completion: context-sensitive, backed by the parser's
                // never-wrong `completions` query. Trigger on `.` (member
                // access); the editor also invokes it on Ctrl+Space. No resolve
                // step — every item is fully built up front.
                completion_provider: Some(CompletionOptions {
                    trigger_characters: Some(vec![".".to_string()]),
                    resolve_provider: Some(false),
                    ..CompletionOptions::default()
                }),
                // signatureHelp: backed by the parser's `signature_help` query
                // reading params/return from the AST. Trigger on `(` (call open)
                // and `,` (next argument); retrigger on `,` so the active
                // parameter updates as the user types further arguments. Never a
                // fabricated signature: an unresolved callee → no help.
                signature_help_provider: Some(SignatureHelpOptions {
                    trigger_characters: Some(vec!["(".to_string(), ",".to_string()]),
                    retrigger_characters: Some(vec![",".to_string()]),
                    work_done_progress_options: WorkDoneProgressOptions::default(),
                }),
                // semanticTokens (Full): syntax highlighting backed by the
                // parser's `semantic_tokens` query. ADDITIVE over TextMate — the
                // query emits a token only when the classification is CERTAIN (an
                // unresolved identifier is omitted), so a semantic color is never
                // wrong. The advertised LEGEND is `semantic::legend()`, the SAME
                // ordered arrays the delta encoder indexes into (one source of
                // truth). Range is not advertised (Full only).
                semantic_tokens_provider: Some(
                    SemanticTokensServerCapabilities::SemanticTokensOptions(
                        SemanticTokensOptions {
                            work_done_progress_options: WorkDoneProgressOptions::default(),
                            legend: semantic::legend(),
                            range: Some(false),
                            full: Some(SemanticTokensFullOptions::Bool(true)),
                        },
                    ),
                ),
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

    // ─── Navigation: go-to-definition ──────────────────────────────────
    //
    // Map (Url, Position) → the identifier under the cursor → its declaration
    // site(s), each turned into an LSP Location computed from the TARGET file's
    // own text. An unresolved target (no symbol, or a definition the parser
    // cannot place) yields `None` — the editor performs no jump, never a wrong
    // one. The parser work runs on `spawn_blocking` behind the session lock.
    async fn goto_definition(
        &self,
        params: GotoDefinitionParams,
    ) -> jsonrpc::Result<Option<GotoDefinitionResponse>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self
            .resolve_definition(uri, position)
            .await
            .map(GotoDefinitionResponse::Array))
    }

    // ─── Hover ──────────────────────────────────────────────────────────
    //
    // Resolve the symbol under the cursor to its declared facts (kind, type,
    // directives, visibility, owner) — cross-unit through the same machinery as
    // definition — and format them as markdown. No honest facts → `None`, never
    // a fabricated signature.
    async fn hover(&self, params: HoverParams) -> jsonrpc::Result<Option<Hover>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self.resolve_hover(uri, position).await)
    }

    // ─── Find references ───────────────────────────────────────────────
    //
    // Map (Url, Position) → the identifier under the cursor → every recorded
    // occurrence of its folded key across cached units, each mapped to a
    // Location from its OWN file's text. This is an OVER-APPROXIMATING candidate
    // set (the usage index is scope-unresolved) — acceptable for a read-only
    // "find all references" the user reviews, and documented as such. Honors
    // `context.include_declaration`. No symbol under the cursor → `None`. The
    // parser work runs on `spawn_blocking` behind the session lock.
    async fn references(&self, params: ReferenceParams) -> jsonrpc::Result<Option<Vec<Location>>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        let include_declaration = params.context.include_declaration;
        Ok(self
            .resolve_references(uri, position, include_declaration)
            .await)
    }

    // ─── Completion ────────────────────────────────────────────────────
    //
    // Context-sensitive completion. The parser's `completions` query decides the
    // set (member-only after `.`, else top-level); this handler maps each result
    // to a `CompletionItem`. An unresolved member receiver yields an empty list
    // (never a wrong member set), no session/meta yields `None`.
    async fn completion(
        &self,
        params: CompletionParams,
    ) -> jsonrpc::Result<Option<CompletionResponse>> {
        let uri = params.text_document_position.text_document.uri;
        let position = params.text_document_position.position;
        Ok(self
            .resolve_completion(uri, position)
            .await
            .map(CompletionResponse::Array))
    }

    // ─── Signature help ────────────────────────────────────────────────
    //
    // Detect the enclosing call context from the raw buffer text (skipping
    // strings/comments/nested parens), resolve the callee via the SAME machinery
    // as definition, and read its params/return from the AST. Never fabricates:
    // no call, no resolvable routine → `None`.
    async fn signature_help(
        &self,
        params: SignatureHelpParams,
    ) -> jsonrpc::Result<Option<SignatureHelp>> {
        let uri = params.text_document_position_params.text_document.uri;
        let position = params.text_document_position_params.position;
        Ok(self.resolve_signature_help(uri, position).await)
    }

    // ─── Semantic tokens (syntax highlighting) ─────────────────────────────
    //
    // Whole-buffer classified tokens, delta-encoded. ADDITIVE over the editor's
    // TextMate grammar: the parser query emits a token only when the
    // classification is CERTAIN, so an unresolved identifier is OMITTED (its
    // TextMate color shows) — never a wrong semantic color. Multi-line spans are
    // split per line and lengths are UTF-16, in `semantic::encode`. The parser
    // work + encoding run on `spawn_blocking` behind the session lock.
    async fn semantic_tokens_full(
        &self,
        params: SemanticTokensParams,
    ) -> jsonrpc::Result<Option<SemanticTokensResult>> {
        let uri = params.text_document.uri;
        Ok(self.resolve_semantic_tokens(uri).await)
    }

    async fn did_close(&self, params: DidCloseTextDocumentParams) {
        let uri = params.text_document.uri;
        {
            let mut store = self.documents.lock().await;
            store.close(&uri);
        }
        // Forget the last-published version for this URL: a reopen starts a fresh
        // version sequence (the editor resets to version 1), which must not be
        // rejected by the monotonic publish guard as "older than" the closed
        // document's last version.
        {
            let mut published = self.published_versions.lock().await;
            published.remove(&uri);
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

#[cfg(test)]
mod publish_guard_tests {
    //! The monotonic publish guard (finding 1): with tower-lsp's bounded
    //! concurrency, `analyze` calls finish out of order. The publish must be
    //! monotonic per URL so an older parse that completes late never overwrites a
    //! newer already-published set.

    use super::claim_publish_slot;
    use std::collections::HashMap;
    use tower_lsp::lsp_types::Url;

    fn uri() -> Url {
        Url::parse("file:///c:/proj/Unit1.pas").unwrap()
    }

    /// Simulate the exact race: a fast v3 publish lands first, then a SLOW v2
    /// parse completes and tries to publish. v2 must be dropped (not overwrite
    /// v3), and the recorded last-published version stays at 3.
    #[test]
    fn out_of_order_v2_after_v3_is_dropped() {
        let mut published: HashMap<Url, i32> = HashMap::new();
        // v3 finishes first and publishes.
        assert!(claim_publish_slot(&mut published, &uri(), 3));
        assert_eq!(published.get(&uri()), Some(&3));
        // v2's slow parse completes AFTER v3 — its publish is dropped.
        assert!(
            !claim_publish_slot(&mut published, &uri(), 2),
            "a v2 publish after v3 must be rejected"
        );
        // v3 is still the last published — v2 did NOT overwrite it.
        assert_eq!(published.get(&uri()), Some(&3));
    }

    /// A newer version always wins; an equal version (duplicate notification) is
    /// dropped so the same set is not republished.
    #[test]
    fn newer_wins_equal_and_older_dropped() {
        let mut published: HashMap<Url, i32> = HashMap::new();
        assert!(claim_publish_slot(&mut published, &uri(), 1));
        assert!(claim_publish_slot(&mut published, &uri(), 2)); // newer wins
        assert!(!claim_publish_slot(&mut published, &uri(), 2)); // equal dropped
        assert!(!claim_publish_slot(&mut published, &uri(), 1)); // older dropped
        assert_eq!(published.get(&uri()), Some(&2));
    }

    /// After close (map entry removed), a reopened document's fresh v1 must
    /// publish again — the guard must not reject it as "older than" the closed
    /// document's last version.
    #[test]
    fn reopen_after_close_publishes_fresh_version() {
        let mut published: HashMap<Url, i32> = HashMap::new();
        assert!(claim_publish_slot(&mut published, &uri(), 5));
        // didClose removes the entry.
        published.remove(&uri());
        // Reopen resets the editor version to 1 — it must publish.
        assert!(
            claim_publish_slot(&mut published, &uri(), 1),
            "a reopened document's v1 must not be blocked by the old v5"
        );
        assert_eq!(published.get(&uri()), Some(&1));
    }
}

#[cfg(test)]
mod lifecycle_tests {
    //! End-to-end proof of the analyze pipeline WITHOUT the LSP transport: the
    //! exact steps `analyze` runs inside `spawn_blocking` — parse the buffer
    //! through a session, then map its unified diagnostics to LSP diagnostics via
    //! the buffer's line index. A live `Client` is needed only for the final
    //! `publish_diagnostics` call, so these tests exercise everything up to (and
    //! including) the produced `Vec<Diagnostic>` — the part that could be wrong.

    use crate::diagnostics::to_lsp_diagnostics;
    use crate::positions::LineIndex;
    use crate::session::build_fallback_session_for_test as build_fallback;
    use crate::session::build_fallback_session_with_search_path;
    use tower_lsp::lsp_types::DiagnosticSeverity;

    /// didOpen/didChange → parse → correctly-ranged diagnostics. An unknown
    /// `{$IF}` on a known line must surface a WARNING whose range covers that
    /// directive, mapped through the buffer's line index.
    #[test]
    fn parse_buffer_produces_correctly_ranged_diagnostic() {
        let mut session = build_fallback();
        // The {$IF} on line 2 references an unknown external type → parse
        // diagnostic. Byte layout puts the directive on line index 2.
        let text = "unit Editing;\ninterface\n{$IF SizeOf(TMysteryExternal) > 4} const A = 1; {$IFEND}\ntype TThing = class end;\nimplementation\nend.";
        let index = LineIndex::new(text.to_string());
        let path = std::env::temp_dir().join("ddk-server-e2e").join("Editing.pas");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let (_, meta) = session.parse_buffer(&path, index.text()).unwrap();
        let meta = meta.expect("unit meta");
        let buffer_file = meta.ast.name.location.file;
        let unified = session.diagnostics(meta.name());
        assert!(!unified.is_empty(), "an unknown {{$IF}} yields a diagnostic");

        let lsp = to_lsp_diagnostics(&unified, buffer_file, &index);
        assert!(!lsp.is_empty(), "diagnostics map to LSP");
        // every produced diagnostic is a WARNING with a valid range on a real
        // line (not a fabricated one), and the {$IF} finding sits on line 2.
        let on_directive_line = lsp.iter().any(|d| d.range.start.line == 2);
        assert!(
            on_directive_line,
            "the {{$IF}} diagnostic maps onto its source line (line 2): {lsp:?}"
        );
        assert!(lsp.iter().all(|d| d.severity == Some(DiagnosticSeverity::WARNING)));
        // ranges are non-degenerate for in-buffer findings on the directive line
        let directive = lsp.iter().find(|d| d.range.start.line == 2).unwrap();
        assert!(
            directive.range.end.character > directive.range.start.character
                || directive.range.end.line > directive.range.start.line,
            "the range spans the directive, not a zero-length point: {directive:?}"
        );
    }

    /// Part B end-to-end through the server mapping: a buffer that imports a
    /// referenced unit (Used) and an unreferenced unit (Unused) publishes a HINT
    /// for Unused and NONE for Used — the conservative unused-uses hint reaches
    /// the editor as a HINT-severity diagnostic, never a wrong "delete Used".
    #[test]
    fn unused_import_surfaces_as_hint_used_one_does_not() {
        let directory = std::env::temp_dir().join("ddk-server-unused-uses");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("Used.pas"),
            "unit Used;\ninterface\ntype TUsed = class end;\nimplementation\nend.",
        )
        .unwrap();
        std::fs::write(
            directory.join("Unused.pas"),
            "unit Unused;\ninterface\ntype TUnused = class end;\nimplementation\nend.",
        )
        .unwrap();

        let mut session = build_fallback_session_with_search_path(directory.clone());
        let text = "unit Consumer;\ninterface\nuses Used, Unused;\n\
             implementation\n\
             procedure P;\nvar X: TUsed;\nbegin X := TUsed.Create; end;\n\
             end.";
        let index = LineIndex::new(text.to_string());
        let path = directory.join("Consumer.pas");

        let (_, meta) = session.parse_buffer(&path, index.text()).unwrap();
        let meta = meta.expect("unit meta");
        let buffer_file = meta.ast.name.location.file;
        let unified = session.diagnostics(meta.name());
        let lsp = to_lsp_diagnostics(&unified, buffer_file, &index);

        // exactly one HINT, naming Unused, from the analysis source, and NOT Used.
        let hints: Vec<_> = lsp
            .iter()
            .filter(|d| d.severity == Some(DiagnosticSeverity::HINT))
            .collect();
        assert_eq!(hints.len(), 1, "one unused-uses hint: {lsp:?}");
        assert!(hints[0].message.contains("Unused"));
        assert!(!hints[0].message.contains("'Used'"), "the referenced Used is never flagged");
        assert_eq!(hints[0].source.as_deref(), Some("delphi-analysis"));
        // the hint sits on the uses-clause line (line index 2).
        assert_eq!(hints[0].range.start.line, 2);
    }

    /// A buffer edited into a genuinely UN-parseable state (an unrecoverable
    /// conditional-directive structure) must not go silent: the hard-`Err` arm
    /// of `analyze` REPLACES the stale set with a single `ERROR`-severity
    /// diagnostic ("failed to parse: <reason>"). This exercises the exact steps
    /// that arm runs — `parse_buffer` returns `Err`, and the failure is mapped to
    /// the ERROR diagnostic via [`crate::diagnostics::parse_failure_diagnostic`]
    /// — proving `Severity::Error` now has a real producer and the buffer gets a
    /// squiggle instead of only a log line.
    #[test]
    fn unrecoverable_buffer_publishes_single_error_diagnostic() {
        use crate::diagnostics::parse_failure_diagnostic;

        let mut session = build_fallback();
        // An unterminated `{$IFDEF}` (no matching `{$ENDIF}`) is an unrecoverable
        // directive-structure error: the conditional-compilation skeleton the
        // token cursor relies on is broken, so the parse fails hard rather than
        // recovering. This is the "editing a file into a genuinely un-parseable
        // state" case.
        let text = "unit Broken;\ninterface\n{$IFDEF FOO}\ntype TThing = class end;\nimplementation\nend.";
        let index = LineIndex::new(text.to_string());
        let path = std::env::temp_dir().join("ddk-server-e2e").join("Broken.pas");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        // The parse fails hard — no meta, an `Err` carrying the failure message
        // (and, when available, the error's source location).
        let error = match session.parse_buffer(&path, index.text()) {
            Err(error) => error,
            Ok(_) => panic!("an unterminated {{$IFDEF}} is an unrecoverable parse failure"),
        };

        // Reproduce the hard-`Err` arm of `analyze`: map the failure to the
        // single ERROR diagnostic that REPLACES the stale set.
        let span = error
            .location
            .map(|location| (location.span.start as usize, location.span.end as usize));
        let diagnostic = parse_failure_diagnostic(&error.message, span, &index);

        // ERROR severity (the previously-dead `Severity::Error` now has a real
        // producer), a "failed to parse" message, and the parse source label.
        assert_eq!(
            diagnostic.severity,
            Some(DiagnosticSeverity::ERROR),
            "an unrecoverable parse failure publishes an ERROR diagnostic: {diagnostic:?}"
        );
        assert!(
            diagnostic.message.starts_with("failed to parse:"),
            "the message states the parse failure: {diagnostic:?}"
        );
        assert_eq!(diagnostic.source.as_deref(), Some("delphi"));
        // The unrecoverable class of failure (broken conditional-directive
        // structure) is the one `parse_and_cache` surfaces as a hard `Err`, and
        // that variant carries NO intrinsic location — so the anchor honestly
        // falls back to the top of the document (never a fabricated specific
        // range). The location THREADING is still wired (see `SessionError`), so
        // any future hard failure that DOES carry a location gets a precise
        // squiggle; for today's directive failure, top-of-document is correct.
        assert_eq!(
            diagnostic.range,
            tower_lsp::lsp_types::Range::default(),
            "no intrinsic location → honest top-of-document anchor: {diagnostic:?}"
        );
        // The published set is exactly this one honest finding — not empty (which
        // would leave NO feedback) and not the stale prior set.
        let published = vec![diagnostic];
        assert_eq!(published.len(), 1, "exactly one replacing ERROR diagnostic");
        assert!(published.iter().all(|d| d.severity == Some(DiagnosticSeverity::ERROR)));
    }

    /// A clean buffer produces an empty diagnostic set (didChange to valid code
    /// clears the squiggles).
    #[test]
    fn clean_buffer_produces_no_diagnostics() {
        let mut session = build_fallback();
        let text = "unit Clean;\ninterface\ntype TThing = class end;\nimplementation\nend.";
        let index = LineIndex::new(text.to_string());
        let path = std::env::temp_dir().join("ddk-server-e2e").join("Clean.pas");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();

        let (_, meta) = session.parse_buffer(&path, index.text()).unwrap();
        let meta = meta.expect("unit meta");
        let unified = session.diagnostics(meta.name());
        let lsp = to_lsp_diagnostics(&unified, meta.ast.name.location.file, &index);
        assert!(lsp.is_empty(), "a clean unit has no diagnostics: {lsp:?}");
    }
}
