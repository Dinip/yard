//! The device session: the root-free RSD tunnel, the media stream, and HID.
//!
//! Ported from `stf-ios-provider/src/device/mod.rs`, with one departure: the
//! tunnel and the capture session no longer live or die together.
//!
//! usbmuxd is normally the host's own socket. `USBMUXD_SOCKET_ADDRESS` exists
//! for the one case where it cannot be: a provider container on macOS, where
//! Docker passes neither USB nor unix sockets through.
//!
//! One supervisor task rebuilds the tunnel forever, and inside it a second loop
//! brings capture up and down as demand comes and goes. Media and HID stay
//! together within that inner loop, because the HID surfaces authenticate
//! against the live media stream; the tunnel outlives both, because health,
//! identity, battery and the file and app services all ride on it.
//!
//! That split is the whole point: an unreserved iPhone would otherwise run its
//! hardware encoder all day for nobody. [`DeviceHost::is_ready`] means *the
//! tunnel is up* and nothing more, so a device that has stopped streaming
//! because no one is watching stays `ready` in the pool. Whether capture is
//! running is [`DeviceHost::wait_media`]'s question, and only the video and
//! input paths ask it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use idevice::core_device_proxy::CoreDeviceProxy;
use idevice::lockdown::LockdownClient;
use idevice::provider::{IdeviceProvider, RsdProvider as _};
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::AdapterHandle;
use idevice::usbmuxd::{Connection, UsbmuxdAddr, UsbmuxdDevice};
use idevice::{IdeviceService as _, ReadWrite, RsdService};
use provider_core::demand::{Demand, IDLE_GRACE};
use provider_core::video::VideoPublisher;
use tokio::sync::{watch, Mutex};
use tokio::time::timeout;
use tracing::{debug, info, warn};

use crate::{ddi, hid, media, Geometry, IosOptions};

/// Delay before rebuilding a session that dropped, so an unplugged or rebooting
/// device does not spin the loop.
const RECONNECT_DELAY: Duration = Duration::from_secs(5);

/// The HID surfaces only authenticate once the media stream is up, and
/// backboardd needs a moment to re-match them afterwards.
const HID_SETTLE: Duration = Duration::from_millis(300);

/// Identity values, read in one lockdown round-trip.
#[derive(Clone, Debug, Default)]
pub struct DeviceIdentity {
    pub name: String,
    pub product_type: String,
    pub version: String,
    pub build: String,
    pub serial: String,
    pub cpu: String,
}

impl DeviceIdentity {
    pub fn is_apple_tv(&self) -> bool {
        self.product_type.contains("AppleTV")
    }
}

/// A live RSD session's handles, cloned out for callers that open their own
/// service connections (screenshots, app list, reboot).
#[derive(Clone)]
pub struct Session {
    pub adapter: AdapterHandle,
    pub handshake: RsdHandshake,
}

pub struct DeviceHost {
    options: IosOptions,
    ready: watch::Receiver<bool>,
    /// Media and HID, which come and go with demand under a tunnel that stays.
    media_ready: watch::Receiver<bool>,
    session: Arc<Mutex<Option<Session>>>,
    generation: Arc<AtomicU64>,

    input: Arc<Mutex<Option<hid::InputHandle>>>,
}

impl DeviceHost {
    /// Start the supervisor. It runs until the process exits.
    pub fn spawn(
        options: IosOptions,
        publisher: VideoPublisher,
        geometry: Geometry,
        demand: Demand,
    ) -> Self {
        let (ready_tx, ready_rx) = watch::channel(false);
        let (media_ready_tx, media_ready_rx) = watch::channel(false);
        let session = Arc::new(Mutex::new(None));
        let input = Arc::new(Mutex::new(None));
        let generation = Arc::new(AtomicU64::new(0));

        let host = Self {
            options: options.clone(),
            ready: ready_rx,
            media_ready: media_ready_rx,
            session: session.clone(),
            generation: generation.clone(),
            input: input.clone(),
        };

        tokio::spawn(supervise(Supervisor {
            options,
            geometry,
            ready: ready_tx,
            media_ready: media_ready_tx,
            session,
            generation,
            publisher,
            input,
            demand,
            ddi_failed_at: None,
        }));

        host
    }

    /// Whether the tunnel is up — *not* whether the device is streaming.
    ///
    /// This is what `is_healthy` reports upstream, and it deliberately says
    /// nothing about capture: a device sitting idle with its encoder off is a
    /// perfectly good device, and reporting otherwise would empty the pool.
    pub fn is_ready(&self) -> bool {
        *self.ready.borrow()
    }

    /// The generation counter, bumped once per successful session bring-up.
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }

    /// The current session's RSD handles, or `None` while rebuilding.
    pub async fn session(&self) -> Option<Session> {
        self.session.lock().await.clone()
    }

    /// The current session's input queue, or `None` while rebuilding.
    pub async fn input(&self) -> Option<hid::InputHandle> {
        self.input.lock().await.clone()
    }

    /// Tear the current session down; the supervisor rebuilds it.
    ///
    /// Closing the adapter is the whole mechanism: it shuts the userspace TCP
    /// stack the media and HID tasks are running over, both fail, and
    /// `run_once` returns. There is no finer-grained restart to offer — the HID
    /// surfaces authenticate against the live media stream, so they come back
    /// together or not at all.
    pub async fn drop_session(&self) {
        let Some(session) = self.session.lock().await.take() else {
            return;
        };
        let mut adapter = session.adapter;
        let _ = adapter.close().await;
    }

    /// Wait for the tunnel to come up, up to `wait`.
    pub async fn wait_ready(&self, wait: Duration) -> bool {
        wait_for(self.ready.clone(), wait).await
    }

    /// Wait for media and HID, up to `wait`.
    ///
    /// The caller is expected to have taken a [`Demand`] lease — or touched it
    /// — first: capture only starts because something asked for it, and this
    /// is the wait for that bring-up.
    pub async fn wait_media(&self, wait: Duration) -> bool {
        wait_for(self.media_ready.clone(), wait).await
    }

    /// Identity values, read over plain lockdown rather than the tunnel.
    ///
    /// `GetValue` with no key is a single cheap plist round-trip and works even
    /// while the tunnel is rebuilding, which is exactly when the provider wants
    /// to re-announce.
    pub async fn identity(&self) -> Result<DeviceIdentity> {
        let provider = usbmux_provider(&self.options.udid).await?;
        let mut lockdown = LockdownClient::connect(&*provider)
            .await
            .map_err(|err| anyhow!("lockdown connect: {err:?}"))?;
        let pairing = provider
            .get_pairing_file()
            .await
            .map_err(|err| anyhow!("pairing file: {err:?}"))?;
        lockdown
            .start_session(&pairing)
            .await
            .map_err(|err| anyhow!("lockdown session: {err:?}"))?;

        let values = lockdown
            .get_value(None, None)
            .await
            .map_err(|err| anyhow!("lockdown GetValue: {err:?}"))?;
        let values = values
            .into_dictionary()
            .ok_or_else(|| anyhow!("lockdown GetValue returned no dictionary"))?;

        let string = |key: &str| {
            values
                .get(key)
                .and_then(plist::Value::as_string)
                .unwrap_or_default()
                .to_owned()
        };

        let mut info = DeviceIdentity {
            name: string("DeviceName"),
            product_type: string("ProductType"),
            version: string("ProductVersion"),
            build: string("BuildVersion"),
            serial: string("SerialNumber"),
            cpu: string("CPUArchitecture"),
        };
        if info.name.is_empty() {
            info.name.clone_from(&self.options.udid);
        }
        if info.cpu.is_empty() {
            info.cpu = "arm64".into();
        }
        Ok(info)
    }
}

async fn wait_for(mut flag: watch::Receiver<bool>, wait: Duration) -> bool {
    if *flag.borrow() {
        return true;
    }
    timeout(wait, async {
        while flag.changed().await.is_ok() {
            if *flag.borrow() {
                return true;
            }
        }
        false
    })
    .await
    .unwrap_or(false)
}

/// Where usbmuxd is, honouring `USBMUXD_SOCKET_ADDRESS`.
///
/// Normally this is the host's own socket and the default is right. The
/// exception is a containerised provider on macOS, where Docker cannot pass USB
/// through *or* bind-mount the host's unix socket, so usbmuxd is reached over a
/// TCP bridge instead — see `scripts/usbmuxd-bridge.ts`.
///
/// `idevice`'s own `from_env_var` handles `ip:port` and unix paths but not
/// hostnames, and the address that matters here is `host.docker.internal`. So
/// the resolution is done here, exactly as `stf-ios-provider` had to.
async fn usbmuxd_address() -> UsbmuxdAddr {
    let Ok(configured) = std::env::var("USBMUXD_SOCKET_ADDRESS") else {
        return UsbmuxdAddr::from_env_var().unwrap_or_default();
    };

    if !configured.contains(':') {
        return UsbmuxdAddr::UnixSocket(configured);
    }
    if let Ok(socket) = configured.parse::<std::net::SocketAddr>() {
        return UsbmuxdAddr::TcpSocket(socket);
    }
    // Owned rather than borrowed: the resolver's future is generic over the
    // address and a `&str` ties it to this frame.
    match tokio::net::lookup_host(configured.clone()).await {
        Ok(mut resolved) => match resolved.next() {
            Some(socket) => {
                info!(configured, %socket, "resolved usbmuxd address");
                UsbmuxdAddr::TcpSocket(socket)
            }
            None => {
                warn!(
                    configured,
                    "usbmuxd address resolved to nothing — using the default"
                );
                UsbmuxdAddr::from_env_var().unwrap_or_default()
            }
        },
        Err(err) => {
            warn!(configured, %err, "could not resolve the usbmuxd address");
            UsbmuxdAddr::from_env_var().unwrap_or_default()
        }
    }
}

/// Resolve a udid to a usbmux provider, over USB.
pub async fn usbmux_provider(udid: &str) -> Result<Box<dyn IdeviceProvider>> {
    let address = usbmuxd_address().await;
    let mut usbmuxd = address.connect(1).await.context("connect to usbmuxd")?;

    let devices = usbmuxd
        .get_devices()
        .await
        .map_err(|err| anyhow!("list usbmuxd devices: {err:?}"))?;
    let device = pick_usb(devices, udid)?;

    Ok(Box::new(device.to_provider(address, "farm-provider")))
}

/// Choose the USB entry for a udid, and refuse anything else.
///
/// usbmuxd lists a Wi-Fi-synced device **twice** — once `ConnectionType: "USB"`
/// and once `"Network"` — in no guaranteed order, and `idevice`'s own
/// `get_device` is a `find`, so which transport the whole session is built over
/// came down to list order. That is not a fallback, it is a coin toss: the
/// root-free CoreDeviceProxy tunnel this provider needs is built and tested
/// over USB, and a device that silently came up over Wi-Fi fails later, in the
/// media path, where the cause is invisible.
///
/// So: take USB when it is there, and when it is not, say so plainly rather
/// than trying the transport that does not work.
fn pick_usb(devices: Vec<UsbmuxdDevice>, udid: &str) -> Result<UsbmuxdDevice> {
    let mut network = false;
    let mut usb = None;

    for device in devices {
        if device.udid != udid {
            continue;
        }
        match device.connection_type {
            Connection::Usb => usb = Some(device),
            _ => network = true,
        }
    }

    match usb {
        Some(device) => {
            if network {
                info!(udid, "device is also paired over the network; using USB");
            }
            Ok(device)
        }
        None if network => Err(anyhow!(
            "{udid} is attached over the network only, and this provider needs USB. \
             Plug the device in, or turn off Wi-Fi sync for it."
        )),
        None => Err(anyhow!("usbmuxd has no device {udid}")),
    }
}

/// Open one RSD service over the tunnel.
///
/// `RsdService::connect_rsd` and `RsdHandshake::connect` are both generic over
/// the provider, and with `connect_to_service_port` taking `&mut self` the
/// stream they hand to `from_stream` carries the provider's borrow. That is
/// harmless inline, but it makes the enclosing future fail the
/// `Send + 'static` check at any `tokio::spawn`. Coercing the stream to a
/// `Box<dyn ReadWrite>` here — which is `'static` — pins the lifetime down at a
/// concrete point and keeps the session tasks spawnable. The service name still
/// comes from the trait, so it stays in step with the crate.
pub async fn connect_service_stream<T: RsdService>(
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
) -> Result<Box<dyn ReadWrite>> {
    let name = T::rsd_service_name().to_string();
    let port = handshake
        .services
        .get(&name)
        .ok_or_else(|| {
            // Which services a device publishes depends on its state, not only
            // its iOS version: the HID service is absent until the phone has
            // been unlocked since boot and Developer Mode is on. Naming the
            // missing service alone sends you looking for a bug in the tunnel,
            // so say what the device *is* offering — that is what tells the two
            // apart at a glance.
            let mut offered: Vec<&str> = handshake.services.keys().map(String::as_str).collect();
            offered.sort_unstable();
            anyhow!(
                "the device does not offer {name}; it offers {}",
                offered.join(", ")
            )
        })?
        .port;

    adapter
        .connect_to_service_port(port)
        .await
        .map_err(|err| anyhow!("connect to {name} on port {port}: {err:?}"))
}

/// Open an RSD service and complete its handshake.
///
/// The two steps are split because the generic must not straddle them: naming
/// the concrete `Self` at `from_stream` is what keeps the obligation
/// first-order.
macro_rules! connect_service {
    ($ty:ty, $adapter:expr, $handshake:expr) => {{
        let connected: ::anyhow::Result<$ty> = async {
            let stream =
                $crate::device::connect_service_stream::<$ty>($adapter, $handshake).await?;
            <$ty as ::idevice::RsdService>::from_stream(stream)
                .await
                .map_err(|err| {
                    ::anyhow::anyhow!(
                        "handshake with {}: {err:?}",
                        <$ty as ::idevice::RsdService>::rsd_service_name()
                    )
                })
        }
        .await;
        connected
    }};
}
pub(crate) use connect_service;

struct Supervisor {
    options: IosOptions,
    geometry: Geometry,
    ready: watch::Sender<bool>,
    media_ready: watch::Sender<bool>,
    session: Arc<Mutex<Option<Session>>>,
    generation: Arc<AtomicU64>,
    publisher: VideoPublisher,
    input: Arc<Mutex<Option<hid::InputHandle>>>,
    /// Viewers, input and cleanup. Capture follows this and nothing else.
    demand: Demand,
    /// When the last DDI mount failed, so a device that cannot mount at all is
    /// not re-personalised against Apple's servers every `RECONNECT_DELAY`.
    /// Owned by the supervisor loop, hence `&mut` rather than a lock.
    ddi_failed_at: Option<Instant>,
}

async fn supervise(mut supervisor: Supervisor) {
    loop {
        if let Err(err) = run_once(&mut supervisor).await {
            // `?err` and not `%err`: anyhow's Display prints only the outermost
            // context, so a session that died three layers down reported the
            // same sentence whatever had actually gone wrong.
            warn!(
                ?err,
                "device session ended; retrying in {}s",
                RECONNECT_DELAY.as_secs()
            );
        }

        // Publish the teardown before sleeping, so the provider marks the
        // device unavailable for the whole gap rather than only at its end.
        let _ = supervisor.ready.send(false);
        let _ = supervisor.media_ready.send(false);
        *supervisor.session.lock().await = None;
        *supervisor.input.lock().await = None;
        supervisor.publisher.mark_stopped();

        tokio::time::sleep(RECONNECT_DELAY).await;
    }
}

async fn run_once(supervisor: &mut Supervisor) -> Result<()> {
    let options = supervisor.options.clone();
    info!(udid = %options.udid, "establishing the CoreDevice tunnel");

    let provider = usbmux_provider(&options.udid).await?;
    assert_supported(&product_version(&*provider).await?)?;

    mount_ddi(supervisor, &*provider).await;

    let proxy = CoreDeviceProxy::connect(&*provider)
        .await
        .map_err(|err| anyhow!("CoreDeviceProxy: {err:?}"))?;
    let rsd_port = proxy.tunnel_info().server_rsd_port;

    let adapter = proxy
        .create_software_tunnel()
        .map_err(|err| anyhow!("software tunnel: {err:?}"))?;
    let mut adapter = adapter.to_async_handle();
    let stream = adapter
        .connect(rsd_port)
        .await
        .context("connect to the RSD port over the tunnel")?;
    let handshake = RsdHandshake::new(stream)
        .await
        .map_err(|err| anyhow!("RSD handshake: {err:?}"))?;

    let generation = supervisor.generation.fetch_add(1, Ordering::Relaxed) + 1;

    *supervisor.session.lock().await = Some(Session {
        adapter: adapter.clone(),
        handshake: handshake.clone(),
    });
    let _ = supervisor.ready.send(true);
    info!(generation, "tunnel up");

    // The tunnel alone is enough to be `ready`. Capture costs the device its
    // hardware encoder, so it waits to be asked for and stops being paid for
    // as soon as nothing is asking.
    let outcome = loop {
        supervisor.demand.wait_for_demand().await;
        if let Err(err) = run_capture(supervisor, &mut adapter, &handshake, generation).await {
            break Err(err);
        }
    };

    // Dropping the adapter shuts the userspace stack down; the next iteration
    // builds a fresh one.
    let _ = adapter.close().await;
    outcome
}

/// One capture session: media, HID, and the idle window that ends them.
///
/// `Ok(())` means nothing needs the device any more, which is not a fault and
/// leaves the tunnel alone. An error means media or HID broke, which takes the
/// whole session down — they authenticate against each other and cannot be
/// rebuilt independently.
async fn run_capture(
    supervisor: &Supervisor,
    adapter: &mut AdapterHandle,
    handshake: &RsdHandshake,
    generation: u64,
) -> Result<()> {
    info!(generation, "something needs this device — starting capture");

    // Media first: the HID surfaces authenticate against the live stream.
    let mut media = tokio::spawn(media::run(
        adapter.clone(),
        handshake.clone(),
        supervisor.options.clone(),
        supervisor.publisher.clone(),
        supervisor.geometry.clone(),
    ));

    tokio::time::sleep(HID_SETTLE).await;

    let clients = match hid::HidClients::connect(adapter, handshake).await {
        Ok(clients) => clients,
        Err(err) => {
            media.abort();
            return Err(err);
        }
    };
    let (input_handle, inputs) = hid::channel();
    let mut hid_task = tokio::spawn(clients.run(inputs));

    *supervisor.input.lock().await = Some(input_handle);
    let _ = supervisor.media_ready.send(true);
    info!(generation, "capture ready");

    // Either half failing ends the session; nothing needing the device ends
    // only the capture.
    let outcome = tokio::select! {
        result = &mut media => result.map_err(|err| anyhow!("media task panicked: {err}"))?,
        result = &mut hid_task => result.map_err(|err| anyhow!("HID task panicked: {err}"))?,
        _ = supervisor.demand.wait_for_idle(IDLE_GRACE) => {
            info!(generation, "nothing needs this device — stopping capture");
            Ok(())
        }
    };

    // Withdraw the input handle before tearing down, so a report queued now is
    // refused rather than dropped into a HID task that is going away.
    *supervisor.input.lock().await = None;
    let _ = supervisor.media_ready.send(false);

    // `media::run` has no shutdown signal — the whole file is a stream loop —
    // so the abort *is* the stop: it drops the display-service connection and
    // the RTP sockets, which is what ends the mirror session on the device.
    media.abort();
    hid_task.abort();
    supervisor.publisher.mark_stopped();

    outcome
}

/// Mount the DDI, if this device wants it mounted for it.
///
/// Deliberately infallible from the caller's point of view: a farm where the
/// operator mounts images by hand — or a device already mounted by Xcode — must
/// keep working exactly as it did, so a failure here is a warning and the tunnel
/// bring-up carries on. `connect_service_stream`'s service list stays the
/// diagnostic for a device that genuinely has nothing mounted.
async fn mount_ddi(supervisor: &mut Supervisor, provider: &dyn IdeviceProvider) {
    if !supervisor.options.auto_mount_ddi {
        return;
    }
    let Some(cache) = supervisor.options.ddi.clone() else {
        return;
    };

    match ddi::ensure_mounted(provider, &cache, supervisor.ddi_failed_at).await {
        Ok(ddi::MountOutcome::Mounted) => supervisor.ddi_failed_at = None,
        Ok(outcome) => debug!(?outcome, "developer disk image"),
        Err(err) => {
            supervisor.ddi_failed_at = Some(Instant::now());
            warn!(
                ?err,
                "could not mount the developer disk image; continuing without it"
            );
        }
    }
}

async fn product_version(provider: &dyn IdeviceProvider) -> Result<String> {
    let mut lockdown = LockdownClient::connect(provider)
        .await
        .map_err(|err| anyhow!("lockdown connect: {err:?}"))?;
    let value = lockdown
        .get_value(Some("ProductVersion"), None)
        .await
        .map_err(|err| anyhow!("read ProductVersion: {err:?}"))?;
    Ok(value.as_string().unwrap_or_default().to_owned())
}

/// This provider is iOS 17.4+ only.
///
/// Below 17.4 the root-free tunnel can only reach the device over the
/// RemotePairing/Wi-Fi path, and below 17 the CoreDevice display service does
/// not exist at all. Neither is implemented here and there is no other backend
/// to hand the device to, so fail loudly rather than half-work.
fn assert_supported(product_version: &str) -> Result<()> {
    let mut parts = product_version.split('.');
    let parsed = (|| {
        let major: u32 = parts.next()?.parse().ok()?;
        let minor: u32 = parts.next().unwrap_or("0").parse().ok()?;
        Some((major, minor))
    })();

    let Some((major, minor)) = parsed else {
        warn!(
            product_version,
            "could not parse the iOS version — continuing"
        );
        return Ok(());
    };

    if (major, minor) < (17, 4) {
        return Err(anyhow!(
            "iOS {product_version} is not supported: this provider needs 17.4+, where \
             CoreDeviceProxy builds the tunnel without root. There is no fallback path for \
             older devices."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The branch behind "no HID — the device is not streaming": a wait that
    /// runs out has to answer `false`, because the caller turns that into an
    /// error rather than dropping the report.
    #[tokio::test(start_paused = true)]
    async fn a_readiness_wait_that_runs_out_says_so() {
        let (tx, rx) = watch::channel(false);
        assert!(!wait_for(rx.clone(), Duration::from_secs(30)).await);

        let _ = tx.send(true);
        assert!(wait_for(rx, Duration::from_secs(30)).await);
    }

    /// Bring-up arriving inside the wait is the ordinary case for the first
    /// input event after an idle period.
    #[tokio::test(start_paused = true)]
    async fn a_readiness_wait_resolves_when_capture_comes_up() {
        let (tx, rx) = watch::channel(false);
        let waiting = tokio::spawn(wait_for(rx, Duration::from_secs(30)));

        tokio::time::sleep(Duration::from_secs(2)).await;
        let _ = tx.send(true);

        assert!(waiting.await.unwrap());
    }

    #[test]
    fn rejects_versions_below_the_tunnel_floor() {
        assert!(assert_supported("17.4").is_ok());
        assert!(assert_supported("18.1.1").is_ok());
        assert!(assert_supported("26.0").is_ok());
        assert!(assert_supported("17.3.1").is_err());
        assert!(assert_supported("16.7").is_err());
        // An unparseable version is a warning, not a hard stop: refusing to run
        // on a version string we simply do not recognise would be worse.
        assert!(assert_supported("").is_ok());
    }

    fn listed(udid: &str, connection_type: Connection) -> UsbmuxdDevice {
        UsbmuxdDevice {
            udid: udid.to_string(),
            device_id: 1,
            connection_type,
        }
    }

    fn network() -> Connection {
        Connection::Network(std::net::IpAddr::V4(std::net::Ipv4Addr::new(10, 0, 0, 4)))
    }

    #[test]
    fn takes_the_usb_entry_whichever_order_usbmuxd_listed_it() {
        // The bug this guards: with both entries present, `find` took whichever
        // came first, so the transport depended on usbmuxd's listing order.
        for devices in [
            vec![listed("A", Connection::Usb), listed("A", network())],
            vec![listed("A", network()), listed("A", Connection::Usb)],
        ] {
            let picked = pick_usb(devices, "A").expect("USB entry is present");
            assert_eq!(picked.connection_type, Connection::Usb);
        }
    }

    #[test]
    fn refuses_a_device_offered_only_over_the_network() {
        let err = pick_usb(vec![listed("A", network())], "A").unwrap_err();
        assert!(err.to_string().contains("network only"), "{err}");
    }

    #[test]
    fn ignores_other_devices() {
        assert!(pick_usb(vec![listed("B", Connection::Usb)], "A").is_err());
        let picked = pick_usb(
            vec![listed("B", Connection::Usb), listed("A", Connection::Usb)],
            "A",
        )
        .expect("the requested udid is present");
        assert_eq!(picked.udid, "A");
    }
}
