//! Session and artifact planes: browser ↔ provider, directly.
//!
//! The coordinator is not on this path. It signs a token and hands the browser
//! this provider's URL; everything after that — video, input, uploads,
//! screenshots — is between the browser and this process.
//!
//! Ported in spirit from `stf-ios-provider/src/screen_ws.rs`, whose per-viewer
//! fanout and backlog shedding are reproduced here on top of `VideoHandle`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context as _, Result};
use axum::body::Body;
use axum::extract::ws::{Message, WebSocket};
use axum::extract::{DefaultBodyLimit, Path, Query, State, WebSocketUpgrade};
use axum::http::{header, HeaderName, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine as _;
use farm_protocol::{frame_au, AuKind, ClientMessage, Display, ServerMessage};
use futures::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};

use tower_http::cors::{AllowOrigin, CorsLayer};

use crate::auth::TokenVerifier;
use crate::backend::{InputEvent, ProgressSink};
use crate::config::Config;
use crate::origins::WebOrigins;
use crate::supervisor::Supervisor;
use crate::video::VideoGeometry;

/// How long a fresh viewer waits for the codec handshake before we give up.
/// Bring-up is legitimately slow — tunnel, encoder spin-up, a locked screen.
const CODEC_TIMEOUT: Duration = Duration::from_secs(30);

/// Keep the socket warm from this side: browsers do not send pings and
/// intermediate proxies reap idle sockets.
const PING_INTERVAL: Duration = Duration::from_secs(20);

#[derive(Clone)]
pub struct ServerState {
    pub config: Arc<Config>,
    pub supervisor: Arc<Supervisor>,
    pub verifier: Arc<TokenVerifier>,
    /// Filled in from `hello.ack`; see [`crate::origins`].
    pub web_origins: WebOrigins,
}

#[derive(Debug, Deserialize)]
pub struct TokenQuery {
    token: String,
}

#[derive(Debug, Deserialize)]
pub struct FileQuery {
    token: String,
    /// Absolute on the device. Absent means "wherever this backend opens",
    /// so the browser never has to know what a device's root is called.
    path: Option<String>,
}

pub fn router(state: ServerState) -> Router {
    let limit = state.config.max_upload_bytes() as usize;

    Router::new()
        .route("/health", get(health))
        .route("/s/{device_id}", get(session_ws))
        .route("/s/{device_id}/screenshot.png", get(screenshot))
        .route("/s/{device_id}/mjpeg", get(mjpeg))
        .route("/s/{device_id}/files", get(list_files))
        .route("/s/{device_id}/file", get(pull_file))
        .route(
            "/s/{device_id}/install",
            post(install).layer(DefaultBodyLimit::max(limit)),
        )
        .layer(cors(state.web_origins.clone()))
        .with_state(state)
}

/// Browser access to the artifact plane.
///
/// The origin list is checked per request rather than baked into the layer,
/// because the server starts listening before the provider has registered and
/// learned which origins the coordinator allows.
///
/// Credentials are deliberately *not* allowed: authorization on this plane is
/// the session token in the query string, never a cookie, so there is nothing
/// ambient for a hostile page to ride on.
fn cors(origins: WebOrigins) -> CorsLayer {
    CorsLayer::new()
        .allow_origin(AllowOrigin::predicate(move |origin, _| {
            origin
                .to_str()
                .map(|origin| origins.allows(origin))
                .unwrap_or(false)
        }))
        .allow_methods([Method::GET, Method::POST])
        .allow_headers([
            header::CONTENT_TYPE,
            HeaderName::from_static("x-farm-filename"),
        ])
}

pub async fn serve(state: ServerState) -> Result<()> {
    let addr: SocketAddr = state
        .config
        .bind
        .parse()
        .with_context(|| format!("parsing bind address {:?}", state.config.bind))?;

    let listener = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr}"))?;

    info!(%addr, public = %state.config.public_base(), "session plane listening");
    axum::serve(listener, router(state))
        .await
        .context("serving the session plane")
}

async fn health() -> impl IntoResponse {
    axum::Json(serde_json::json!({ "ok": true }))
}

/// Verifies the token, then checks it against the reservation the coordinator
/// last authorized. Both must pass: a signed, unexpired token whose reservation
/// has since been revoked is refused.
async fn authorize(
    state: &ServerState,
    device_id: &str,
    token: &str,
) -> std::result::Result<crate::auth::SessionClaims, Response> {
    let claims = match state.verifier.verify(token).await {
        Ok(claims) => claims,
        Err(err) => {
            debug!(device = %device_id, error = %format!("{err:#}"), "token rejected");
            return Err((StatusCode::UNAUTHORIZED, "invalid session token").into_response());
        }
    };

    if claims.device_id != device_id {
        return Err((StatusCode::FORBIDDEN, "token is for a different device").into_response());
    }

    if let Err(err) = state
        .supervisor
        .sessions()
        .check(device_id, &claims.reservation_id)
        .await
    {
        debug!(device = %device_id, error = %err, "reservation check failed");
        return Err((StatusCode::FORBIDDEN, err.to_string()).into_response());
    }

    Ok(claims)
}

async fn session_ws(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<TokenQuery>,
    upgrade: WebSocketUpgrade,
) -> Response {
    let claims = match authorize(&state, &device_id, &query.token).await {
        Ok(claims) => claims,
        Err(response) => return response,
    };

    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };

    upgrade.on_upgrade(move |socket| async move {
        let reservation = claims.reservation_id.clone();
        if let Err(err) = run_session(state, socket, device, reservation).await {
            debug!(device = %device_id, error = %format!("{err:#}"), "session ended");
        }
    })
}

/// One viewer.
///
/// Video out, input in, and a revocation watch that closes the socket the moment
/// the coordinator withdraws the reservation — rather than waiting up to the
/// token's ~60s lifetime.
async fn run_session(
    state: ServerState,
    socket: WebSocket,
    device: Arc<crate::supervisor::Device>,
    reservation_id: String,
) -> Result<()> {
    let (mut sink, mut source) = socket.split();
    let video = device.backend.video();

    // A fresh viewer cannot decode anything without the parameter sets.
    let Some(codec) = video.wait_for_codec(CODEC_TIMEOUT).await else {
        let msg = ServerMessage::Error {
            message: "device is not streaming".into(),
        };
        sink.send(Message::Text(serde_json::to_string(&msg)?.into()))
            .await
            .ok();
        return Ok(());
    };

    let mut geometry = video.geometry();
    let mut codec_watch = video.codec_watch();
    // Mark what we are about to send as seen, so the loop's watch arms fire on
    // the *next* change rather than replaying the handshake immediately.
    geometry.borrow_and_update();
    codec_watch.borrow_and_update();

    let geo = *geometry.borrow();
    let display = current_display(&device, geo).await;

    let handshake = ServerMessage::Codec {
        codec: codec.codec.clone(),
        description: Some(base64::engine::general_purpose::STANDARD.encode(&codec.description)),
        display,
    };
    sink.send(Message::Text(serde_json::to_string(&handshake)?.into()))
        .await?;

    let mut frames = video.subscribe();
    let mut revocations = state.supervisor.sessions().subscribe_revocations();
    let mut ping = tokio::time::interval(PING_INTERVAL);
    ping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    ping.tick().await;

    // Set after a Lagged: everything is dropped until the next keyframe, which
    // is then promoted to a decoder reset. Feeding deltas that reference a frame
    // the viewer never decoded produces torn output with no error callback.
    let mut awaiting_keyframe = false;

    // The first frame a viewer sees must be a keyframe.
    video.request_keyframe();

    let device_id = device.id.clone();

    loop {
        tokio::select! {
            frame = frames.recv() => match frame {
                Ok(au) => {
                    if awaiting_keyframe {
                        if !au.is_key {
                            continue;
                        }
                        awaiting_keyframe = false;
                        sink.send(Message::Binary(frame_au(AuKind::KeyWithReset, &au.data).into())).await?;
                    } else {
                        let kind = if au.is_key { AuKind::Key } else { AuKind::Delta };
                        sink.send(Message::Binary(frame_au(kind, &au.data).into())).await?;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(dropped)) => {
                    debug!(device = %device_id, dropped, "viewer fell behind, shedding to next keyframe");
                    awaiting_keyframe = true;
                    video.request_keyframe();
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },

            // A rotation re-encodes at new dimensions, so the parameter sets
            // change. The viewer must rebuild its decoder around them before it
            // sees a frame of the new geometry, or it decodes new-shape frames
            // against a stale avcC/hvcC and paints garbage.
            changed = codec_watch.changed() => {
                if changed.is_err() { break }
                let Some(next) = codec_watch.borrow_and_update().clone() else { continue };
                let geo = *geometry.borrow();
                let display = current_display(&device, geo).await;
                let msg = ServerMessage::Codec {
                    codec: next.codec.clone(),
                    description: Some(
                        base64::engine::general_purpose::STANDARD.encode(&next.description),
                    ),
                    display,
                };
                sink.send(Message::Text(serde_json::to_string(&msg)?.into())).await?;
                // The next key frame is promoted to KeyWithReset, which is what
                // tells the browser to rebuild rather than keep decoding.
                awaiting_keyframe = true;
                video.request_keyframe();
                debug!(device = %device_id, codec = %next.codec, "codec changed mid-session");
            }

            changed = geometry.changed() => {
                if changed.is_err() { break }
                let next = *geometry.borrow_and_update();
                if let Some(next) = next {
                    let msg = ServerMessage::Display { display: display_of(next) };
                    sink.send(Message::Text(serde_json::to_string(&msg)?.into())).await?;
                }
            }

            revocation = revocations.recv() => {
                if let Ok(event) = revocation {
                    if event.device_id == device_id {
                        let msg = ServerMessage::SessionClosed { reason: event.reason };
                        sink.send(Message::Text(serde_json::to_string(&msg)?.into())).await.ok();
                        break;
                    }
                }
            }

            _ = ping.tick() => {
                sink.send(Message::Ping(Vec::new().into())).await?;
            }

            incoming = source.next() => {
                let Some(message) = incoming else { break };
                match message? {
                    Message::Text(text) => {
                        match serde_json::from_str::<ClientMessage>(&text) {
                            Ok(msg) => {
                                if let Some(reply) =
                                    handle_client_message(&state, &device, &video, msg).await
                                {
                                    sink.send(Message::Text(serde_json::to_string(&reply)?.into())).await?;
                                }
                            }
                            Err(err) => {
                                warn!(device = %device_id, %err, "unparseable client message");
                            }
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }

        // Cheap guard against a reservation that was replaced while we were
        // mid-stream and whose revocation we somehow missed.
        if state
            .supervisor
            .sessions()
            .check(&device_id, &reservation_id)
            .await
            .is_err()
        {
            break;
        }
    }

    Ok(())
}

fn display_of(geometry: VideoGeometry) -> Display {
    Display {
        width: geometry.width,
        height: geometry.height,
        scale: None,
        rotation: geometry.rotation,
        render_rotation: geometry.render_rotation,
    }
}

/// What to tell a viewer the screen looks like.
///
/// The stream's own geometry wins when the backend publishes one: it reports
/// what is actually encoded, which is what the viewer is looking at. `info()`
/// is the fallback, and it costs a round-trip to the device.
async fn current_display(
    device: &Arc<crate::supervisor::Device>,
    geometry: Option<VideoGeometry>,
) -> Display {
    if let Some(geometry) = geometry {
        return display_of(geometry);
    }
    device
        .info()
        .await
        .and_then(|i| i.display)
        .unwrap_or(Display {
            width: 0,
            height: 0,
            scale: None,
            rotation: None,
            render_rotation: None,
        })
}

async fn handle_client_message(
    state: &ServerState,
    device: &Arc<crate::supervisor::Device>,
    video: &crate::video::VideoHandle,
    msg: ClientMessage,
) -> Option<ServerMessage> {
    let backend = &device.backend;

    // Anything a viewer sends that reaches the device is use of it. Reported
    // (rate-limited) so an idle timeout measures the device rather than the
    // tab: a session driven entirely from adb looks identical from here.
    if msg.is_interaction() {
        state.supervisor.note_activity(&device.id).await;
    }

    let result = match msg {
        ClientMessage::PointerDown { pointer_id, at } => {
            backend
                .input(InputEvent::PointerDown {
                    pointer_id,
                    x: at.x,
                    y: at.y,
                })
                .await
        }
        ClientMessage::PointerMove { pointer_id, at } => {
            backend
                .input(InputEvent::PointerMove {
                    pointer_id,
                    x: at.x,
                    y: at.y,
                })
                .await
        }
        ClientMessage::PointerUp { pointer_id, at } => {
            backend
                .input(InputEvent::PointerUp {
                    pointer_id,
                    x: at.x,
                    y: at.y,
                })
                .await
        }
        ClientMessage::Key { key, down } => backend.input(InputEvent::Key { key, down }).await,
        ClientMessage::Text { text } => backend.input(InputEvent::Text { text }).await,

        ClientMessage::ClipboardGet => {
            return match backend.clipboard_get().await {
                Ok(text) => Some(ServerMessage::Clipboard { text }),
                Err(err) => Some(ServerMessage::Error {
                    message: err.to_string(),
                }),
            };
        }
        ClientMessage::ClipboardSet { text } => backend.clipboard_set(&text).await,
        ClientMessage::Rotate { degrees } => backend.rotate(degrees).await,

        ClientMessage::Keyframe => {
            video.request_keyframe();
            Ok(())
        }
        ClientMessage::Pong { .. } => Ok(()),
    };

    result.err().map(|err| ServerMessage::Error {
        message: err.to_string(),
    })
}

async fn screenshot(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &device_id, &query.token).await {
        return response;
    }
    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };

    match device.backend.screenshot().await {
        Ok(png) => (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "no-store"),
            ],
            png,
        )
            .into_response(),
        Err(err) => (StatusCode::BAD_GATEWAY, err.to_string()).into_response(),
    }
}

/// Cadence of the fallback stream.
///
/// Deliberately slow. This path exists for browsers that cannot decode the real
/// stream at all, and each frame is a full screen capture round-trip to the
/// device — pushing it faster would cost the device more than it gains the
/// viewer.
const FALLBACK_INTERVAL: Duration = Duration::from_millis(333);

/// The fallback video path: `multipart/x-mixed-replace`, one still per part.
///
/// Served when the browser cannot use WebCodecs — no hardware decoder for the
/// stream, or a page that is not a secure context. It is honestly degraded and
/// the UI says so.
///
/// **The parts are PNG, not JPEG**, despite the route's name. Both backends
/// capture PNG, and converting would mean an image codec and a transcode in the
/// provider — the one thing this project does not do anywhere. Browsers render
/// whatever each part declares, so the type in the part header is what matters,
/// not the path.
async fn mjpeg(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<TokenQuery>,
) -> Response {
    let claims = match authorize(&state, &device_id, &query.token).await {
        Ok(claims) => claims,
        Err(response) => return response,
    };
    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };

    const BOUNDARY: &str = "farmframe";

    let stream = async_stream::stream! {
        let mut tick = tokio::time::interval(FALLBACK_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

        loop {
            tick.tick().await;

            // Re-checked every frame, not just at the start: this request
            // outlives its token by design, and a revoked reservation must stop
            // seeing the screen immediately rather than when the socket
            // happens to close.
            if state
                .supervisor
                .sessions()
                .check(&device_id, &claims.reservation_id)
                .await
                .is_err()
            {
                debug!(device = %device_id, "fallback stream ended: reservation revoked");
                break;
            }

            match device.backend.screenshot().await {
                Ok(png) => {
                    let mut part = format!(
                        "--{BOUNDARY}\r\nContent-Type: image/png\r\nContent-Length: {}\r\n\r\n",
                        png.len()
                    )
                    .into_bytes();
                    part.extend_from_slice(&png);
                    part.extend_from_slice(b"\r\n");
                    yield Ok::<_, std::io::Error>(part);
                }
                Err(err) => {
                    // One failed capture is not the end of the stream — a
                    // device mid-rotation refuses for a moment.
                    debug!(device = %device_id, %err, "fallback capture failed");
                }
            }
        }
    };

    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                format!("multipart/x-mixed-replace; boundary={BOUNDARY}"),
            ),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        Body::from_stream(stream),
    )
        .into_response()
}

/// Deletes the staged upload however the install turns out.
///
/// A failed install must not leak disk — on a provider with a tmpfs scratch dir
/// that is the difference between a bad build and a wedged host.
struct StagedFile(std::path::PathBuf);

impl Drop for StagedFile {
    fn drop(&mut self) {
        if let Err(err) = std::fs::remove_file(&self.0) {
            if err.kind() != std::io::ErrorKind::NotFound {
                warn!(path = %self.0.display(), %err, "could not remove staged upload");
            }
        }
    }
}

/// Streams an APK/IPA to disk, installs it, and deletes it.
///
/// The body is written to the scratch dir as it arrives rather than buffered,
/// so a 200 MB APK never sits in memory. Nothing is stored server-side and
/// there is no artifact table anywhere in the system — the only trace is the
/// `install.finished` event this sends up the control plane, which the
/// coordinator writes to its audit log.
async fn install(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<TokenQuery>,
    headers: axum::http::HeaderMap,
    body: Body,
) -> Response {
    let claims = match authorize(&state, &device_id, &query.token).await {
        Ok(claims) => claims,
        Err(response) => return response,
    };
    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };

    let filename = headers
        .get("x-farm-filename")
        .and_then(|v| v.to_str().ok())
        .map(sanitize_filename)
        .unwrap_or_else(|| "upload.bin".to_owned());

    if let Err(err) = tokio::fs::create_dir_all(&state.config.scratch_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratch dir unusable: {err}"),
        )
            .into_response();
    }

    let staged_path =
        state
            .config
            .scratch_dir
            .join(format!("{}-{}", uuid::Uuid::new_v4(), filename));
    let staged = StagedFile(staged_path.clone());

    let (size, sha256) = match stream_to_disk(body, &staged_path).await {
        Ok(result) => result,
        Err(err) => {
            return (StatusCode::BAD_REQUEST, format!("upload failed: {err:#}")).into_response()
        }
    };

    info!(device = %device_id, %filename, size, "installing upload");

    state.supervisor.note_activity(&device_id).await;

    let progress = LogProgress {
        device_id: device_id.clone(),
    };
    let outcome = device.backend.install(&staged_path, &progress).await;

    // The file is gone by the time this row is written, which is exactly why
    // the audit entry has to carry the digest.
    state
        .supervisor
        .push_install_result(
            &device_id,
            &claims.user_id,
            &filename,
            size,
            &sha256,
            outcome.as_ref().err().map(|e| e.to_string()),
        )
        .await;

    drop(staged);

    match outcome {
        Ok(()) => axum::Json(serde_json::json!({
            "ok": true, "filename": filename, "size": size, "sha256": sha256
        }))
        .into_response(),
        Err(err) => (
            StatusCode::BAD_GATEWAY,
            axum::Json(serde_json::json!({ "ok": false, "error": err.to_string() })),
        )
            .into_response(),
    }
}

/// How a backend refusal reads on the wire.
///
/// `Unsupported` is 501 rather than 502 on purpose: it is the difference
/// between "this device cannot do that" and "it tried and failed", and the
/// browser shows a different thing for each.
fn backend_status(err: &crate::backend::BackendError) -> StatusCode {
    match err {
        crate::backend::BackendError::Unsupported(_) => StatusCode::NOT_IMPLEMENTED,
        crate::backend::BackendError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        crate::backend::BackendError::Failed(_) => StatusCode::BAD_GATEWAY,
    }
}

/// `GET /s/{id}/files?path=…` — one directory, as JSON.
///
/// Not audited. A listing is a metadata read and one browse would write a row
/// per click; the row worth having is the one that says bytes left the device,
/// which [`pull_file`] writes.
async fn list_files(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    if let Err(response) = authorize(&state, &device_id, &query.token).await {
        return response;
    }
    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };

    let Some(root) = device.backend.files_root() else {
        return (
            StatusCode::NOT_IMPLEMENTED,
            "this device does not offer file access",
        )
            .into_response();
    };
    let path = query.path.as_deref().unwrap_or(root);

    match device.backend.list_files(path).await {
        Ok(listing) => (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            axum::Json(listing),
        )
            .into_response(),
        // The device's own refusal, verbatim — "Permission denied" is the
        // whole answer for an unrooted adb reading /data/data, and rewording it
        // would only hide which directory said no.
        Err(err) => (backend_status(&err), err.to_string()).into_response(),
    }
}

/// `GET /s/{id}/file?path=…` — the bytes, as an attachment.
///
/// The file is copied off the device into the scratch directory and deleted
/// when the response body is dropped. That staging is what keeps a large pull
/// out of the provider's memory, and it is the same arrangement the install
/// path uses in the other direction — there is still no artifact storage
/// anywhere, and the only lasting trace is the audit row.
async fn pull_file(
    State(state): State<ServerState>,
    Path(device_id): Path<String>,
    Query(query): Query<FileQuery>,
) -> Response {
    let claims = match authorize(&state, &device_id, &query.token).await {
        Ok(claims) => claims,
        Err(response) => return response,
    };
    let Some(device) = state.supervisor.device(&device_id) else {
        return (StatusCode::NOT_FOUND, "unknown device").into_response();
    };
    let Some(remote_path) = query.path.filter(|p| !p.is_empty()) else {
        return (StatusCode::BAD_REQUEST, "path is required").into_response();
    };

    if let Err(err) = tokio::fs::create_dir_all(&state.config.scratch_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("scratch dir unusable: {err}"),
        )
            .into_response();
    }

    // The device's path never becomes the staged name — a uuid does, with the
    // sanitized basename only for legibility in a log or a stray temp file.
    let filename = sanitize_filename(&remote_path);
    let staged_path =
        state
            .config
            .scratch_dir
            .join(format!("{}-{}", uuid::Uuid::new_v4(), filename));
    let staged = StagedFile(staged_path.clone());

    let size = match device.backend.pull_file(&remote_path, &staged_path).await {
        Ok(size) => size as i64,
        Err(err) => return (backend_status(&err), err.to_string()).into_response(),
    };

    let sha256 = match hash_file(&staged_path).await {
        Ok(digest) => digest,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("hashing the staged file: {err:#}"),
            )
                .into_response()
        }
    };

    info!(device = %device_id, path = %remote_path, size, "serving a pulled file");
    state.supervisor.note_activity(&device_id).await;

    // Written before the body is sent, not after: a download the client aborts
    // half way still took the bytes off the device, and an egress record that
    // only lands on a clean finish is the wrong way for this to be wrong.
    state
        .supervisor
        .push_file_pulled(&device_id, &claims.user_id, &remote_path, size, &sha256)
        .await;

    let mut file = match tokio::fs::File::open(&staged_path).await {
        Ok(file) => file,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("staged file unreadable: {err}"),
            )
                .into_response()
        }
    };

    let body = Body::from_stream(async_stream::stream! {
        // Moved in so the file outlives the response exactly, and is deleted
        // when the client is done with it — or when it gives up.
        let _staged = staged;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match tokio::io::AsyncReadExt::read(&mut file, &mut buf).await {
                Ok(0) => break,
                Ok(n) => yield Ok::<_, std::io::Error>(buf[..n].to_vec()),
                Err(err) => {
                    yield Err(err);
                    break;
                }
            }
        }
    });

    (
        StatusCode::OK,
        [
            (
                header::CONTENT_TYPE,
                "application/octet-stream".to_owned(),
            ),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{filename}\""),
            ),
            (header::CONTENT_LENGTH, size.to_string()),
            (header::CACHE_CONTROL, "no-store".to_owned()),
        ],
        body,
    )
        .into_response()
}

async fn hash_file(path: &std::path::Path) -> Result<String> {
    use sha2::Digest as _;

    let mut file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = vec![0u8; 64 * 1024];
    loop {
        let read = tokio::io::AsyncReadExt::read(&mut file, &mut buf)
            .await
            .context("reading the staged file")?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex(&hasher.finalize()))
}

async fn stream_to_disk(body: Body, path: &std::path::Path) -> Result<(i64, String)> {
    use sha2::Digest as _;

    let mut file = tokio::fs::File::create(path)
        .await
        .with_context(|| format!("creating {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut size: i64 = 0;

    let mut stream = body.into_data_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.context("reading upload body")?;
        hasher.update(&chunk);
        size += chunk.len() as i64;
        file.write_all(&chunk)
            .await
            .context("writing staged upload")?;
    }
    file.flush().await?;
    file.sync_all().await.ok();

    Ok((size, hex(&hasher.finalize())))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Strips any path component: the name comes from a client header and must
/// never be able to escape the scratch directory.
fn sanitize_filename(raw: &str) -> String {
    let base = raw.rsplit(['/', '\\']).next().unwrap_or("upload.bin");
    let cleaned: String = base
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "upload.bin".to_owned()
    } else {
        cleaned
    }
}

struct LogProgress {
    device_id: String,
}

impl ProgressSink for LogProgress {
    fn report(&self, stage: &str, progress: Option<f64>) {
        debug!(device = %self.device_id, stage, ?progress, "install progress");
    }
}

#[cfg(test)]
mod tests {
    use super::sanitize_filename;

    #[test]
    fn filenames_cannot_escape_the_scratch_directory() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("/absolute/app.apk"), "app.apk");
        assert_eq!(sanitize_filename(r"..\..\windows\evil.exe"), "evil.exe");
        assert_eq!(sanitize_filename(".."), "upload.bin");
        assert_eq!(sanitize_filename(""), "upload.bin");
    }

    #[test]
    fn ordinary_names_survive_intact() {
        assert_eq!(sanitize_filename("app-release.apk"), "app-release.apk");
        assert_eq!(sanitize_filename("MyApp_1.2.3.ipa"), "MyApp_1.2.3.ipa");
    }

    #[test]
    fn shell_metacharacters_are_stripped() {
        assert_eq!(sanitize_filename("app$(id).apk"), "appid.apk");
        assert_eq!(sanitize_filename("app;rm -rf x.apk"), "apprm-rfx.apk");
        // The basename is taken first, so anything before the last separator is
        // discarded outright rather than merely being scrubbed.
        assert_eq!(sanitize_filename("app;rm -rf /.apk"), ".apk");
    }

    #[test]
    fn the_result_is_always_a_single_harmless_path_component() {
        for hostile in [
            "../../etc/passwd",
            "/absolute/app.apk",
            r"..\..\evil.exe",
            "app;rm -rf /.apk",
            "....//....//x",
            "",
            "..",
        ] {
            let cleaned = sanitize_filename(hostile);
            assert!(!cleaned.is_empty(), "{hostile:?} produced an empty name");
            assert!(!cleaned.contains('/'), "{hostile:?} kept a separator");
            assert!(!cleaned.contains('\\'), "{hostile:?} kept a separator");
            assert_eq!(
                std::path::Path::new(&cleaned).components().count(),
                1,
                "{hostile:?} produced more than one path component"
            );
            // The joined path must stay inside the scratch directory.
            let joined = std::path::Path::new("/scratch").join(&cleaned);
            assert!(
                joined.starts_with("/scratch"),
                "{hostile:?} escaped: {joined:?}"
            );
        }
    }
}
