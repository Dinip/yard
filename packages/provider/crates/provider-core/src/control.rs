//! One outbound WSS to the coordinator.
//!
//! Replaces `stf-ios-provider/src/bus.rs` (ZMQ PUSH/SUB against STF's triproxy)
//! and deletes the `zeromq`, `prost` and `protox` dependencies with it.
//!
//! Dialling *out* is the load-bearing choice: the provider needs no inbound
//! reachability, so it works identically on the coordinator's host, in another
//! datacentre, or behind office NAT.

use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use farm_protocol::{
    CommandData, CommandPayload, CoordinatorMessage, DeviceSnapshot, ProviderMessage,
    PROTOCOL_VERSION,
};
use futures::{SinkExt as _, StreamExt as _};
use tokio::sync::mpsc;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, error, info, warn};

use crate::config::{Config, RECONNECT_MAX, RECONNECT_MIN};
use crate::origins::WebOrigins;

/// What the control plane hands to whoever executes commands. Implemented by
/// the supervisor; kept as a trait so the client is testable on its own.
#[async_trait::async_trait]
pub trait CommandHandler: Send + Sync + 'static {
    async fn handle(&self, payload: CommandPayload) -> Result<Option<CommandData>>;

    /// The full inventory to send in `hello`, gathered fresh on every connect.
    async fn inventory(&self) -> Vec<DeviceSnapshot>;

    /// Called whenever the socket drops, so per-device state that the
    /// coordinator is the source of truth for can be cleared.
    async fn on_disconnected(&self) {}
}

/// Send handle for pushing events up to the coordinator.
///
/// Cloneable and cheap. Sends are best-effort: when the socket is down the
/// message is dropped rather than queued, because the coordinator reconciles
/// the whole inventory on the next `hello` anyway. Queueing would only deliver
/// stale device states after a reconnect.
#[derive(Clone)]
pub struct ControlSender {
    tx: mpsc::UnboundedSender<ProviderMessage>,
}

impl ControlSender {
    pub fn send(&self, msg: ProviderMessage) {
        let _ = self.tx.send(msg);
    }
}

pub struct ControlClient {
    config: Arc<Config>,
    handler: Arc<dyn CommandHandler>,
    /// Written on every `hello.ack`; read by the browser-facing server.
    web_origins: WebOrigins,
    tx: mpsc::UnboundedSender<ProviderMessage>,
    rx: mpsc::UnboundedReceiver<ProviderMessage>,
}

impl ControlClient {
    pub fn new(
        config: Arc<Config>,
        handler: Arc<dyn CommandHandler>,
        web_origins: WebOrigins,
    ) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            config,
            handler,
            web_origins,
            tx,
            rx,
        }
    }

    pub fn sender(&self) -> ControlSender {
        ControlSender {
            tx: self.tx.clone(),
        }
    }

    /// Connects, and keeps reconnecting, until the process stops.
    ///
    /// A coordinator outage must never take a provider down: devices keep
    /// streaming to already-authorized viewers throughout, because
    /// authorization lives in `SessionRegistry` and tokens verify against the
    /// cached JWKS.
    pub async fn run(mut self) {
        let mut backoff = RECONNECT_MIN;

        loop {
            match self.connect_once().await {
                Ok(()) => {
                    info!("control plane closed cleanly, reconnecting");
                    backoff = RECONNECT_MIN;
                }
                Err(err) => {
                    warn!(error = %format!("{err:#}"), backoff = ?backoff, "control plane failed");
                }
            }

            self.handler.on_disconnected().await;

            tokio::time::sleep(backoff).await;
            backoff = (backoff * 2).min(RECONNECT_MAX);
        }
    }

    async fn connect_once(&mut self) -> Result<()> {
        let url = self.config.control_url();
        let mut request = url.as_str().into_client_request()?;
        request.headers_mut().insert(
            "authorization",
            format!("Bearer {}", self.config.token())
                .parse()
                .context("provider token is not a valid header value")?,
        );

        debug!(%url, "dialling coordinator");
        let (stream, response) = tokio_tungstenite::connect_async(request)
            .await
            .with_context(|| format!("connecting to {url}"))?;
        debug!(status = ?response.status(), "control plane connected");

        let (mut sink, mut source) = stream.split();

        // `hello` carries the whole inventory: the coordinator reconciles rather
        // than applying deltas, so a device that vanished while we were
        // disconnected is correctly marked absent.
        let hello = ProviderMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            provider_id: self.config.id.clone(),
            name: self.config.name.clone(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            public_base_url: self.config.public_base().to_owned(),
            hostname: hostname(),
            devices: self.handler.inventory().await,
        };
        sink.send(Message::Text(serde_json::to_string(&hello)?.into()))
            .await
            .context("sending hello")?;

        // Set once hello.ack lands; heartbeats must not start before that or the
        // coordinator sees traffic it has not agreed a cadence for.
        let mut heartbeat: Option<tokio::time::Interval> = None;

        loop {
            tokio::select! {
                // Outbound: device events raised anywhere in the process.
                outgoing = self.rx.recv() => {
                    let Some(msg) = outgoing else { return Ok(()) };
                    let text = serde_json::to_string(&msg)?;
                    sink.send(Message::Text(text.into())).await.context("sending event")?;
                }

                _ = async { heartbeat.as_mut().unwrap().tick().await }, if heartbeat.is_some() => {
                    let beat = ProviderMessage::Heartbeat { at: now_millis() };
                    sink.send(Message::Text(serde_json::to_string(&beat)?.into()))
                        .await
                        .context("sending heartbeat")?;
                }

                incoming = source.next() => {
                    let Some(frame) = incoming else { return Ok(()) };
                    match frame.context("reading from the control plane")? {
                        Message::Text(text) => {
                            match self.on_message(&text, &mut heartbeat).await {
                                Ok(Some(reply)) => {
                                    sink.send(Message::Text(serde_json::to_string(&reply)?.into()))
                                        .await
                                        .context("sending command result")?;
                                }
                                Ok(None) => {}
                                Err(err) => return Err(err),
                            }
                        }
                        Message::Ping(payload) => {
                            sink.send(Message::Pong(payload)).await.ok();
                        }
                        Message::Close(frame) => {
                            info!(?frame, "coordinator closed the control plane");
                            return Ok(());
                        }
                        // Binary on the control plane means someone confused it
                        // with the session plane.
                        other => debug!(?other, "ignoring unexpected control frame"),
                    }
                }
            }
        }
    }

    async fn on_message(
        &self,
        text: &str,
        heartbeat: &mut Option<tokio::time::Interval>,
    ) -> Result<Option<ProviderMessage>> {
        let msg: CoordinatorMessage = serde_json::from_str(text)
            .with_context(|| format!("parsing coordinator message: {text}"))?;

        match msg {
            CoordinatorMessage::HelloAck {
                protocol_version,
                heartbeat_interval_ms,
                jwks_url,
                web_origins,
            } => {
                if protocol_version != PROTOCOL_VERSION {
                    bail!(
                        "protocol mismatch: coordinator speaks {protocol_version}, \
                         this provider speaks {PROTOCOL_VERSION}"
                    );
                }
                info!(
                    %jwks_url,
                    heartbeat_interval_ms,
                    origins = ?web_origins,
                    "registered with coordinator"
                );
                // Until this lands the artifact plane refuses every browser
                // request, so it must be applied before anything else.
                self.web_origins.set(web_origins);

                let mut interval = tokio::time::interval(Duration::from_millis(
                    heartbeat_interval_ms.max(1) as u64,
                ));
                // A stalled write must not cause a burst of catch-up beats.
                interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                interval.tick().await; // the first tick is immediate
                *heartbeat = Some(interval);
                Ok(None)
            }

            CoordinatorMessage::HelloReject { reason } => {
                // Fatal for this connection, not for the process: an admin
                // fixing the token should not need a restart.
                error!(%reason, "coordinator rejected this provider");
                bail!("rejected by coordinator: {reason}")
            }

            CoordinatorMessage::Command { id, payload } => {
                let kind = format!("{payload:?}");
                let reply = match self.handler.handle(payload).await {
                    Ok(data) => ProviderMessage::CommandResult {
                        id,
                        ok: true,
                        error: None,
                        data,
                    },
                    Err(err) => {
                        warn!(command = %kind, error = %format!("{err:#}"), "command failed");
                        ProviderMessage::CommandResult {
                            id,
                            ok: false,
                            error: Some(format!("{err:#}")),
                            data: None,
                        }
                    }
                };
                Ok(Some(reply))
            }

            CoordinatorMessage::Ping { .. } => Ok(None),
        }
    }
}

fn hostname() -> Option<String> {
    std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty())
}

pub fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}
