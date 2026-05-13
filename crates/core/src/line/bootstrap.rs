//! Boot a `LineSession` from a stored `LineConfig` and own its
//! lifetime for the worker.
//!
//! Phase 2: replaces the Phase-1.3 placeholder `EchoHandler` with
//! `WorkerForwardHandler`, which routes each inbound LINE message
//! into the worker's `ShellInput::LineMessage` channel. The worker
//! drives `Agent::run_turn`, captures the final assistant text,
//! and answers via a `oneshot::Sender`; this handler returns that
//! captured text so the existing `LineSession::SessionSink` posts
//! the LINE reply unchanged.
//!
//! `LineSessionHandle` is what the worker stashes — it bundles
//! the cancel token (for `/disconnect`) with a status snapshot
//! the IPC layer can render.

use std::sync::mpsc;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::oneshot;

use super::approver::LineApprover;
use super::client::LineClient;
use super::config::LineConfig;
use super::session::{LineMessageHandler, LineSession};
use crate::cancel::CancelToken;

/// Phase 2 handler: forward LINE messages to the worker via the
/// `ShellInput::LineMessage` channel, wait for the captured agent
/// reply, return it so the `LineSession` posts the LINE response.
///
/// On worker channel closure (very unlikely — worker dropped) we
/// return a "thClaws worker unavailable" fallback so the LINE user
/// at least sees *something* instead of dead silence.
struct WorkerForwardHandler {
    input_tx: mpsc::Sender<crate::shared_session::ShellInput>,
}

#[async_trait]
impl LineMessageHandler for WorkerForwardHandler {
    async fn handle_message(&self, text: String) -> Option<String> {
        let (tx, rx) = oneshot::channel();
        // Worker's input channel is `std::sync::mpsc`, which is
        // synchronous + unbounded, so `send()` returns immediately
        // without `.await`. The matching oneshot::Receiver below
        // IS async — that's what makes us wait for the turn.
        if self
            .input_tx
            .send(crate::shared_session::ShellInput::LineMessage { text, respond: tx })
            .is_err()
        {
            return Some("⚠️ thClaws worker is unavailable; restart thClaws and try again.".into());
        }
        match rx.await {
            Ok(s) if !s.trim().is_empty() => Some(s),
            // Empty / dropped sender — agent finished but produced
            // no assistant text (e.g. tool-only turn). Surface a
            // gentle hint rather than absolute silence.
            _ => Some("(thClaws agent finished the turn without a text reply.)".into()),
        }
    }
}

/// Live LINE-bridge handle stored on the worker. Dropping it
/// alone won't cancel the session — fire `cancel.cancel()` first
/// (the IPC `line_disconnect` arm does this).
pub struct LineSessionHandle {
    pub cancel: CancelToken,
    pub status: LineStatus,
    /// JoinHandle so the worker can await graceful shutdown if
    /// it ever needs to. Not surfaced via IPC.
    pub join: tokio::task::JoinHandle<()>,
    /// Shared approver — the agent's `ApprovalSink` swaps to this
    /// while LINE is connected. Same instance the LineSession
    /// holds, so postbacks / text replies resolve the same set
    /// of pending decisions.
    pub approver: Arc<LineApprover>,
    /// Shared HTTP client for relay-bound calls (push, reply,
    /// chat-bridge event fan-out). Exposed so the worker's
    /// LineMessage collector can push `ViewEvent`s to the browser
    /// chat when it's connected (plan-10 Phase 2).
    pub client: Arc<LineClient>,
}

/// Snapshot of the bridge's state. Serialised into the
/// `chat_line_status` IPC payload so the GUI sidebar /
/// LineConnectModal can render an accurate pill.
#[derive(Debug, Clone, serde::Serialize)]
pub struct LineStatus {
    /// `"connected"` once the session task has been spawned.
    /// More granular states (`"connecting"`, `"reconnecting"`)
    /// are Phase 2 work — the WS client logs them internally for
    /// now, but the GUI just sees a boolean.
    pub state: &'static str,
    /// The relay URL the bridge connects to. Always safe to
    /// surface in the UI (no token).
    pub server_url: String,
    /// Number of pending approvals at the time of snapshot.
    /// Lets the sidebar pill flash when LINE is waiting for the
    /// user's tap.
    pub pending_approvals: usize,
}

impl LineStatus {
    pub fn disconnected() -> Self {
        Self {
            state: "disconnected",
            server_url: String::new(),
            pending_approvals: 0,
        }
    }

    pub fn connected(server_url: String) -> Self {
        Self {
            state: "connected",
            server_url,
            pending_approvals: 0,
        }
    }
}

/// Spawn a `LineSession` on the tokio runtime with the worker-
/// forwarding handler + `LineApprover`. Returns the handle so the
/// caller can stash it on `WorkerState` and cancel later.
///
/// `input_tx` is the worker's `ShellInput` channel sender — every
/// inbound LINE message arrives here as
/// `ShellInput::LineMessage`, runs through the agent loop, and
/// the captured assistant text is shipped back over LINE.
pub fn spawn(
    config: LineConfig,
    input_tx: mpsc::Sender<crate::shared_session::ShellInput>,
) -> LineSessionHandle {
    let cancel = CancelToken::new();
    let server_url = config.resolved_server_url();
    let handler: Arc<dyn LineMessageHandler> = Arc::new(WorkerForwardHandler { input_tx });

    // Build the client + approver up front so they share the
    // same configured server URL (and so the approver doesn't
    // need to know how to construct a client).
    let client = Arc::new(LineClient::new(config.clone()).with_cancel(cancel.clone()));
    let approver = Arc::new(LineApprover::new(client.clone()));

    let session = Arc::new(
        LineSession::new(config, handler)
            .with_approver(approver.clone())
            .with_cancel(cancel.clone()),
    );
    let cancel_for_task = cancel.clone();
    let join = tokio::spawn(async move {
        if let Err(e) = session.run().await {
            eprintln!("[line] session ended: {e}");
        }
        // The cancel token may have been fired externally
        // (line_disconnect) — fire it again is a no-op but keeps
        // the rest of the worker from waiting on the join.
        cancel_for_task.cancel();
    });

    LineSessionHandle {
        cancel,
        status: LineStatus::connected(server_url),
        join,
        approver,
        client,
    }
}
