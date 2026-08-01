//! The persistent `ddk/serverStatus` notification (Task 24).
//!
//! Task 17 wired TRANSIENT `window/workDoneProgress` — a spinner shown only for
//! the duration of a single operation. This module adds a COMPLEMENTARY,
//! PERSISTENT status the server pushes so the editor can always show, at a
//! glance, what ddk-server is currently doing (Ready / Analyzing / Indexing N/M /
//! Bootstrapping). The two are independent: task 17 drives VS Code's built-in
//! progress UI, this drives a persistent status-bar item; keeping both means the
//! user sees both the momentary spinner AND the standing "what is the server up
//! to" indicator.
//!
//! ## BEST-EFFORT TELEMETRY
//!
//! Exactly like [`crate::progress`], a status send is fire-and-forget: a failure
//! to deliver the notification must NEVER fail or slow the operation it
//! describes. [`DelphiLsp::set_status`](crate::DelphiLsp::set_status) clones the
//! `Client` (a cheap handle) and awaits `send_notification` with no document or
//! session lock held — so a status update can never introduce a
//! lock-across-`.await` (task-8 discipline) and never blocks analyze/indexing.
//!
//! ## NEVER MISLEAD
//!
//! A transient state (Analyzing/Indexing/Bootstrapping) is always paired with a
//! return to [`ServerState::Ready`] on completion OR cancel, so the editor never
//! shows a stuck spinner for work that already finished or was preempted. The
//! indexing pass in particular flips back to `Ready` both when the pass runs to
//! the end AND when a foreground event cancels it mid-pass.

use serde::{Deserialize, Serialize};
use tower_lsp::lsp_types::notification::Notification;

/// The overall state of the language server, as shown in the persistent status
/// view. Serialized as its variant name (`"Ready"`, `"Analyzing"`, …) so the
/// extension can switch on a plain string.
#[derive(Debug, Eq, PartialEq, Clone, Copy, Deserialize, Serialize)]
pub enum ServerState {
    /// The server is starting up (before `initialized`). The extension shows a
    /// "starting…" indicator until the first `Ready`.
    Initializing,
    /// Idle and ready — no analyze/indexing/bootstrapping in flight.
    Ready,
    /// Analyzing a single open buffer (parse + diagnostics). `detail` carries the
    /// file name being analyzed.
    Analyzing,
    /// The background indexing pass (Task 18) is warming the project's units.
    /// `current`/`total` carry the N/M progress and `detail` the current unit.
    Indexing,
    /// Bootstrapping the RTL/VCL standard library cache. DEFINED NOW so the view
    /// is ready for it; emitted by Task 22 later. `current`/`total`/`detail`
    /// mirror `Indexing`.
    Bootstrapping,
}

/// The owned payload of a `ddk/serverStatus` notification.
///
/// `detail`/`current`/`total` are all optional so a bare state transition (e.g.
/// `Ready`) carries no extra fields, while `Indexing`/`Bootstrapping` fill in the
/// N/M counters and the current-unit `detail`.
#[derive(Debug, Eq, PartialEq, Clone, Deserialize, Serialize)]
pub struct ServerStatusParams {
    pub state: ServerState,
    /// Free-form detail: the file for `Analyzing`, the unit for
    /// `Indexing`/`Bootstrapping`, `None` for a bare `Ready`/`Initializing`.
    pub detail: Option<String>,
    /// The 1-based position in the work list for `Indexing`/`Bootstrapping`.
    pub current: Option<u32>,
    /// The total size of the work list for `Indexing`/`Bootstrapping`.
    pub total: Option<u32>,
}

impl ServerStatusParams {
    /// A bare state with no detail/counters — used for `Initializing` and the
    /// `Ready` transitions that end an operation.
    pub fn bare(state: ServerState) -> Self {
        ServerStatusParams {
            state,
            detail: None,
            current: None,
            total: None,
        }
    }

    /// A state carrying only a `detail` line (no counters) — used for
    /// `Analyzing { detail: filename }`.
    pub fn with_detail(state: ServerState, detail: impl Into<String>) -> Self {
        ServerStatusParams {
            state,
            detail: Some(detail.into()),
            current: None,
            total: None,
        }
    }

    /// A counted state (`current`/`total`) with a `detail` unit — used for
    /// `Indexing`/`Bootstrapping { current, total, detail: unit }`.
    pub fn counted(
        state: ServerState,
        current: u32,
        total: u32,
        detail: impl Into<String>,
    ) -> Self {
        ServerStatusParams {
            state,
            detail: Some(detail.into()),
            current: Some(current),
            total: Some(total),
        }
    }
}

/// The `ddk/serverStatus` notification. A dedicated `ddk/`-namespaced method (not
/// under `notifications/` like the project/compiler notifications) so it reads as
/// server-status telemetry in a protocol trace, matching the `ddk/progress/*`
/// token prefix task 17 uses.
pub enum ServerStatus {}

impl Notification for ServerStatus {
    type Params = ServerStatusParams;
    const METHOD: &'static str = "ddk/serverStatus";
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The payload shape for each state, without a live `Client`. The transport
    /// (send_notification over stdio) needs a live client and is covered by the
    /// live-editor check, noted as unverifiable in a unit test — but the payload
    /// VALUES (state name + which optional fields are populated) are the part that
    /// could be wrong, so they are asserted here per state.
    #[test]
    fn payload_shape_per_state_is_well_formed() {
        // Initializing: bare, no detail/counters.
        let initializing = ServerStatusParams::bare(ServerState::Initializing);
        assert_eq!(initializing.state, ServerState::Initializing);
        assert_eq!(initializing.detail, None);
        assert_eq!(initializing.current, None);
        assert_eq!(initializing.total, None);

        // Ready: bare — the transition every operation ends on.
        let ready = ServerStatusParams::bare(ServerState::Ready);
        assert_eq!(ready.state, ServerState::Ready);
        assert_eq!(ready.detail, None);
        assert!(ready.current.is_none() && ready.total.is_none());

        // Analyzing: detail is the file, no counters.
        let analyzing = ServerStatusParams::with_detail(ServerState::Analyzing, "Unit1.pas");
        assert_eq!(analyzing.state, ServerState::Analyzing);
        assert_eq!(analyzing.detail.as_deref(), Some("Unit1.pas"));
        assert!(analyzing.current.is_none() && analyzing.total.is_none());

        // Indexing: N/M counters + the current unit as detail.
        let indexing = ServerStatusParams::counted(ServerState::Indexing, 340, 1200, "Foo.pas");
        assert_eq!(indexing.state, ServerState::Indexing);
        assert_eq!(indexing.current, Some(340));
        assert_eq!(indexing.total, Some(1200));
        assert_eq!(indexing.detail.as_deref(), Some("Foo.pas"));

        // Bootstrapping: same counted shape (task 22 emits it).
        let bootstrapping =
            ServerStatusParams::counted(ServerState::Bootstrapping, 120, 900, "System.pas");
        assert_eq!(bootstrapping.state, ServerState::Bootstrapping);
        assert_eq!(bootstrapping.current, Some(120));
        assert_eq!(bootstrapping.total, Some(900));
        assert_eq!(bootstrapping.detail.as_deref(), Some("System.pas"));
    }

    /// Each state serializes as its plain variant name, so the extension can
    /// switch on a bare string (`status.state === 'Ready'`). This is the wire
    /// contract the TypeScript handler depends on.
    #[test]
    fn state_serializes_as_its_variant_name() {
        let json = serde_json::to_value(ServerStatusParams::bare(ServerState::Ready)).unwrap();
        assert_eq!(json["state"], "Ready");
        // Absent optional fields serialize as JSON null (serde's default for
        // `Option::None`), which the extension reads as "no detail/counters".
        assert!(json["detail"].is_null());
        assert!(json["current"].is_null());
        assert!(json["total"].is_null());

        let indexing = serde_json::to_value(ServerStatusParams::counted(
            ServerState::Indexing,
            3,
            25,
            "Bar.pas",
        ))
        .unwrap();
        assert_eq!(indexing["state"], "Indexing");
        assert_eq!(indexing["detail"], "Bar.pas");
        assert_eq!(indexing["current"], 3);
        assert_eq!(indexing["total"], 25);
    }

    /// Every state's name round-trips through serde (the extension and server
    /// agree on the five wire names). Guards against a rename drifting the two
    /// sides apart.
    #[test]
    fn all_state_names_round_trip() {
        for (state, name) in [
            (ServerState::Initializing, "Initializing"),
            (ServerState::Ready, "Ready"),
            (ServerState::Analyzing, "Analyzing"),
            (ServerState::Indexing, "Indexing"),
            (ServerState::Bootstrapping, "Bootstrapping"),
        ] {
            let serialized = serde_json::to_value(state).unwrap();
            assert_eq!(serialized, serde_json::json!(name));
            let back: ServerState = serde_json::from_value(serialized).unwrap();
            assert_eq!(back, state);
        }
    }
}
