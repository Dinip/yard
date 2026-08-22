//! Wire contract shared with the coordinator.
//!
//! Everything in [`generated`] is emitted from the zod schemas in
//! `packages/protocol/src` by `bun run protocol:gen`. **Do not edit it.** CI
//! regenerates and diffs, so a hand-edit will fail the build — which is the
//! point: a wire-contract change must not be able to reach a provider as a
//! silent no-op.
//!
//! Hand-written helpers that are *about* the protocol but not *part* of it
//! belong here in `lib.rs`, not in the generated file.

mod generated;

pub use generated::*;

/// Frames the session plane sends to a browser viewer.
///
/// Text frames are [`ServerMessage`]; binary frames are a single type byte
/// followed by one access unit. Kept as a helper rather than generated because
/// it describes the *framing*, which has no zod counterpart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuKind {
    /// Keyframe. Decodable on its own.
    Key,
    /// Delta frame. Requires the preceding key.
    Delta,
    /// Keyframe that additionally invalidates the decoder: the viewer must
    /// rebuild `VideoDecoder` before feeding it. Emitted when the codec
    /// description changes mid-stream, e.g. after a rotation.
    KeyWithReset,
}

impl AuKind {
    pub fn to_byte(self) -> u8 {
        match self {
            AuKind::Key => AU_KEY,
            AuKind::Delta => AU_DELTA,
            AuKind::KeyWithReset => AU_KEY_RESET,
        }
    }

    pub fn from_byte(b: u8) -> Option<Self> {
        match b {
            AU_KEY => Some(AuKind::Key),
            AU_DELTA => Some(AuKind::Delta),
            AU_KEY_RESET => Some(AuKind::KeyWithReset),
            _ => None,
        }
    }

    pub fn is_key(self) -> bool {
        matches!(self, AuKind::Key | AuKind::KeyWithReset)
    }
}

/// Prefixes an access unit with its type byte, ready to send as a binary frame.
pub fn frame_au(kind: AuKind, au: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(au.len() + 1);
    out.push(kind.to_byte());
    out.extend_from_slice(au);
    out
}

impl ClientMessage {
    /// Whether this message is a person driving the device, as opposed to the
    /// viewer keeping itself alive.
    ///
    /// `keyframe` and `pong` are the stream asking for what it needs; they say
    /// nothing about whether anyone is at the keyboard, and counting them would
    /// make an idle timeout unreachable for as long as a tab stays open.
    pub fn is_interaction(&self) -> bool {
        !matches!(self, ClientMessage::Keyframe | ClientMessage::Pong { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn au_kind_round_trips() {
        for kind in [AuKind::Key, AuKind::Delta, AuKind::KeyWithReset] {
            assert_eq!(AuKind::from_byte(kind.to_byte()), Some(kind));
        }
        assert_eq!(AuKind::from_byte(7), None);
    }

    #[test]
    fn framing_prefixes_the_type_byte() {
        assert_eq!(
            frame_au(AuKind::Delta, &[0xaa, 0xbb]),
            vec![AU_DELTA, 0xaa, 0xbb]
        );
    }

    /// The generated enums must produce exactly the JSON the coordinator's zod
    /// schemas parse — internally tagged, camelCase fields, dotted tag values.
    #[test]
    fn command_payload_matches_the_json_shape() {
        let cmd = CommandPayload::SessionAuthorize {
            device_id: "udid-1".into(),
            reservation_id: "res-1".into(),
            user_id: "user-1".into(),
            adb_keys: vec![AdbKey {
                user_id: "user-1".into(),
                fingerprint: "SHA256:abc".into(),
                public_key: "QAAAAO0l96Y=".into(),
                comment: Some("dev@example.test".into()),
            }],
        };
        let json = serde_json::to_value(&cmd).unwrap();
        assert_eq!(json["kind"], "session.authorize");
        assert_eq!(json["adbKeys"][0]["publicKey"], "QAAAAO0l96Y=");
        assert_eq!(json["deviceId"], "udid-1");
        assert_eq!(json["reservationId"], "res-1");

        let back: CommandPayload = serde_json::from_value(json).unwrap();
        assert_eq!(back, cmd);
    }

    #[test]
    fn optional_fields_are_omitted_not_nulled() {
        let msg = ProviderMessage::DeviceStatus {
            device_id: "udid-1".into(),
            status: DeviceStatus::Ready,
            note: None,
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(
            !json.contains("note"),
            "absent optionals must not serialize: {json}"
        );
        assert!(json.contains(r#""type":"device.status""#));
    }

    #[test]
    fn unknown_optional_fields_deserialize_from_a_minimal_snapshot() {
        let json = r#"{"id":"serial-1","platform":"android","status":"ready"}"#;
        let snap: DeviceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(snap.id, "serial-1");
        assert_eq!(snap.platform, Platform::Android);
        assert!(snap.display.is_none());
    }
}
