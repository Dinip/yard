//! The session and artifact planes, end to end and in-process.
//!
//! Runs the real axum server against the real mock backend, with a locally
//! generated Ed25519 key served as a JWKS by a throwaway server — so the whole
//! browser-facing surface is exercised without a coordinator, a database, or a
//! device.
//!
//! Most of what is asserted here is a security boundary: an unauthorized
//! request must be refused, and a revoked one must stop working *immediately*
//! rather than when its token happens to expire.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use axum::Router;
use base64::Engine as _;
use farm_protocol::Platform;
use jsonwebtoken::{Algorithm, EncodingKey, Header};
use provider_core::auth::TokenVerifier;
use provider_core::config::Config;
use provider_core::server::{router, ServerState};
use provider_core::session::{Authorization, SessionRegistry};
use provider_core::supervisor::Supervisor;
use serde::Serialize;

const PROVIDER_ID: &str = "test-provider";
const DEVICE_ID: &str = "mock-ios-1";
const OTHER_DEVICE: &str = "mock-android-1";
const RESERVATION: &str = "res-1";

#[derive(Serialize)]
struct Claims<'a> {
    iss: &'a str,
    sub: &'a str,
    aud: &'a str,
    exp: i64,
    iat: i64,
    #[serde(rename = "deviceId")]
    device_id: &'a str,
    #[serde(rename = "userId")]
    user_id: &'a str,
    #[serde(rename = "reservationId")]
    reservation_id: &'a str,
    #[serde(rename = "providerId")]
    provider_id: &'a str,
}

/// An Ed25519 keypair plus the JWKS a provider would fetch for it.
struct Signer {
    encoding: EncodingKey,
    jwks: String,
}

impl Signer {
    fn new() -> Self {
        use ed25519_dalek::SigningKey;
        use rand::RngExt as _;

        let seed: [u8; 32] = rand::rng().random();
        let signing = SigningKey::from_bytes(&seed);

        // PKCS#8 v1 for Ed25519 is a fixed 16-byte prefix followed by the seed.
        // Built by hand rather than via a der encoder: it is a constant, and it
        // keeps this test off an API that churns between dalek releases.
        const PKCS8_PREFIX: [u8; 16] = [
            0x30, 0x2e, 0x02, 0x01, 0x00, 0x30, 0x05, 0x06, 0x03, 0x2b, 0x65, 0x70, 0x04, 0x22,
            0x04, 0x20,
        ];
        let mut pkcs8 = Vec::with_capacity(48);
        pkcs8.extend_from_slice(&PKCS8_PREFIX);
        pkcs8.extend_from_slice(&seed);
        let encoding = EncodingKey::from_ed_der(&pkcs8);

        let x = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(signing.verifying_key().to_bytes());
        let jwks = format!(
            r#"{{"keys":[{{"kty":"OKP","crv":"Ed25519","alg":"EdDSA","use":"sig","kid":"test-key","x":"{x}"}}]}}"#
        );

        Self { encoding, jwks }
    }

    fn token(&self, issuer: &str, device_id: &str, reservation_id: &str, ttl_secs: i64) -> String {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let mut header = Header::new(Algorithm::EdDSA);
        header.kid = Some("test-key".into());

        jsonwebtoken::encode(
            &header,
            &Claims {
                iss: issuer,
                sub: "user-1",
                aud: farm_protocol::SESSION_TOKEN_AUDIENCE,
                exp: now + ttl_secs,
                iat: now,
                device_id,
                user_id: "user-1",
                reservation_id,
                provider_id: PROVIDER_ID,
            },
            &self.encoding,
        )
        .expect("signing a test token")
    }
}

struct Harness {
    base: String,
    sessions: SessionRegistry,
    signer: Signer,
    issuer: String,
    scratch: std::path::PathBuf,
}

async fn start() -> Harness {
    let signer = Signer::new();

    // Throwaway JWKS server, standing in for the coordinator.
    let jwks_body = signer.jwks.clone();
    let jwks_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}", jwks_listener.local_addr().unwrap());
    let jwks_app = Router::new().route(
        farm_protocol::JWKS_PATH,
        axum::routing::get(move || {
            let body = jwks_body.clone();
            async move {
                (
                    [(axum::http::header::CONTENT_TYPE, "application/json")],
                    body,
                )
            }
        }),
    );
    tokio::spawn(async move { axum::serve(jwks_listener, jwks_app).await });

    // A counter, not a timestamp: these tests run concurrently in one process,
    // and two harnesses landing in the same nanosecond would share a scratch
    // directory — so one test would see another's in-flight upload and the
    // "nothing was left behind" assertions would fail at random.
    static NEXT_SCRATCH: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let scratch = std::env::temp_dir().join(format!(
        "farm-session-test-{}-{}",
        std::process::id(),
        NEXT_SCRATCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    tokio::fs::create_dir_all(&scratch).await.unwrap();

    let config: Config = serde_yaml_ng::from_str(&format!(
        r#"
id: {PROVIDER_ID}
name: test
coordinator_url: {issuer}
public_base_url: http://localhost:7100
token: pft_test
scratch_dir: {}
"#,
        scratch.display()
    ))
    .unwrap();
    let config = Arc::new(config);

    let sessions = SessionRegistry::new();
    let mut supervisor = Supervisor::new(sessions.clone());
    supervisor.add(
        DEVICE_ID.into(),
        backend_mock::MockBackend::new(DEVICE_ID, Platform::Ios, "Mock iPhone"),
    );
    supervisor.add(
        OTHER_DEVICE.into(),
        backend_mock::MockBackend::new(OTHER_DEVICE, Platform::Android, "Mock Pixel"),
    );
    let supervisor = Arc::new(supervisor);
    supervisor.bootstrap().await;

    let verifier = Arc::new(TokenVerifier::new(
        format!("{issuer}{}", farm_protocol::JWKS_PATH),
        PROVIDER_ID.into(),
        issuer.clone(),
    ));
    verifier.refresh().await.expect("fetching the test JWKS");
    verifier.self_test().await.expect("verifier self-test");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let state = ServerState {
        config,
        supervisor,
        verifier,
    };
    tokio::spawn(async move { axum::serve(listener, router(state)).await });

    Harness {
        base,
        sessions,
        signer,
        issuer,
        scratch,
    }
}

impl Harness {
    fn token(&self) -> String {
        self.signer.token(&self.issuer, DEVICE_ID, RESERVATION, 60)
    }

    async fn authorize(&self) {
        self.sessions
            .authorize(
                DEVICE_ID,
                Authorization {
                    reservation_id: RESERVATION.into(),
                    user_id: "user-1".into(),
                },
            )
            .await;
    }

    async fn get(&self, path: &str) -> reqwest::Response {
        reqwest::get(format!("{}{path}", self.base)).await.unwrap()
    }

    fn scratch_files(&self) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(&self.scratch)
            .map(|dir| dir.filter_map(|e| e.ok().map(|e| e.path())).collect())
            .unwrap_or_default()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.scratch);
    }
}

#[tokio::test]
async fn health_needs_no_token() {
    let h = start().await;
    assert_eq!(h.get("/health").await.status(), 200);
}

#[tokio::test]
async fn a_garbage_token_is_rejected() {
    let h = start().await;
    h.authorize().await;

    let res = h
        .get(&format!("/s/{DEVICE_ID}/screenshot.png?token=not.a.jwt"))
        .await;
    assert_eq!(res.status(), 401);
}

/// Regression: `jsonwebtoken` defaults to 60s of leeway, which silently doubled
/// the lifetime of a ~60s session token — an expired token kept working for
/// another full minute. The verifier now allows only clock-skew tolerance.
#[tokio::test]
async fn an_expired_token_is_rejected() {
    let h = start().await;
    h.authorize().await;

    for expired_by in [-30i64, -60, -300] {
        let token = h
            .signer
            .token(&h.issuer, DEVICE_ID, RESERVATION, expired_by);
        let res = h
            .get(&format!("/s/{DEVICE_ID}/screenshot.png?token={token}"))
            .await;
        assert_eq!(
            res.status(),
            401,
            "a token that expired {}s ago was accepted",
            -expired_by
        );
    }
}

/// The flip side: a little clock skew must not lock users out.
#[tokio::test]
async fn a_token_just_inside_the_skew_allowance_still_works() {
    let h = start().await;
    h.authorize().await;

    let token = h.signer.token(&h.issuer, DEVICE_ID, RESERVATION, -2);
    let res = h
        .get(&format!("/s/{DEVICE_ID}/screenshot.png?token={token}"))
        .await;
    assert_eq!(res.status(), 200);
}

/// A token signed by a *different* key must not be accepted — this is the check
/// that would fail if verification were ever reduced to decoding the payload.
#[tokio::test]
async fn a_token_signed_by_another_key_is_rejected() {
    let h = start().await;
    h.authorize().await;

    let impostor = Signer::new();
    let forged = impostor.token(&h.issuer, DEVICE_ID, RESERVATION, 60);
    let res = h
        .get(&format!("/s/{DEVICE_ID}/screenshot.png?token={forged}"))
        .await;
    assert_eq!(res.status(), 401);
}

#[tokio::test]
async fn a_valid_token_for_another_device_cannot_reach_this_one() {
    let h = start().await;
    h.authorize().await;

    let res = h
        .get(&format!(
            "/s/{OTHER_DEVICE}/screenshot.png?token={}",
            h.token()
        ))
        .await;
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn an_unreserved_device_refuses_a_perfectly_valid_token() {
    let h = start().await;
    // Deliberately not authorized: the signature and expiry are fine, but the
    // coordinator never said this reservation may use the device.
    let res = h
        .get(&format!(
            "/s/{DEVICE_ID}/screenshot.png?token={}",
            h.token()
        ))
        .await;
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn a_stale_reservation_is_refused_even_though_the_token_is_valid() {
    let h = start().await;
    h.authorize().await;

    let stale = h.signer.token(&h.issuer, DEVICE_ID, "res-previous", 60);
    let res = h
        .get(&format!("/s/{DEVICE_ID}/screenshot.png?token={stale}"))
        .await;
    assert_eq!(res.status(), 403);
}

#[tokio::test]
async fn revocation_takes_effect_immediately_not_at_token_expiry() {
    let h = start().await;
    h.authorize().await;
    let token = h.token();

    assert_eq!(
        h.get(&format!("/s/{DEVICE_ID}/screenshot.png?token={token}"))
            .await
            .status(),
        200
    );

    h.sessions.revoke(DEVICE_ID, "reservation released").await;

    // Same token, still signed, still unexpired — and now useless.
    assert_eq!(
        h.get(&format!("/s/{DEVICE_ID}/screenshot.png?token={token}"))
            .await
            .status(),
        403
    );
}

#[tokio::test]
async fn screenshot_returns_a_real_png() {
    let h = start().await;
    h.authorize().await;

    let res = h
        .get(&format!(
            "/s/{DEVICE_ID}/screenshot.png?token={}",
            h.token()
        ))
        .await;
    assert_eq!(res.status(), 200);
    assert_eq!(res.headers()["content-type"], "image/png");

    let body = res.bytes().await.unwrap();
    assert_eq!(&body[..4], b"\x89PNG");
}

#[tokio::test]
async fn upload_installs_then_deletes_the_staged_file() {
    let h = start().await;
    h.authorize().await;

    let payload = vec![7u8; 512 * 1024];
    let res = reqwest::Client::new()
        .post(format!(
            "{}/s/{DEVICE_ID}/install?token={}",
            h.base,
            h.token()
        ))
        .header("x-farm-filename", "app-release.apk")
        .body(payload)
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["ok"], true);
    assert_eq!(body["size"], 512 * 1024);
    assert_eq!(body["filename"], "app-release.apk");

    // Nothing is stored server-side; a leaked staged file on a tmpfs scratch
    // dir is how a provider host runs out of space.
    assert!(
        h.scratch_files().is_empty(),
        "staged upload was left behind: {:?}",
        h.scratch_files()
    );
}

#[tokio::test]
async fn a_hostile_upload_filename_cannot_escape_the_scratch_directory() {
    let h = start().await;
    h.authorize().await;

    let res = reqwest::Client::new()
        .post(format!(
            "{}/s/{DEVICE_ID}/install?token={}",
            h.base,
            h.token()
        ))
        .header("x-farm-filename", "../../../../tmp/pwned.apk")
        .body(vec![0u8; 1024])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 200);
    let body: serde_json::Value = res.json().await.unwrap();
    assert_eq!(body["filename"], "pwned.apk");
    assert!(h.scratch_files().is_empty());
}

#[tokio::test]
async fn an_unauthorized_upload_is_refused_before_anything_is_written() {
    let h = start().await;
    // No authorization at all.
    let res = reqwest::Client::new()
        .post(format!(
            "{}/s/{DEVICE_ID}/install?token={}",
            h.base,
            h.token()
        ))
        .body(vec![0u8; 1024])
        .send()
        .await
        .unwrap();

    assert_eq!(res.status(), 403);
    assert!(h.scratch_files().is_empty());
}

#[tokio::test]
async fn the_session_socket_hands_over_a_codec_then_streams_frames() {
    use futures::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;

    let h = start().await;
    h.authorize().await;

    let url = format!(
        "{}/s/{DEVICE_ID}?token={}",
        h.base.replace("http://", "ws://"),
        h.token()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    // First frame is always the handshake — a viewer cannot decode without it.
    let first = socket.next().await.unwrap().unwrap();
    let Message::Text(text) = first else {
        panic!("expected the codec handshake first, got {first:?}");
    };
    let handshake: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(handshake["type"], "codec");
    assert_eq!(handshake["codec"], "hev1.1.6.L93.B0");
    assert_eq!(handshake["display"]["width"], 1179);
    assert!(handshake["description"].is_string());

    let mut keys = 0;
    let mut deltas = 0;
    for _ in 0..12 {
        match tokio::time::timeout(Duration::from_secs(5), socket.next())
            .await
            .expect("timed out waiting for a frame")
            .unwrap()
            .unwrap()
        {
            Message::Binary(bytes) => match bytes[0] {
                farm_protocol::AU_KEY | farm_protocol::AU_KEY_RESET => keys += 1,
                farm_protocol::AU_DELTA => deltas += 1,
                other => panic!("unknown access-unit type byte {other}"),
            },
            Message::Ping(_) | Message::Text(_) => {}
            other => panic!("unexpected frame {other:?}"),
        }
    }

    // The first thing a viewer receives must be decodable on its own.
    assert!(keys >= 1, "no keyframe arrived");
    assert!(deltas >= 1, "no delta frames arrived");
}

#[tokio::test]
async fn revoking_closes_a_live_socket_rather_than_leaving_it_streaming() {
    use futures::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;

    let h = start().await;
    h.authorize().await;

    let url = format!(
        "{}/s/{DEVICE_ID}?token={}",
        h.base.replace("http://", "ws://"),
        h.token()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _handshake = socket.next().await.unwrap().unwrap();

    h.sessions.revoke(DEVICE_ID, "reservation released").await;

    // The viewer must be told, promptly — not left watching a device someone
    // else has since been handed.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut closed = false;
    while tokio::time::Instant::now() < deadline {
        let Ok(Some(Ok(frame))) = tokio::time::timeout(Duration::from_secs(5), socket.next()).await
        else {
            closed = true;
            break;
        };
        if let Message::Text(text) = frame {
            let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
            if msg["type"] == "session.closed" {
                assert_eq!(msg["reason"], "reservation released");
                closed = true;
                break;
            }
        }
    }
    assert!(closed, "the socket kept streaming after revocation");
}

#[tokio::test]
async fn input_reaches_the_backend_and_clipboard_round_trips() {
    use futures::{SinkExt as _, StreamExt as _};
    use tokio_tungstenite::tungstenite::Message;

    let h = start().await;
    h.authorize().await;

    let url = format!(
        "{}/s/{DEVICE_ID}?token={}",
        h.base.replace("http://", "ws://"),
        h.token()
    );
    let (mut socket, _) = tokio_tungstenite::connect_async(url).await.unwrap();
    let _handshake = socket.next().await.unwrap().unwrap();

    for message in [
        r#"{"type":"pointer.down","pointerId":0,"at":{"x":0.5,"y":0.25}}"#,
        r#"{"type":"pointer.move","pointerId":0,"at":{"x":0.5,"y":0.75}}"#,
        r#"{"type":"pointer.up","pointerId":0,"at":{"x":0.5,"y":0.75}}"#,
        r#"{"type":"clipboard.set","text":"copied"}"#,
    ] {
        socket.send(Message::Text(message.into())).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    socket
        .send(Message::Text(r#"{"type":"clipboard.get"}"#.into()))
        .await
        .unwrap();

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut clipboard = None;
    while tokio::time::Instant::now() < deadline && clipboard.is_none() {
        if let Ok(Some(Ok(Message::Text(text)))) =
            tokio::time::timeout(Duration::from_secs(5), socket.next()).await
        {
            let msg: serde_json::Value = serde_json::from_str(&text).unwrap();
            if msg["type"] == "clipboard" {
                clipboard = msg["text"].as_str().map(str::to_owned);
            }
        }
    }

    assert_eq!(clipboard.as_deref(), Some("copied"));
}

/// The popout window and the parent tab share one reservation, so both must be
/// able to hold a socket at once.
#[tokio::test]
async fn two_viewers_share_one_reservation() {
    use futures::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;

    let h = start().await;
    h.authorize().await;

    let url = format!(
        "{}/s/{DEVICE_ID}?token={}",
        h.base.replace("http://", "ws://"),
        h.token()
    );
    let (mut first, _) = tokio_tungstenite::connect_async(url.clone()).await.unwrap();
    let (mut second, _) = tokio_tungstenite::connect_async(url).await.unwrap();

    for socket in [&mut first, &mut second] {
        let frame = socket.next().await.unwrap().unwrap();
        let Message::Text(text) = frame else {
            panic!("expected a codec handshake");
        };
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&text).unwrap()["type"],
            "codec"
        );
    }

    // Both must actually receive video, not just connect.
    for socket in [&mut first, &mut second] {
        let mut saw_frame = false;
        for _ in 0..10 {
            if let Ok(Some(Ok(Message::Binary(_)))) =
                tokio::time::timeout(Duration::from_secs(5), socket.next()).await
            {
                saw_frame = true;
                break;
            }
        }
        assert!(saw_frame, "a viewer connected but received no video");
    }
}
