//! The device session: the root-free RSD tunnel, the media stream, and HID.
//!
//! Ported from `stf-ios-provider/src/device/mod.rs`. The supervision shape is
//! unchanged — one task rebuilds tunnel, media and HID together, because they
//! are not independent — but readiness now feeds `DeviceBackend::is_healthy`
//! instead of STF's `TemporarilyUnavailable`.
//!
//! usbmuxd is normally a local unix socket — the host's on a bare-metal
//! provider, the container's own on Linux under Docker.
//! `USBMUXD_SOCKET_ADDRESS` exists for the one case where it cannot be: a
//! provider container on macOS, where Docker passes no USB through and usbmuxd
//! is reached over a TCP bridge on the host.
//!
//! One supervisor task rebuilds the whole session forever. Everything below the
//! tunnel is torn down and recreated together, because the pieces are not
//! independent: the HID surfaces authenticate against the live media stream, so
//! a media restart invalidates them, and a tunnel drop invalidates everything.
//!
//! [`DeviceHost::is_ready`] tracks whether a session is up. The supervisor
//! polls it through `is_healthy`, so a device that is rebooting or unplugged is
//! reported `unhealthy` upstream rather than silently accepting input into a
//! void.

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
    session: Arc<Mutex<Option<Session>>>,
    generation: Arc<AtomicU64>,

    input: Arc<Mutex<Option<hid::InputHandle>>>,
}

impl DeviceHost {
    /// Start the supervisor. It runs until the process exits.
    pub fn spawn(options: IosOptions, publisher: VideoPublisher, geometry: Geometry) -> Self {
        let (ready_tx, ready_rx) = watch::channel(false);
        let session = Arc::new(Mutex::new(None));
        let input = Arc::new(Mutex::new(None));
        let generation = Arc::new(AtomicU64::new(0));

        let host = Self {
            options: options.clone(),
            ready: ready_rx,
            session: session.clone(),
            generation: generation.clone(),
            input: input.clone(),
        };

        tokio::spawn(supervise(Supervisor {
            options,
            geometry,
            ready: ready_tx,
            session,
            generation,
            publisher,
            input,
            ddi_failed_at: None,
        }));

        host
    }

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

    /// Wait for a session to come up, up to `wait`.
    pub async fn wait_ready(&self, wait: Duration) -> bool {
        let mut ready = self.ready.clone();
        if *ready.borrow() {
            return true;
        }
        timeout(wait, async {
            while ready.changed().await.is_ok() {
                if *ready.borrow() {
                    return true;
                }
            }
            false
        })
        .await
        .unwrap_or(false)
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

/// What `USBMUXD_SOCKET_ADDRESS` named, before any name resolution.
enum Configured {
    Unix(String),
    Tcp(std::net::SocketAddr),
    Host(String),
}

/// Split libusbmuxd's env-var syntax into something addressable.
///
/// The documented unix form is `UNIX:/path`, which contains a colon — reading
/// one as a `host:port` and resolving it is how a perfectly good socket path
/// used to end up silently discarded for the default. A bare path is accepted
/// too, since that is what this codebase's own examples used.
fn parse_usbmuxd_address(configured: &str) -> Configured {
    let trimmed = configured.trim();

    if let Some((scheme, rest)) = trimmed.split_once(':') {
        if scheme.eq_ignore_ascii_case("unix") {
            return Configured::Unix(rest.to_string());
        }
    }
    if trimmed.starts_with('/') || !trimmed.contains(':') {
        return Configured::Unix(trimmed.to_string());
    }
    match trimmed.parse::<std::net::SocketAddr>() {
        Ok(socket) => Configured::Tcp(socket),
        Err(_) => Configured::Host(trimmed.to_string()),
    }
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

    let host = match parse_usbmuxd_address(&configured) {
        Configured::Unix(path) => return UsbmuxdAddr::UnixSocket(path),
        Configured::Tcp(socket) => return UsbmuxdAddr::TcpSocket(socket),
        // Owned rather than borrowed: the resolver's future is generic over the
        // address and a `&str` ties it to this frame.
        Configured::Host(host) => host,
    };

    match tokio::net::lookup_host(host).await {
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

    Ok(Box::new(device.to_provider(address, "yard-provider")))
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
    session: Arc<Mutex<Option<Session>>>,
    generation: Arc<AtomicU64>,
    publisher: VideoPublisher,
    input: Arc<Mutex<Option<hid::InputHandle>>>,
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
    info!(generation, "tunnel up");

    // Media first: the HID surfaces authenticate against the live stream.
    let media = tokio::spawn(media::run(
        adapter.clone(),
        handshake.clone(),
        options.clone(),
        supervisor.publisher.clone(),
        supervisor.geometry.clone(),
    ));

    tokio::time::sleep(HID_SETTLE).await;

    let clients = hid::HidClients::connect(&mut adapter, &handshake).await?;
    let (input_handle, inputs) = hid::channel();
    let hid_task = tokio::spawn(clients.run(inputs));

    *supervisor.session.lock().await = Some(Session {
        adapter: adapter.clone(),
        handshake: handshake.clone(),
    });
    *supervisor.input.lock().await = Some(input_handle);
    let _ = supervisor.ready.send(true);
    info!(generation, "device session ready");

    // Either half failing ends the session.
    let outcome = tokio::select! {
        result = media => result.map_err(|err| anyhow!("media task panicked: {err}"))?,
        result = hid_task => result.map_err(|err| anyhow!("HID task panicked: {err}"))?,
    };

    // Dropping the adapter shuts the userspace stack down; the next iteration
    // builds a fresh one.
    let _ = adapter.close().await;
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

/// This provider is iOS 27+ only.
///
/// The root-free tunnel needs CoreDeviceProxy, which Apple shipped in 17.4 —
/// but real hardware testing (an iPhone 13) only worked reliably from iOS 27.
/// Below that, whatever the version of CoreDeviceProxy shipped has not held up
/// in practice, and there is no other backend to hand the device to, so fail
/// loudly rather than half-work.
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

    if (major, minor) < (27, 0) {
        return Err(anyhow!(
            "iOS {product_version} is not supported: this provider needs 27+, where \
             CoreDeviceProxy reliably builds the tunnel without root. There is no fallback path \
             for older devices."
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_versions_below_the_tunnel_floor() {
        assert!(assert_supported("27.0").is_ok());
        assert!(assert_supported("27.1.1").is_ok());
        assert!(assert_supported("28.0").is_ok());
        assert!(assert_supported("26.1").is_err());
        assert!(assert_supported("17.4").is_err());
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
    fn reads_libusbmuxds_unix_form_as_a_path_not_a_hostname() {
        // The bug this guards: `UNIX:/run/usbmuxd` contains a colon, so it was
        // treated as host:port, failed to resolve, and fell back to the default
        // socket — silently ignoring the one setting that had been made.
        for configured in ["UNIX:/run/usbmuxd", "unix:/run/usbmuxd", "/run/usbmuxd"] {
            match parse_usbmuxd_address(configured) {
                Configured::Unix(path) => assert_eq!(path, "/run/usbmuxd"),
                _ => panic!("{configured} is a unix socket"),
            }
        }
    }

    #[test]
    fn keeps_telling_a_literal_address_from_a_hostname() {
        match parse_usbmuxd_address("127.0.0.1:27015") {
            Configured::Tcp(socket) => assert_eq!(socket.port(), 27015),
            _ => panic!("an ip:port is a TCP address"),
        }
        match parse_usbmuxd_address("host.docker.internal:27015") {
            Configured::Host(host) => assert_eq!(host, "host.docker.internal:27015"),
            _ => panic!("a hostname needs resolving"),
        }
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
