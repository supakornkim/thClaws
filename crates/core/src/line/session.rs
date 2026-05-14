//! Bridge between the LINE WS client and the agent loop.
//!
//! `LineSession` is what `gui.rs` / `repl.rs` spawn when a binding
//! token is present. It owns:
//! - A `LineClient` for WS + reply API
//! - A shared `Agent` to run turns against
//! - The current `Session` (shared with the rest of thClaws so
//!   LINE-driven turns appear in the user's normal chat history)
//!
//! Phase 1.1 scope: simplest possible relay — when a
//! `UserMessage` envelope arrives, push it as a user turn into
//! the shared session via the `LineMessageHandler` trait. The
//! caller controls the agent / permission posture (which is why
//! we don't ship a full implementation here yet — `gui.rs` and
//! `repl.rs` will provide concrete impls in Phase 1.2/1.3).
//!
//! Phase 1.2 will add a built-in `ToolGate` that suspends turns
//! on mutating tool calls and round-trips a Quick Reply.

use std::sync::Arc;

use async_trait::async_trait;

use super::approver::{ApprovalReply, LineApprover};
use super::client::{LineClient, LineClientError, LineEnvelopeSink};
use super::config::LineConfig;
use super::filter::filter_for_line;
use super::protocol::WsEnvelope;

/// Pluggable handler — what to do when a LINE user message
/// arrives. Implementations live in `gui.rs` (drives the shared
/// session + GUI broadcasts) and `repl.rs` (drives the standalone
/// LINE-only agent loop).
#[async_trait]
pub trait LineMessageHandler: Send + Sync + 'static {
    /// Called once per inbound user text. Implementer drives the
    /// agent and returns the final assistant text. `None` skips
    /// the LINE reply (e.g. recognised a `/help` command and
    /// handled it inline).
    async fn handle_message(&self, text: String) -> Option<String>;

    /// Called for Quick-Reply postbacks (Phase 1.2 permission
    /// gate). Default no-op so Phase 1.1 implementations don't
    /// have to override.
    async fn handle_postback(&self, _data: String) {}
}

pub struct LineSession {
    client: Arc<LineClient>,
    handler: Arc<dyn LineMessageHandler>,
    /// When `Some`, inbound text + postbacks are routed to the
    /// approver first; an approval reply short-circuits the agent
    /// turn. `None` when the worker isn't running in
    /// `PermissionMode::LineGated` — the session falls back to
    /// the plain handler-only flow used for Phase 1.1 smoke
    /// testing.
    approver: Option<Arc<LineApprover>>,
}

impl LineSession {
    pub fn new(config: LineConfig, handler: Arc<dyn LineMessageHandler>) -> Self {
        Self {
            client: Arc::new(LineClient::new(config)),
            handler,
            approver: None,
        }
    }

    /// Attach a `LineApprover` so inbound text / postbacks can
    /// resolve pending tool-approval prompts.
    pub fn with_approver(mut self, approver: Arc<LineApprover>) -> Self {
        self.approver = Some(approver);
        self
    }

    pub fn with_cancel(mut self, token: crate::cancel::CancelToken) -> Self {
        // Replace the Arc'd client with one carrying the cancel
        // token. Cheap — only one client per session.
        let client = Arc::try_unwrap(self.client)
            .map(|c| c.with_cancel(token.clone()))
            .unwrap_or_else(|arc| {
                let cfg = LineConfig::default();
                // Should never hit this branch — `new` is the
                // only constructor — but fail-safe rather than
                // unwrap-panic.
                let _ = arc;
                LineClient::new(cfg).with_cancel(token)
            });
        self.client = Arc::new(client);
        self
    }

    /// Drive the WS loop forever. Returns only on cancellation or
    /// a permanent error (rare — reconnect handles transient).
    pub async fn run(self: Arc<Self>) -> Result<(), LineClientError> {
        let workspace = std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
        let sink = SessionSink {
            client: self.client.clone(),
            handler: self.handler.clone(),
            approver: self.approver.clone(),
            workspace: Arc::new(workspace),
        };
        self.client.run(sink).await
    }
}

struct SessionSink {
    client: Arc<LineClient>,
    handler: Arc<dyn LineMessageHandler>,
    approver: Option<Arc<LineApprover>>,
    workspace: Arc<std::path::PathBuf>,
}

#[async_trait]
impl LineEnvelopeSink for SessionSink {
    async fn on_envelope(&self, envelope: WsEnvelope) {
        match envelope {
            WsEnvelope::UserMessage {
                text, request_id, ..
            } => {
                eprintln!(
                    "[line] user message ({} chars, request_id={})",
                    text.chars().count(),
                    request_id
                );
                // C2 fix: NEVER block this method on the agent
                // turn. The WS recv loop in `LineClient` awaits
                // `on_envelope` in line, so an agent turn that
                // pauses on an approval prompt would deadlock the
                // delivery of the very Postback that would resolve
                // it. Each `UserMessage` spawns a detached task —
                // the worker channel still serialises turns at the
                // ShellInput layer, so concurrent agent execution
                // isn't a concern.
                //
                // The approval-text short-circuit runs synchronously
                // BEFORE the spawn so a `record_decision_from_text`
                // race doesn't leak into a half-spawned turn — but
                // its outbound `send_reply` confirmation also gets
                // spawned to keep `on_envelope` non-blocking.

                if let Some(approver) = &self.approver {
                    if approver.has_pending() {
                        match approver.record_decision_from_text(&text) {
                            Some(reply_kind) => {
                                let msg = match reply_kind {
                                    ApprovalReply::Allow => "✅ Approved — running tool now.",
                                    ApprovalReply::Deny => {
                                        "🚫 Denied — agent will not run the tool."
                                    }
                                    ApprovalReply::Unrecognised => {
                                        "I didn't catch that. Please reply 'approve' or 'deny'."
                                    }
                                };
                                let client = self.client.clone();
                                let request_id = request_id.clone();
                                let msg = msg.to_string();
                                tokio::spawn(async move {
                                    if let Err(e) = client.send_reply(&request_id, msg).await {
                                        eprintln!("[line] approval confirm reply failed: {e}");
                                    }
                                });
                                return;
                            }
                            None => {
                                // Race — pending was cleared
                                // between has_pending() and the
                                // resolve attempt. Fall through
                                // to the normal handler path.
                            }
                        }
                    }
                }

                let handler = self.handler.clone();
                let client = self.client.clone();
                tokio::spawn(async move {
                    if let Some(reply) = handler.handle_message(text).await {
                        let body = filter_for_line(&reply);
                        if let Err(e) = client.send_reply(&request_id, body).await {
                            eprintln!("[line] reply failed (request_id={}): {}", request_id, e);
                        }
                    }
                });
            }
            WsEnvelope::Postback { data } => {
                eprintln!("[line] postback: {data}");
                // Postback resolution is sync (just resolves a
                // oneshot via `record_decision_from_postback`), so
                // we don't need to spawn here. Returning quickly
                // is critical: this is the path that UNBLOCKS the
                // approval-waiting agent turn.
                if let Some(approver) = &self.approver {
                    if approver.record_decision_from_postback(&data).is_some() {
                        return;
                    }
                }
                // No pending approval matched — give the handler a
                // chance. Default impl is a no-op; spawn so an
                // implementer's async work can't reintroduce the
                // deadlock either.
                let handler = self.handler.clone();
                tokio::spawn(async move {
                    handler.handle_postback(data).await;
                });
            }
            WsEnvelope::Notice { text } => {
                // Surface as a regular eprintln — Phase 1.3 GUI
                // will also drop a side-bubble on Notice.
                eprintln!("[line] notice: {text}");
            }
            WsEnvelope::Upload {
                filename,
                content_b64,
                media_type,
                size_bytes,
                request_id,
            } => {
                eprintln!(
                    "[line] upload: {} ({} bytes, media_type={:?})",
                    filename, size_bytes, media_type
                );
                let saved = match crate::line::save_upload(
                    &self.workspace,
                    &filename,
                    &content_b64,
                    size_bytes,
                    media_type,
                ) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[line] upload save failed: {e}");
                        return;
                    }
                };
                let synth = crate::uploads::render_upload_message("line", &[saved]);
                let handler = self.handler.clone();
                let client = self.client.clone();
                tokio::spawn(async move {
                    if let Some(reply) = handler.handle_message(synth).await {
                        let body = filter_for_line(&reply);
                        if let Err(e) = client.send_reply(&request_id, body).await {
                            eprintln!(
                                "[line] upload reply failed (request_id={}): {}",
                                request_id, e
                            );
                        }
                    }
                });
            }
        }
    }
}
