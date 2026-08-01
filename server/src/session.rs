//! Bridges ddk-core's project/compiler configuration into a
//! [`delphi_parser::driver::ProjectSession`], and owns that session behind an
//! async-safe lock so parses run OFF the async executor.
//!
//! ## Async / lock model (why it can't deadlock or block the executor)
//!
//! `ProjectSession` is SYNCHRONOUS and does blocking file IO + parsing. It must
//! never run on a tokio worker thread directly (that would stall the executor).
//! So:
//!
//! - The session lives in `Arc<tokio::sync::Mutex<Option<ProjectSession>>>`
//!   (`Option` because no project may be resolvable yet — the server degrades
//!   gracefully rather than panicking).
//! - Every parse runs inside `tokio::task::spawn_blocking`. Inside that blocking
//!   thread we take the lock with `blocking_lock()`. The lock is a plain
//!   critical section around a synchronous parse — it is NEVER held across an
//!   `.await`. The async caller `.await`s the JoinHandle, not the lock, so no
//!   task holds the mutex while suspended: a classic async-mutex-across-await
//!   deadlock is structurally impossible here.
//! - One session per process (the parser's interner/arena are process globals —
//!   one project per process, which is exactly the LSP model). Re-opening for a
//!   different (project, config, platform) swaps the `Option`'s contents.
//!
//! ## CompilerProfile bridge
//!
//! ddk-core's [`CompilerConfiguration`] carries `compiler_version: usize` (36
//! for Delphi 12) and `condition: String` (the `VERxxx` auto-define). The parser
//! needs a [`CompilerProfile`] with `compiler_version: f64`, an optional
//! `rtl_version` (None ⇒ equals compiler_version — correct for every modern
//! Delphi), and the full auto-define list. The `VERxxx` condition is one define;
//! the rest are the standard compiler/platform symbols (MSWINDOWS, UNICODE,
//! CPU…, WIN32/WIN64) that dcc defines for the target — assembled here from the
//! selected platform. `DCC_Define`s from the dproj are added by
//! `ProjectContext::from_dproj` on top of these.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::sync::Mutex;

use delphi_parser::context::CompilerProfile;
use delphi_parser::driver::{ProjectSession, SessionError, SessionOptions};

use ddk_core::projects::CompilerConfiguration;

/// The parser session for the currently-open project, plus the identity it was
/// opened for so we know when a re-open is needed.
pub struct SessionManager {
    session: Arc<Mutex<Option<ProjectSession>>>,
    /// Identity of the currently-open session: `(dproj_path, config, platform)`.
    /// `None` when no session is open (nothing resolvable yet, or a fallback
    /// context with no dproj). Guarded by the same mutex as `session` via a
    /// second lock kept strictly for identity comparison (never held across a
    /// parse).
    identity: Arc<Mutex<Option<SessionIdentity>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionIdentity {
    dproj: Option<PathBuf>,
    configuration: String,
    platform: String,
}

impl Default for SessionManager {
    fn default() -> Self {
        Self::new()
    }
}

impl SessionManager {
    pub fn new() -> Self {
        Self {
            session: Arc::new(Mutex::new(None)),
            identity: Arc::new(Mutex::new(None)),
        }
    }

    /// Shared handle to the session mutex — cloned into `spawn_blocking` tasks.
    pub fn handle(&self) -> Arc<Mutex<Option<ProjectSession>>> {
        self.session.clone()
    }

    /// Ensure a session is open for `(dproj, config, platform, profile)`. Opens
    /// (or re-opens on an identity change) the parser [`ProjectSession`]. On any
    /// failure the session is left `None` (graceful degradation — the caller
    /// falls back to a default context). Returns whether a session is now open.
    ///
    /// Runs the (blocking) open on a blocking thread and never holds the lock
    /// across `.await`.
    pub async fn ensure_open(
        &self,
        dproj: Option<PathBuf>,
        configuration: String,
        platform: String,
        profile: CompilerProfile,
        standard_source_paths: Vec<PathBuf>,
    ) -> bool {
        let wanted = SessionIdentity {
            dproj: dproj.clone(),
            configuration: configuration.clone(),
            platform: platform.clone(),
        };
        // Already open for this exact identity? Nothing to do.
        {
            let current = self.identity.lock().await;
            if current.as_ref() == Some(&wanted) {
                return true;
            }
        }

        let Some(dproj_path) = dproj.clone() else {
            // No dproj → build the graceful default-context fallback session.
            return self
                .open_fallback(wanted, configuration, platform, standard_source_paths)
                .await;
        };

        let session_slot = self.session.clone();
        let identity_slot = self.identity.clone();
        let opened = tokio::task::spawn_blocking(move || {
            let options = SessionOptions {
                // The LSP owns file watching (ddk-core watchers + didChange);
                // avoid double-watching by keeping the parser's OS watcher off
                // and driving invalidation from the editor lifecycle.
                watch: false,
                standard_source_paths,
                ..SessionOptions::default()
            };
            ProjectSession::open(
                &dproj_path,
                Some(&configuration),
                Some(&platform),
                &profile,
                options,
            )
        })
        .await;

        match opened {
            Ok(Ok(session)) => {
                *session_slot.lock().await = Some(session);
                *identity_slot.lock().await = Some(wanted);
                true
            }
            Ok(Err(error)) => {
                self.clear_on_error(error).await;
                false
            }
            Err(join_error) => {
                // The blocking task panicked or was cancelled — never propagate,
                // degrade to no session.
                self.clear_on_error(SessionError {
                    message: format!("session open task failed: {join_error}"),
                })
                .await;
                false
            }
        }
    }

    /// Build a minimal default-context session when no dproj is resolvable, so
    /// buffers still parse (with no project search paths / defines beyond the
    /// compiler profile). Never panics.
    async fn open_fallback(
        &self,
        wanted: SessionIdentity,
        configuration: String,
        platform: String,
        standard_source_paths: Vec<PathBuf>,
    ) -> bool {
        let session_slot = self.session.clone();
        let identity_slot = self.identity.clone();
        let built = tokio::task::spawn_blocking(move || {
            build_fallback_session(&configuration, &platform, standard_source_paths)
        })
        .await;

        match built {
            Ok(Ok(session)) => {
                *session_slot.lock().await = Some(session);
                *identity_slot.lock().await = Some(wanted);
                true
            }
            _ => {
                *session_slot.lock().await = None;
                *identity_slot.lock().await = None;
                false
            }
        }
    }

    async fn clear_on_error(&self, error: SessionError) {
        eprintln!("ddk-server: could not open project session: {}", error.message);
        *self.session.lock().await = None;
        *self.identity.lock().await = None;
    }
}

/// Build a fallback [`ProjectSession`] over a minimal default context: no dproj,
/// default Debug/Win32-ish switches, only the standard source paths as search
/// paths. Used when no project is resolvable — buffers still parse, degrade
/// gracefully. The snapshot store points at a scratch subdirectory so a
/// fallback never clobbers a real project's cache.
fn build_fallback_session(
    configuration: &str,
    platform: &str,
    standard_source_paths: Vec<PathBuf>,
) -> Result<ProjectSession, SessionError> {
    use delphi_parser::cache_store::{CacheIdentity, CacheStore};
    use delphi_parser::context::{
        DefineSet, ProjectContext, SwitchState, TargetPlatform,
    };
    use delphi_parser::unit_cache::UnitCache;
    use std::collections::HashMap;
    use std::time::Duration;

    let target = TargetPlatform::from_dproj_name(platform);
    let context = ProjectContext {
        configuration: configuration.to_string(),
        platform_name: platform.to_string(),
        platform: target,
        compiler_version: 36.0,
        rtl_version: 36.0,
        base_defines: DefineSet::default(),
        search_paths: standard_source_paths,
        include_paths: Vec::new(),
        namespaces: Vec::new(),
        unit_aliases: HashMap::new(),
        default_switches: SwitchState::default(),
        unit_cache: UnitCache::default(),
    };

    // A scratch cache identity so the fallback never shares a real project's
    // snapshot file. A stable synthetic path keyed on config/platform. The cache
    // identity canonicalizes the project path, so the placeholder file must
    // exist on disk — create it (idempotently) before building the identity.
    let scratch_base = std::env::temp_dir().join("ddk-server").join("fallback-cache");
    std::fs::create_dir_all(&scratch_base).map_err(|error| SessionError {
        message: format!("cannot create fallback cache dir: {error}"),
    })?;
    let synthetic_dproj = scratch_base.join(format!("{configuration}-{platform}.dproj"));
    if !synthetic_dproj.exists() {
        // A minimal placeholder; its only role is to give the cache identity a
        // canonicalizable, project-stable path. Never parsed.
        std::fs::write(&synthetic_dproj, b"<Project/>").map_err(|error| SessionError {
            message: format!("cannot create fallback dproj placeholder: {error}"),
        })?;
    }
    let identity = CacheIdentity {
        project_path: &synthetic_dproj,
        configuration,
        platform,
        compiler_version: 36.0,
    };
    let store = CacheStore::in_directory(&scratch_base, &identity)
        .map_err(|error| SessionError { message: error.message })?;

    Ok(ProjectSession::from_parts(
        Arc::new(context),
        store,
        Duration::from_secs(300),
    ))
}

/// Bridge a ddk-core [`CompilerConfiguration`] + target platform into a parser
/// [`CompilerProfile`]. The `VERxxx` condition plus the standard compiler +
/// platform auto-defines dcc emits for the target.
pub fn compiler_profile(compiler: &CompilerConfiguration, platform: &str) -> CompilerProfile {
    let mut defines: Vec<String> = Vec::new();
    // The compiler's own VERxxx condition (e.g. "VER360").
    if !compiler.condition.trim().is_empty() {
        defines.push(compiler.condition.clone());
    }
    // Compiler-family symbols (modern Delphi, Unicode RTL).
    for symbol in ["CONDITIONALEXPRESSIONS", "UNICODE", "ASSEMBLER"] {
        defines.push(symbol.to_string());
    }
    // Target-platform symbols. Delphi's Win32/Win64 auto-defines.
    match platform.to_ascii_lowercase().as_str() {
        "win32" => {
            for symbol in ["MSWINDOWS", "WIN32", "CPU386", "CPUX86", "CPU32BITS"] {
                defines.push(symbol.to_string());
            }
        }
        "win64" | "win64x" => {
            for symbol in ["MSWINDOWS", "WIN64", "CPUX64", "CPU64BITS"] {
                defines.push(symbol.to_string());
            }
        }
        // Unknown platform: still define the Windows base (the dominant target);
        // an unknown platform never gets a fabricated CPU symbol.
        _ => defines.push("MSWINDOWS".to_string()),
    }

    CompilerProfile {
        compiler_version: compiler.compiler_version as f64,
        // RTLVersion == CompilerVersion for every modern Delphi; let the parser
        // default (None ⇒ compiler_version) apply.
        rtl_version: None,
        defines,
    }
}

/// A `file://` URL → local filesystem path. Returns `None` for a non-file URL.
pub fn uri_to_path(uri: &tower_lsp::lsp_types::Url) -> Option<PathBuf> {
    uri.to_file_path().ok()
}

/// The `.pas`/`.dpr`/`.dpk` this path names, unchanged — a hook for future
/// extension-specific handling. Currently the identity function; kept so callers
/// have one place to normalize a document path.
pub fn document_path(path: &Path) -> PathBuf {
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(version: usize, condition: &str) -> CompilerConfiguration {
        CompilerConfiguration {
            condition: condition.to_string(),
            product_name: "Delphi".to_string(),
            product_version: 23,
            package_version: 290,
            compiler_version: version,
            installation_path: String::new(),
            build_arguments: Vec::new(),
        }
    }

    #[test]
    fn profile_bridges_version_and_platform_defines() {
        let profile = compiler_profile(&config(36, "VER360"), "Win32");
        assert_eq!(profile.compiler_version, 36.0);
        assert_eq!(profile.rtl_version, None); // ⇒ equals compiler_version
        assert!(profile.defines.contains(&"VER360".to_string()));
        assert!(profile.defines.contains(&"MSWINDOWS".to_string()));
        assert!(profile.defines.contains(&"WIN32".to_string()));
        assert!(profile.defines.contains(&"CPUX86".to_string()));
        assert!(profile.defines.contains(&"UNICODE".to_string()));
        // no Win64 symbols leaked into a Win32 profile
        assert!(!profile.defines.contains(&"WIN64".to_string()));
    }

    #[test]
    fn profile_win64_defines() {
        let profile = compiler_profile(&config(36, "VER360"), "Win64");
        assert!(profile.defines.contains(&"WIN64".to_string()));
        assert!(profile.defines.contains(&"CPUX64".to_string()));
        assert!(!profile.defines.contains(&"WIN32".to_string()));
    }

    #[test]
    fn profile_unknown_platform_defines_only_windows_base() {
        let profile = compiler_profile(&config(36, "VER360"), "Linux64");
        assert!(profile.defines.contains(&"MSWINDOWS".to_string()));
        assert!(!profile.defines.contains(&"WIN32".to_string()));
        assert!(!profile.defines.contains(&"CPUX64".to_string()));
    }

    #[test]
    fn empty_condition_is_not_pushed() {
        let profile = compiler_profile(&config(36, "   "), "Win32");
        assert!(!profile.defines.iter().any(|d| d.trim().is_empty()));
    }

    /// The fallback session builds without a dproj and parses a buffer — proving
    /// the graceful no-project degradation path never panics.
    #[test]
    fn fallback_session_parses_a_buffer() {
        let mut session =
            build_fallback_session("Debug", "Win32", Vec::new()).expect("fallback builds");
        let dir = std::env::temp_dir().join("ddk-server-fallback-test");
        std::fs::create_dir_all(&dir).unwrap();
        let (_, meta) = session
            .parse_buffer(
                dir.join("Scratch.pas"),
                "unit Scratch;\ninterface\ntype TX = class end;\nimplementation\nend.",
            )
            .expect("buffer parses under fallback context");
        let meta = meta.expect("unit meta");
        assert!(
            meta.interface()
                .contains_key(delphi_parser::globals::intern_key("TX"))
        );
    }
}
