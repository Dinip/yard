//! Serving one `adb connect` client: authenticate, then multiplex its streams.
//!
//! Once the handshake is through, ADB is a stream multiplexer over a single
//! socket. Each `OPEN` names a service — `shell:`, `sync:`, `tcp:8080` — and
//! every service the client can name, the provider's own adb server can open on
//! the device. So the bridge does not implement `shell` or `sync` or anything
//! else: it opens the same service upstream and copies bytes, and the features
//! a client gets are whatever the device actually supports.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::{mpsc, Mutex, Semaphore};
use tracing::{debug, info, warn};

use crate::auth::{authenticate, AuthError, Authorizer, BannerSource, Identity};
use crate::message::{Command, Message};

/// Services that change a device in ways that outlive the session.
///
/// `root:` and `unroot:` restart `adbd` — taking the provider's own transport
/// and the live screen stream with it — and leave the *next* person a rooted
/// phone. `remount:` makes /system writable and stays that way until a reboot.
/// STF refuses the same three. Everything else is passed through untouched,
/// including `shell:su -c …` on a device that genuinely has `su`: that is the
/// operator's decision about their fleet, not a state change we caused.
const REFUSED: [&str; 3] = ["root", "unroot", "remount"];

/// Anything the bridge can copy bytes over.
pub trait Transport: AsyncRead + AsyncWrite + Unpin + Send + 'static {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send + 'static> Transport for T {}

/// The provider's side: opening services on a real device.
#[async_trait]
pub trait ServiceOpener: Send + Sync {
    /// Open one ADB service on the device.
    ///
    /// An error closes that stream alone — a failed `shell:` must not take the
    /// client's whole connection with it.
    async fn open(&self, service: &str) -> anyhow::Result<Box<dyn Transport>>;

    /// The `device::…` banner, read from the device rather than invented.
    ///
    /// It carries the feature list, so a made-up one silently costs the client
    /// `shell_v2` — no exit codes, stdout and stderr merged.
    async fn banner(&self) -> String;

    /// Somebody is driving this device. Called as traffic flows, so
    /// implementations must rate-limit rather than forward every call.
    async fn activity(&self);
}

pub struct Bridge {
    authorizer: Arc<dyn Authorizer>,
    opener: Arc<dyn ServiceOpener>,
}

impl Bridge {
    pub fn new(authorizer: Arc<dyn Authorizer>, opener: Arc<dyn ServiceOpener>) -> Self {
        Self { authorizer, opener }
    }

    /// Serve one client to completion.
    pub async fn serve<S: Transport>(&self, socket: S, peer: &str) -> Result<(), AuthError> {
        let mut socket = socket;
        let handshake =
            authenticate(&mut socket, &*self.authorizer, DeviceBanner(&*self.opener)).await?;
        let identity = handshake.identity.clone();
        info!(
            %peer,
            user = %identity.user_id,
            fingerprint = %identity.fingerprint,
            "adb client authenticated"
        );

        let (reader, writer) = tokio::io::split(socket);
        let session = Session {
            writer: Arc::new(Mutex::new(Box::new(writer) as Box<dyn AsyncWrite + Unpin + Send>)),
            opener: self.opener.clone(),
            streams: Mutex::new(HashMap::new()),
            next_id: AtomicU32::new(1),
            max_payload: handshake.max_payload as usize,
            identity,
        };

        let outcome = session.run(reader).await;
        session.close_all().await;
        outcome
    }
}

/// Lets the auth machine pull the banner off the device without knowing what a
/// device is.
struct DeviceBanner<'a>(&'a dyn ServiceOpener);

#[async_trait]
impl BannerSource for DeviceBanner<'_> {
    async fn banner(&self) -> String {
        self.0.banner().await
    }
}

type SharedWriter = Arc<Mutex<Box<dyn AsyncWrite + Unpin + Send>>>;

struct StreamHandle {
    /// Payloads from the client, on their way to the device.
    to_device: mpsc::Sender<Bytes>,
    /// One permit per `OKAY` the client owes us; ADB allows one unacknowledged
    /// write in flight per stream.
    ready: Arc<Semaphore>,
    task: tokio::task::AbortHandle,
}

struct Session {
    writer: SharedWriter,
    opener: Arc<dyn ServiceOpener>,
    streams: Mutex<HashMap<u32, StreamHandle>>,
    next_id: AtomicU32,
    max_payload: usize,
    identity: Identity,
}

impl Session {
    async fn run<R: AsyncRead + Unpin>(&self, mut reader: R) -> Result<(), AuthError> {
        loop {
            let msg = match Message::read(&mut reader).await {
                Ok(msg) => msg,
                // The client hanging up is how this normally ends.
                Err(err) => {
                    debug!(user = %self.identity.user_id, error = %err, "adb client gone");
                    return Ok(());
                }
            };

            match msg.command {
                Command::Open => self.on_open(msg.arg0, msg.payload_str()).await,
                Command::Wrte => self.on_write(msg.arg1, msg.payload).await,
                Command::Okay => self.on_okay(msg.arg1).await,
                Command::Clse => self.on_close(msg.arg1).await,
                // A second CNXN, or a TLS offer we never advertised. Ignoring
                // beats dropping a working connection over a stray frame.
                other => debug!(?other, "ignoring a post-handshake frame"),
            }
        }
    }

    /// The client wants a service. `remote` is *its* id for the stream.
    async fn on_open(&self, remote: u32, service: String) {
        self.opener.activity().await;

        if let Some(name) = refused(&service) {
            warn!(
                user = %self.identity.user_id,
                %service,
                "refusing a service that would outlive the session"
            );
            self.refuse(remote, &format!(
                "adb {name} is not available on a farm device: it changes the device for whoever \
                 has it next.\n"
            ))
            .await;
            return;
        }

        let local = self.next_id.fetch_add(1, Ordering::Relaxed);
        let device = match self.opener.open(&service).await {
            Ok(device) => device,
            Err(err) => {
                debug!(%service, error = %format!("{err:#}"), "opening a service failed");
                // `CLSE` with a zero local id is how adbd says "no such
                // service"; the client reports it and keeps the connection.
                self.send(Message::empty(Command::Clse, 0, remote)).await;
                return;
            }
        };

        let (to_device, from_client) = mpsc::channel::<Bytes>(8);
        let ready = Arc::new(Semaphore::new(1));

        // The writer stays locked across the whole of this. `OKAY` is what
        // tells the client the stream exists and which id to use for it, so a
        // service that speaks the moment it opens — `logcat`, an interactive
        // shell's prompt — must not get its first `WRTE` out in front of it.
        // The pump needs the same lock to write, which is what orders them.
        let mut writer = self.writer.lock().await;
        if let Err(err) = Message::empty(Command::Okay, local, remote)
            .write(&mut *writer)
            .await
        {
            debug!(error = %err, "writing to the adb client failed");
            return;
        }

        let task = tokio::spawn(pump(
            device,
            from_client,
            ready.clone(),
            self.writer.clone(),
            local,
            remote,
            self.max_payload,
        ));

        self.streams.lock().await.insert(
            local,
            StreamHandle {
                to_device,
                ready,
                task: task.abort_handle(),
            },
        );
        debug!(%service, local, remote, "stream opened");
    }

    /// Accept the stream, say why, and close it.
    ///
    /// A bare `CLSE` would make `adb root` print nothing at all, and the person
    /// running it would reasonably conclude the farm is broken.
    async fn refuse(&self, remote: u32, message: &str) {
        let local = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.send(Message::empty(Command::Okay, local, remote)).await;
        self.send(Message::new(
            Command::Wrte,
            local,
            remote,
            message.as_bytes().to_vec(),
        ))
        .await;
        self.send(Message::empty(Command::Clse, local, remote)).await;
    }

    async fn on_write(&self, local: u32, payload: Bytes) {
        self.opener.activity().await;
        let sender = {
            let streams = self.streams.lock().await;
            streams.get(&local).map(|s| s.to_device.clone())
        };
        let Some(sender) = sender else {
            debug!(local, "a write for a stream that is gone");
            return;
        };
        // Full means the device is slower than the client; waiting here is the
        // backpressure, and it reaches the client because we stop reading.
        if sender.send(payload).await.is_err() {
            self.on_close(local).await;
        }
    }

    /// The client acknowledged our write and will take another.
    async fn on_okay(&self, local: u32) {
        if let Some(stream) = self.streams.lock().await.get(&local) {
            stream.ready.add_permits(1);
        }
    }

    async fn on_close(&self, local: u32) {
        if let Some(stream) = self.streams.lock().await.remove(&local) {
            stream.task.abort();
            debug!(local, "stream closed");
        }
    }

    async fn close_all(&self) {
        for (_, stream) in self.streams.lock().await.drain() {
            stream.task.abort();
        }
    }

    async fn send(&self, msg: Message) {
        let mut writer = self.writer.lock().await;
        if let Err(err) = msg.write(&mut *writer).await {
            debug!(error = %err, "writing to the adb client failed");
        }
    }
}

/// Copy one stream in both directions until either end stops.
#[allow(clippy::too_many_arguments)]
async fn pump(
    device: Box<dyn Transport>,
    mut from_client: mpsc::Receiver<Bytes>,
    ready: Arc<Semaphore>,
    writer: SharedWriter,
    local: u32,
    remote: u32,
    max_payload: usize,
) {
    let (mut device_read, mut device_write) = tokio::io::split(device);
    let mut buf = vec![0u8; max_payload];

    loop {
        tokio::select! {
            incoming = from_client.recv() => {
                let Some(data) = incoming else { break };
                if device_write.write_all(&data).await.is_err() {
                    break;
                }
                // Acknowledged only once the device has it, so the client's
                // one-write-in-flight limit tracks the device rather than us.
                let mut w = writer.lock().await;
                if Message::empty(Command::Okay, local, remote).write(&mut *w).await.is_err() {
                    break;
                }
            }

            read = device_read.read(&mut buf) => {
                let Ok(n) = read else { break };
                if n == 0 {
                    break;
                }
                // ADB allows one unacknowledged write per stream. Taking the
                // permit before sending is what stops a chatty `logcat` from
                // burying a client that has not caught up.
                let Ok(permit) = ready.acquire().await else { break };
                permit.forget();

                let mut w = writer.lock().await;
                let chunk = Bytes::copy_from_slice(&buf[..n]);
                if Message::new(Command::Wrte, local, remote, chunk).write(&mut *w).await.is_err() {
                    break;
                }
            }
        }
    }

    // Whichever side ended it, the client has to be told, or `adb shell` sits
    // at a prompt that will never answer again.
    let mut w = writer.lock().await;
    let _ = Message::empty(Command::Clse, local, remote).write(&mut *w).await;
}

/// The service name a refusal applies to, if this is one.
fn refused(service: &str) -> Option<&'static str> {
    let name = service.split(':').next().unwrap_or_default();
    REFUSED.into_iter().find(|refused| *refused == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_three_state_changing_services_are_refused() {
        assert_eq!(refused("root:"), Some("root"));
        assert_eq!(refused("remount:"), Some("remount"));
        assert_eq!(refused("unroot:"), Some("unroot"));

        assert_eq!(refused("shell:"), None);
        assert_eq!(refused("sync:"), None);
        assert_eq!(refused("tcp:8080"), None);
        // Not a prefix match: refusing this would break `adb shell su -c`,
        // which is the operator's call to make about their own fleet.
        assert_eq!(refused("shell:su -c id"), None);
        assert_eq!(refused("shell:rootish"), None);
    }
}
