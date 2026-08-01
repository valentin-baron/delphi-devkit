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
use delphi_parser::unit_cache::SaveReport;

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

    /// Test-only: inject a prebuilt session (e.g. a fallback with an explicit
    /// snapshot base), bypassing `ensure_open`, so the persistence paths
    /// (`save_now` / `parse_disk_and_save`) can be exercised against a session
    /// whose snapshot lands in a caller-controlled temp directory.
    #[cfg(test)]
    pub async fn inject_session_for_test(&self, session: ProjectSession) {
        *self.session.lock().await = Some(session);
    }

    /// Ensure a session is open for `(dproj, config, platform, profile)`. Opens
    /// (or re-opens on an identity change) the parser [`ProjectSession`]. On any
    /// failure the session is left `None` (graceful degradation — the caller
    /// falls back to a default context). Returns whether a session is now open.
    ///
    /// Runs the (blocking) open on a blocking thread and never holds the lock
    /// across `.await`.
    ///
    /// Sequencing note (finding 4): this releases the session lock before the
    /// caller's parse re-acquires it. That window cannot swap in a
    /// *different-project* session under the current model — one active project
    /// per process (process-global interner/arena) and every concurrent caller
    /// resolves the same inputs, so an interleaved re-open targets the identical
    /// identity and returns early here. See `DelphiLsp::analyze` for the full
    /// argument.
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
                self.clear_on_error(SessionError::message(format!(
                    "session open task failed: {join_error}"
                )))
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

    /// Persist the session's snapshot NOW (the LocalAppData cache). Runs the
    /// blocking save on a blocking thread, taking the session lock with
    /// `blocking_lock()` — the lock is a plain synchronous critical section
    /// around `ProjectSession::save_now`, never held across `.await` (the async
    /// caller awaits the JoinHandle, not the lock).
    ///
    /// No session open (nothing resolvable) → `Ok(None)`, a legitimate no-op.
    /// Returns the [`SaveReport`] so the caller can LOG any skipped units — a
    /// skipped meta is a dropped-and-re-parsed-next-session unit, never silently
    /// swallowed. The VIRTUAL open buffer is not persistable BY CONSTRUCTION
    /// (invariant #21/#25 — a virtual `FileId` never validates on load), so
    /// `save_now` only ever writes on-disk units.
    pub async fn save_now(&self) -> Result<Option<SaveReport>, SessionError> {
        let session_handle = self.session.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = session_handle.blocking_lock();
            match guard.as_mut() {
                Some(project_session) => project_session.save_now().map(Some),
                None => Ok(None),
            }
        })
        .await;

        match joined {
            Ok(result) => result,
            Err(join_error) => Err(SessionError::message(format!(
                "save task failed: {join_error}"
            ))),
        }
    }

    /// A document was CLOSED: free its virtual (unsaved) buffer's content in the
    /// process-global arena, returning the memory it held (Task-15 `did_close`
    /// cleanup). No-op when no session is open or the path was never a virtual
    /// buffer. Runs on a blocking task under the session lock (a short critical
    /// section, never held across `.await`) so it serializes with parses — a
    /// close can only free content between parses, never during one.
    pub async fn free_virtual_buffer(&self, path: PathBuf) {
        let session_handle = self.session.clone();
        let _ = tokio::task::spawn_blocking(move || {
            let guard = session_handle.blocking_lock();
            if let Some(project_session) = guard.as_ref() {
                project_session.arena().free_virtual(&path);
            }
        })
        .await;
    }

    /// A file was SAVED to disk: parse it from DISK (via
    /// [`ProjectSession::parse_source_file`], so the file AND its imports enter
    /// the cache as PERSISTABLE on-disk units — not the virtual editor buffer),
    /// then persist the snapshot.
    ///
    /// This is the didSave persistence path. The parse + save run on ONE
    /// blocking task under a single `blocking_lock()` acquisition, so no other
    /// task can interleave a re-open between the parse and the save; the lock is
    /// never held across `.await`.
    ///
    /// No session open → `Ok(None)` (nothing to persist). A parse failure is
    /// returned as `Err` (the caller logs it, best-effort); a successful parse
    /// returns the [`SaveReport`] so the caller can log skipped units.
    pub async fn parse_disk_and_save(
        &self,
        path: PathBuf,
    ) -> Result<Option<SaveReport>, SessionError> {
        let session_handle = self.session.clone();
        let joined = tokio::task::spawn_blocking(move || {
            let mut guard = session_handle.blocking_lock();
            let Some(project_session) = guard.as_mut() else {
                return Ok(None);
            };
            // Parse from disk so the saved file and its imports become on-disk
            // (persistable) cache units. A parse failure here does not abort the
            // save — the imports pulled in before the failure are still worth
            // persisting — but we surface the parse error to the caller AFTER
            // the save so it is logged, not swallowed.
            let parse_result = project_session.parse_source_file(&path);
            let save_report = project_session.save_now()?;
            match parse_result {
                Ok(_) => Ok(Some(save_report)),
                Err(parse_error) => Err(SessionError {
                    message: format!(
                        "saved snapshot but parse of {} failed: {}",
                        path.display(),
                        parse_error.message
                    ),
                    location: parse_error.location,
                }),
            }
        })
        .await;

        match joined {
            Ok(result) => result,
            Err(join_error) => Err(SessionError::message(format!(
                "didSave persistence task failed: {join_error}"
            ))),
        }
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
    let scratch_base = std::env::temp_dir().join("ddk-server").join("fallback-cache");
    build_fallback_session_in(configuration, platform, standard_source_paths, scratch_base)
}

/// The fallback-session builder with an EXPLICIT snapshot base directory. Both
/// the placeholder synthetic dproj (needed to give the cache identity a
/// canonicalizable path) and the snapshot `.bin` land under `snapshot_base`, so
/// a caller (e.g. a server test) can point persistence at a temp directory and
/// assert the snapshot was written. The default fallback (above) uses a stable
/// scratch subdirectory so it never clobbers a real project's cache.
fn build_fallback_session_in(
    configuration: &str,
    platform: &str,
    standard_source_paths: Vec<PathBuf>,
    snapshot_base: PathBuf,
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
    let scratch_base = snapshot_base;
    std::fs::create_dir_all(&scratch_base)
        .map_err(|error| SessionError::message(format!("cannot create fallback cache dir: {error}")))?;
    let synthetic_dproj = scratch_base.join(format!("{configuration}-{platform}.dproj"));
    if !synthetic_dproj.exists() {
        // A minimal placeholder; its only role is to give the cache identity a
        // canonicalizable, project-stable path. Never parsed.
        std::fs::write(&synthetic_dproj, b"<Project/>").map_err(|error| {
            SessionError::message(format!("cannot create fallback dproj placeholder: {error}"))
        })?;
    }
    let identity = CacheIdentity {
        project_path: &synthetic_dproj,
        configuration,
        platform,
        compiler_version: 36.0,
    };
    let store = CacheStore::in_directory(&scratch_base, &identity)
        .map_err(|error| SessionError::message(error.message))?;

    Ok(ProjectSession::from_parts(
        Arc::new(context),
        store,
        Duration::from_secs(300),
    ))
}

/// Test-only: build a fallback (no-dproj) session for the default Debug/Win32
/// context. Exposed so the server's end-to-end lifecycle tests can drive the
/// analyze pipeline without live ddk-core project state.
#[cfg(test)]
pub fn build_fallback_session_for_test() -> ProjectSession {
    build_fallback_session("Debug", "Win32", Vec::new()).expect("fallback session builds")
}

/// Test-only: a fallback session whose search paths include `directory`, so a
/// buffer's `uses` clause resolves against on-disk sibling units there. Used by
/// the unused-uses diagnostic test (cross-unit import resolution required).
#[cfg(test)]
pub fn build_fallback_session_with_search_path(directory: PathBuf) -> ProjectSession {
    build_fallback_session("Debug", "Win32", vec![directory])
        .expect("fallback session builds")
}

/// Test-only: a fallback session whose snapshot base AND search path are both
/// `directory`, so persistence lands in a caller-controlled temp directory (the
/// snapshot `.bin` can then be asserted on disk) and a saved file's imports
/// resolve against the same directory. Used by the persistence tests.
#[cfg(test)]
pub fn build_fallback_session_with_snapshot_base(directory: PathBuf) -> ProjectSession {
    build_fallback_session_in("Debug", "Win32", vec![directory.clone()], directory)
        .expect("fallback session builds")
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

/// The project/compiler inputs a session needs, resolved from ddk-core state.
#[derive(Debug, Clone)]
pub struct ProjectInputs {
    pub dproj: Option<PathBuf>,
    pub configuration: String,
    pub platform: String,
    pub profile: CompilerProfile,
    pub standard_source_paths: Vec<PathBuf>,
}

/// Resolve the inputs for the active project from ddk-core's `PROJECTS_DATA` and
/// `COMPILER_CONFIGURATIONS`. Degrades gracefully:
///
/// - no active project, or an active project with no `.dproj` → a fallback
///   (`dproj: None`) with a default Delphi-12 Win32 profile — buffers still
///   parse, just without project search paths / dproj defines;
/// - the active project's workspace names its compiler; its
///   `installation_path/source` subtree provides the standard-unit search paths.
///
/// This is the FOUNDATION resolver: one session for the active project. A
/// per-document project match (which project owns an arbitrary opened file) is a
/// later refinement — noted, not silently assumed.
pub async fn resolve_active_project_inputs() -> ProjectInputs {
    use ddk_core::state::{COMPILER_CONFIGURATIONS, PROJECTS_DATA};

    let projects = PROJECTS_DATA.read().await;
    let Some(project) = projects.active_project() else {
        return fallback_inputs();
    };

    // The compiler for this project = its workspace's compiler_id.
    let compiler_id = projects
        .workspaces
        .iter()
        .find(|workspace| {
            workspace
                .project_links
                .iter()
                .any(|link| link.project_id == project.id)
        })
        .map(|workspace| workspace.compiler_id.clone())
        .unwrap_or_else(|| projects.group_project_compiler_id.clone());

    let compilers = COMPILER_CONFIGURATIONS.read().await;
    let Some(compiler) = compilers.get(&compiler_id) else {
        return fallback_inputs();
    };

    // Resolve config/platform: the project's dproj (if any) supplies the active
    // defaults, honoring per-project overrides.
    let dproj_path = project.dproj.as_ref().map(PathBuf::from);
    let (configuration, platform) = match &dproj_path {
        Some(path) => match ddk_core::files::dproj::get_or_load(project.id, path) {
            Ok(dproj) => project.effective_config_platform(&dproj),
            Err(_) => (
                project
                    .active_configuration
                    .clone()
                    .unwrap_or_else(|| "Debug".to_string()),
                project
                    .active_platform
                    .clone()
                    .unwrap_or_else(|| "Win32".to_string()),
            ),
        },
        None => (
            project
                .active_configuration
                .clone()
                .unwrap_or_else(|| "Debug".to_string()),
            project
                .active_platform
                .clone()
                .unwrap_or_else(|| "Win32".to_string()),
        ),
    };

    let profile = compiler_profile(compiler, &platform);
    let standard_source_paths = standard_source_paths(&compiler.installation_path);

    ProjectInputs {
        dproj: dproj_path,
        configuration,
        platform,
        profile,
        standard_source_paths,
    }
}

/// The default fallback inputs (Delphi 12 / Win32) when no project is
/// resolvable — never panics, always parseable.
fn fallback_inputs() -> ProjectInputs {
    ProjectInputs {
        dproj: None,
        configuration: "Debug".to_string(),
        platform: "Win32".to_string(),
        profile: CompilerProfile {
            compiler_version: 36.0,
            rtl_version: None,
            // Keep this set aligned with `compiler_profile(.., "Win32")`: the
            // VERxxx condition + compiler-family symbols + the full Win32
            // auto-define set (CPU386/CPUX86/CPU32BITS), so a fallback buffer
            // parses under the same defines a real Win32 project would.
            defines: vec![
                "VER360".to_string(),
                "CONDITIONALEXPRESSIONS".to_string(),
                "UNICODE".to_string(),
                "ASSEMBLER".to_string(),
                "MSWINDOWS".to_string(),
                "WIN32".to_string(),
                "CPU386".to_string(),
                "CPUX86".to_string(),
                "CPU32BITS".to_string(),
            ],
        },
        standard_source_paths: Vec::new(),
    }
}

/// The standard-unit source directories under a compiler installation
/// (`<install>\source\...` dirs containing `.pas`). Empty on any failure — a
/// missing RTL source tree degrades to "RTL units unresolved", never a crash.
fn standard_source_paths(installation_path: &str) -> Vec<PathBuf> {
    use delphi_parser::ddk::{standard_source_directories, CompilerInstallation};
    if installation_path.trim().is_empty() {
        return Vec::new();
    }
    let installation = CompilerInstallation {
        key: String::new(),
        product_name: String::new(),
        product_version: 0.0,
        compiler_version: 36.0,
        installation_path: PathBuf::from(installation_path),
    };
    standard_source_directories(&installation).unwrap_or_default()
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

    /// A fresh temp directory unique to this test run — the snapshot base for a
    /// persistence test, so its `.bin` never collides with another test's.
    fn fresh_snapshot_base(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let nonce = COUNTER.fetch_add(1, Ordering::Relaxed);
        let directory = std::env::temp_dir()
            .join("ddk-server-persist")
            .join(format!("{tag}-{}-{nonce}", std::process::id()));
        // Start clean so a pre-existing snapshot from a prior run can't mask a
        // "was a snapshot written THIS run" assertion.
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    /// Whether ANY snapshot `.bin` exists under `base` — the persisted cache
    /// file the fallback identity writes (its name is a hash, so match by
    /// extension rather than by exact name).
    fn snapshot_written(base: &Path) -> bool {
        std::fs::read_dir(base)
            .map(|entries| {
                entries.flatten().any(|entry| {
                    entry.path().extension().and_then(|e| e.to_str()) == Some("bin")
                })
            })
            .unwrap_or(false)
    }

    /// didSave path: `parse_disk_and_save` parses the saved file FROM DISK (so it
    /// and its import become persistable on-disk units) and writes a snapshot to
    /// the temp cache base. Proves the didSave handler persists.
    #[tokio::test]
    async fn did_save_parses_from_disk_and_writes_snapshot() {
        let base = fresh_snapshot_base("didsave");
        // A saved unit that imports a sibling on-disk unit — both are on-disk
        // (persistable) units the snapshot must capture.
        std::fs::write(
            base.join("Imported.pas"),
            "unit Imported;\ninterface\ntype TImported = class end;\nimplementation\nend.",
        )
        .unwrap();
        let saved = base.join("Saver.pas");
        // Saver `uses` the on-disk import and references its type — the disk
        // parse resolves the `uses` clause against the sibling on disk.
        std::fs::write(
            &saved,
            "unit Saver;\ninterface\nuses Imported;\n\
             type TSaver = class\n  Field: TImported;\nend;\n\
             implementation\nend.",
        )
        .unwrap();

        let manager = SessionManager::new();
        manager
            .inject_session_for_test(build_fallback_session_with_snapshot_base(base.clone()))
            .await;

        // No snapshot before the save.
        assert!(!snapshot_written(&base), "no snapshot before didSave");

        let report = manager
            .parse_disk_and_save(saved)
            .await
            .expect("parse+save succeeds")
            .expect("a session is open, so a report is produced");
        // The saved on-disk file persists (at least itself; imports that the
        // loader fully materialized as a cached meta are folded in too).
        assert!(
            report.written >= 1,
            "the saved on-disk file persists: {report:?}"
        );
        assert!(report.skipped.is_empty(), "no on-disk unit is skipped: {report:?}");
        assert!(snapshot_written(&base), "didSave wrote a snapshot .bin");

        // Reload the snapshot into a FRESH cache and prove the SAVED on-disk unit
        // survives the round-trip (it has a real on-disk FileId, so it
        // re-registers) — the persistence the didSave path exists to provide.
        use delphi_parser::unit_cache::UnitCache;
        let store_path = std::fs::read_dir(&base)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("bin"))
            .expect("a snapshot .bin exists");
        let fresh = UnitCache::default();
        let load_report = fresh.load(&store_path).expect("snapshot loads");
        fresh.run_pending_tasks();
        assert!(
            fresh
                .get(delphi_parser::globals::intern_key("Saver"))
                .is_some(),
            "the saved on-disk unit reloads from the snapshot: {load_report:?}"
        );
        // Imports the parser materialized into a full cached meta persist too;
        // the parser loads a `uses` target's INTERFACE lazily, so whether a given
        // import becomes a full cached meta (vs. an interface-only load) is a
        // parser-internal decision. The guarantee this test pins is that the
        // SAVED on-disk unit round-trips — the point of the didSave persistence
        // path. (The parser's own suite covers nested-unit caching.)
    }

    /// shutdown path: after a session did work, `save_now` writes the snapshot
    /// (best-effort persistence on server shutdown).
    #[tokio::test]
    async fn shutdown_save_now_writes_snapshot() {
        let base = fresh_snapshot_base("shutdown");
        std::fs::write(
            base.join("Persisted.pas"),
            "unit Persisted;\ninterface\ntype TP = class end;\nimplementation\nend.",
        )
        .unwrap();

        let mut session = build_fallback_session_with_snapshot_base(base.clone());
        // Parse an on-disk unit so there is durable state to persist.
        session
            .parse_source_file(base.join("Persisted.pas"))
            .expect("on-disk unit parses");
        let manager = SessionManager::new();
        manager.inject_session_for_test(session).await;

        assert!(!snapshot_written(&base), "no snapshot before shutdown save");
        let report = manager
            .save_now()
            .await
            .expect("save succeeds")
            .expect("a session is open");
        assert!(report.written >= 1, "the on-disk unit persists: {report:?}");
        assert!(snapshot_written(&base), "shutdown save wrote a snapshot .bin");
    }

    /// The VIRTUAL open buffer must NEVER persist ACROSS SESSIONS (invariant
    /// #21/#25). A virtual unit's meta IS written into the snapshot segment, but
    /// its display-only `FileId` path does not exist on disk, so on LOAD it fails
    /// to `register` (canonicalize) and is dropped as unreadable — it never
    /// re-enters a fresh cache. This is the load-time gate the invariant
    /// describes; the proof is a save→load round-trip that does NOT surface the
    /// virtual unit. (Contrast the on-disk unit tests, which DO reload.)
    #[tokio::test]
    async fn virtual_open_buffer_does_not_survive_a_save_load_roundtrip() {
        let base = fresh_snapshot_base("virtual");
        let mut session = build_fallback_session_with_snapshot_base(base.clone());
        // An UNSAVED buffer whose display path does NOT exist on disk — the
        // essence of an editor buffer with unsaved content.
        let virtual_path = base.join("VirtualOnly_unsaved.pas");
        assert!(!virtual_path.exists(), "the buffer path must not exist on disk");
        session
            .parse_buffer(
                &virtual_path,
                "unit VirtualOnly;\ninterface\ntype TVirtual = class end;\n\
                 implementation\nend.",
            )
            .expect("virtual buffer parses");
        let manager = SessionManager::new();
        manager.inject_session_for_test(session).await;

        // save_now succeeds; a snapshot .bin lands on disk.
        manager
            .save_now()
            .await
            .expect("save succeeds")
            .expect("session open");
        assert!(snapshot_written(&base), "a snapshot .bin is written");

        // Reload the snapshot into a FRESH cache: the virtual unit must NOT come
        // back (its FileId cannot re-register → dropped as unreadable). A future
        // session therefore never sees the unsaved buffer's state.
        use delphi_parser::unit_cache::UnitCache;
        let store_path = std::fs::read_dir(&base)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .find(|p| p.extension().and_then(|x| x.to_str()) == Some("bin"))
            .expect("a snapshot .bin exists");
        let fresh = UnitCache::default();
        let report = fresh.load(&store_path).expect("snapshot loads");
        fresh.run_pending_tasks();
        assert!(
            fresh
                .get(delphi_parser::globals::intern_key("VirtualOnly"))
                .is_none(),
            "the virtual (unsaved) unit must be absent from a freshly loaded cache: {report:?}"
        );
        assert!(
            report.unreadable >= 1,
            "the virtual unit is dropped as unreadable on load: {report:?}"
        );
    }
}
