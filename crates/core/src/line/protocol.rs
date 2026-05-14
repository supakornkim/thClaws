//! Wire shapes for the line-server ↔ thClaws-client channel.
//!
//! These structs deserialise the JSON frames the relay pushes
//! over the WebSocket (`WsEnvelope`) and serialise the reply
//! body posted back to `POST /reply/{request_id}` (`ReplyBody`).
//! They MUST stay byte-compatible with `line-server`'s
//! `broker::WsEnvelope`.

use serde::{Deserialize, Serialize};

/// Inbound envelope — what the relay pushes us. `kind`-tagged so
/// future additions (image, voice, etc.) deserialise side-by-side
/// without breaking the existing variants.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WsEnvelope {
    UserMessage {
        text: String,
        reply_token: String,
        request_id: String,
        /// LINE message id (currently empty from server — kept on
        /// the wire so Phase 3 dedup can latch onto it without a
        /// protocol bump).
        #[serde(default)]
        line_msg_id: String,
    },
    /// Quick Reply tap — used for the `LineGated` permission UX
    /// in Phase 1.2. `data` is the developer-set string we put on
    /// the Quick Reply button (e.g. `tool:approve:<id>`).
    Postback { data: String },
    /// Server-pushed notice (paired, kicked, reconnected …) —
    /// shown verbatim in the local thClaws log; not forwarded
    /// to the agent.
    Notice { text: String },
    /// File uploaded from the browser-chat surface via the relay's
    /// `POST /chat/upload`. Bytes are base64 over the broker
    /// channel because the WS frame is text. The desktop decodes,
    /// writes to `<workspace>/uploads/<unique>`, and feeds a
    /// synthesized chat message into the session — project
    /// AGENTS.md / CLAUDE.md instructions steer behavior.
    Upload {
        filename: String,
        content_b64: String,
        #[serde(default)]
        media_type: Option<String>,
        size_bytes: u64,
        request_id: String,
    },
}

/// Container that lets us tag the WS frame variant we received
/// before pattern-matching, useful for logging.
#[derive(Debug)]
pub enum WsIncoming {
    Envelope(WsEnvelope),
    /// Frame was valid JSON but didn't match any known variant.
    Unknown(String),
    /// Server closed the WS cleanly.
    Closed,
}

/// Body of `POST /reply/{request_id}`. The token goes in the
/// `Authorization: Bearer` header, NOT this body.
#[derive(Debug, Clone, Serialize, Default)]
pub struct ReplyBody {
    pub text: String,
    /// Optional Quick Reply buttons. When present, the relay
    /// attaches a `quickReply.items[]` payload to the outbound
    /// text message so the user sees tappable chips instead of
    /// having to type a free-form answer. Phase 1.2.b — currently
    /// used by `LineApprover` to render `[Approve] / [Deny]`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quick_reply: Option<Vec<QuickReplyButton>>,
}

/// One Quick Reply chip. Keep the shape minimal — the relay
/// expands this into the full LINE Messaging API JSON
/// (`type: action`, nested `action: { type: postback, … }`).
///
/// `data` is what the user's tap lands as on the relay's webhook
/// (`postback.data`); see [`super::approver::ApprovalReply::parse_postback`]
/// for the expected `tool:<verb>:<request_id>` shape.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QuickReplyButton {
    /// Chip label shown to the user. ≤ 20 chars per LINE docs.
    pub label: String,
    /// Postback `data` payload the chip carries.
    pub data: String,
    /// Echoed back into the chat as the user's "typed" reply.
    /// Optional — when omitted, LINE shows nothing after the tap.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub display_text: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_message_round_trips() {
        let json = r#"{"kind":"user_message","text":"hi","reply_token":"rt","request_id":"r1","line_msg_id":""}"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        match env {
            WsEnvelope::UserMessage {
                text,
                reply_token,
                request_id,
                ..
            } => {
                assert_eq!(text, "hi");
                assert_eq!(reply_token, "rt");
                assert_eq!(request_id, "r1");
            }
            _ => panic!("expected UserMessage"),
        }
    }

    #[test]
    fn notice_decodes() {
        let json = r#"{"kind":"notice","text":"connected"}"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        assert!(matches!(env, WsEnvelope::Notice { .. }));
    }

    #[test]
    fn postback_decodes() {
        let json = r#"{"kind":"postback","data":"tool:approve:abc"}"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        match env {
            WsEnvelope::Postback { data } => assert_eq!(data, "tool:approve:abc"),
            _ => panic!("expected Postback"),
        }
    }

    #[test]
    fn reply_body_with_quick_reply_serialises() {
        let body = ReplyBody {
            text: "approve?".into(),
            quick_reply: Some(vec![
                QuickReplyButton {
                    label: "Allow".into(),
                    data: "tool:allow:abc".into(),
                    display_text: Some("Allow".into()),
                },
                QuickReplyButton {
                    label: "Deny".into(),
                    data: "tool:deny:abc".into(),
                    display_text: None,
                },
            ]),
        };
        let json = serde_json::to_value(&body).unwrap();
        assert_eq!(json["text"], "approve?");
        let qr = &json["quick_reply"];
        assert!(qr.is_array());
        assert_eq!(qr[0]["label"], "Allow");
        assert_eq!(qr[0]["data"], "tool:allow:abc");
        assert_eq!(qr[0]["display_text"], "Allow");
        // display_text omitted on second button — must not appear.
        assert!(qr[1].get("display_text").is_none());
    }

    #[test]
    fn reply_body_without_quick_reply_omits_field() {
        let body = ReplyBody {
            text: "plain".into(),
            quick_reply: None,
        };
        let json = serde_json::to_string(&body).unwrap();
        // Must NOT serialise the `quick_reply` key when absent —
        // older relays without the field should ignore the body
        // cleanly (and the wire stays small).
        assert!(!json.contains("quick_reply"), "got: {json}");
    }

    #[test]
    fn missing_line_msg_id_defaults_to_empty() {
        // Server omits the field when it's empty — `#[serde(default)]`
        // means we tolerate that without erroring.
        let json = r#"{"kind":"user_message","text":"hi","reply_token":"rt","request_id":"r1"}"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        match env {
            WsEnvelope::UserMessage { line_msg_id, .. } => assert!(line_msg_id.is_empty()),
            _ => panic!("expected UserMessage"),
        }
    }

    #[test]
    fn upload_decodes_from_relay_shape() {
        // Mirror of the JSON `line-server`'s
        // `broker::WsEnvelope::Upload` serializes (snake_case tag,
        // `kind` discriminator). Cross-crate wire compatibility is
        // pinned here — any rename on either side fails this test.
        let json = r#"{
            "kind": "upload",
            "filename": "photo.jpg",
            "content_b64": "AQIDBAU=",
            "media_type": "image/jpeg",
            "size_bytes": 5,
            "request_id": "req-1"
        }"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        match env {
            WsEnvelope::Upload {
                filename,
                content_b64,
                media_type,
                size_bytes,
                request_id,
            } => {
                assert_eq!(filename, "photo.jpg");
                assert_eq!(content_b64, "AQIDBAU=");
                assert_eq!(media_type.as_deref(), Some("image/jpeg"));
                assert_eq!(size_bytes, 5);
                assert_eq!(request_id, "req-1");
            }
            _ => panic!("expected Upload"),
        }
    }

    #[test]
    fn upload_tolerates_missing_media_type() {
        let json = r#"{"kind":"upload","filename":"a.bin","content_b64":"AQ==","size_bytes":1,"request_id":"r"}"#;
        let env: WsEnvelope = serde_json::from_str(json).unwrap();
        match env {
            WsEnvelope::Upload { media_type, .. } => assert!(media_type.is_none()),
            _ => panic!("expected Upload"),
        }
    }
}
