//! LINE bridge — thClaws-side client for the LINE-OA relay.
//!
//! Architecture (plan-07):
//! 1. User pairs their thClaws install with their LINE OA via the
//!    GUI Line Connect modal. The relay (`thclaws-line-server`,
//!    see workspace-only crate `line-server/`) hands back a
//!    binding JWT which we persist at `~/.config/thclaws/line.json`.
//! 2. On startup (CLI `--line` flag or GUI modal connect) we open
//!    a WebSocket to `wss://line.thclaws.ai/ws?token=<jwt>` and
//!    listen for `WsEnvelope` frames.
//! 3. `UserMessage` envelopes drive a regular `Agent::run_turn`;
//!    the final assistant text is shipped back to LINE via
//!    `POST /reply/<request_id>` on the same server.
//! 4. `Postback` envelopes are Quick Reply taps for the
//!    permission-gating UX (Phase 1.2 — `LineGated` tool gate).
//!
//! This crate-side scope (Phase 1.1):
//! - WebSocket client with reconnect/backoff
//! - Binding-token config loader (`~/.config/thclaws/line.json`)
//! - Output filter that strips intermediate / thinking blocks
//!   and truncates to LINE's 5 000-char ceiling
//! - Wire-protocol shapes that match `line-server`'s `WsEnvelope`
//!
//! Permission gating (`LineGated` mode + ToolGate) and the GUI
//! modal land in Phase 1.2 / 1.3.

pub mod approver;
// `bootstrap` wires the bridge into the GUI worker (`shared_session`)
// — that module is `#[cfg(feature = "gui")]`, so this one is too.
// The CLI binary doesn't have a worker to forward LINE messages
// into; a `--line` headless CLI mode is plan-07 future work.
#[cfg(feature = "gui")]
pub mod bootstrap;
pub mod client;
pub mod config;
pub mod filter;
pub mod protocol;
pub mod session;

pub use approver::{ApprovalReply, LineApprover};
#[cfg(feature = "gui")]
pub use bootstrap::{LineSessionHandle, LineStatus};
pub use client::{LineClient, LineClientError};
pub use config::{LineConfig, LineConfigError};
pub use filter::{clean_for_stream, filter_for_line};
pub use protocol::{WsEnvelope, WsIncoming};
pub use session::LineSession;
