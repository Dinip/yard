//! A client for the adb *server*'s host protocol.
//!
//! The provider never shells out to the `adb` binary. It speaks the same TCP
//! protocol adb's own client uses, which means one dependency (an adb server
//! owning the USB transport) instead of a subprocess per operation, and it
//! means `track-devices` gives us hotplug as a stream rather than a poll.
//!
//! ```text
//!   farm-provider ──TCP :5037──> adb server ──USB──> device
//! ```
//!
//! The server's address is configuration, not a constant, because the two
//! deployments differ: on a Linux provider host adb is bundled in the image
//! with the USB bus passed through, so it is local; on macOS, where Docker
//! cannot pass USB through at all, the provider points at the host's adb
//! server instead.
//!
//! ## Two framings, and the trap in that
//!
//! The **host protocol** prefixes payloads with a 4-digit *hex* length. The
//! **sync subprotocol** — used for file transfer, after `sync:` — prefixes with
//! a 4-byte *little-endian* length. Mixing them up produces a hang rather than
//! an error, because each side sits waiting for bytes the other will never
//! send.

use std::time::Duration;

use anyhow::{bail, Context as _, Result};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tracing::debug;

/// The adb server's own port, and the default everywhere.
pub const DEFAULT_ADB_SERVER: &str = "127.0.0.1:5037";

/// Sync-protocol chunks are capped by adb itself; a larger DATA is rejected.
const SYNC_CHUNK: usize = 64 * 1024;

/// How long to wait for the server to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdbDevice {
    pub serial: String,
    /// `device`, `offline`, `unauthorized`, `bootloader`, …
    pub state: String,
    pub model: Option<String>,
}

impl AdbDevice {
    /// Only `device` means usable. `unauthorized` in particular looks connected
    /// and answers nothing — surfacing it as present would mean handing a user
    /// a device that silently refuses every command.
    pub fn is_usable(&self) -> bool {
        self.state == "device"
    }
}

/// A connection to the adb server, before or after it has been switched to a
/// device transport.
pub struct AdbStream {
    stream: TcpStream,
}

impl AdbStream {
    pub async fn connect(server: &str) -> Result<Self> {
        let stream = tokio::time::timeout(REQUEST_TIMEOUT, TcpStream::connect(server))
            .await
            .with_context(|| format!("connecting to the adb server at {server}"))?
            .with_context(|| format!("connecting to the adb server at {server}"))?;
        // Input latency is the whole product here; Nagle would coalesce touch
        // reports into 40 ms batches.
        stream.set_nodelay(true).ok();
        Ok(Self { stream })
    }

    /// Send one host-protocol request and read the status word.
    pub async fn request(&mut self, service: &str) -> Result<()> {
        let framed = format!("{:04x}{service}", service.len());
        self.stream
            .write_all(framed.as_bytes())
            .await
            .with_context(|| format!("sending adb request {service:?}"))?;
        self.status(service).await
    }

    async fn status(&mut self, context: &str) -> Result<()> {
        let mut status = [0u8; 4];
        tokio::time::timeout(REQUEST_TIMEOUT, self.stream.read_exact(&mut status))
            .await
            .with_context(|| format!("adb server did not answer {context:?}"))?
            .with_context(|| format!("reading the adb status for {context:?}"))?;

        match &status {
            b"OKAY" => Ok(()),
            b"FAIL" => {
                let message = self.payload().await.unwrap_or_default();
                bail!("adb refused {context:?}: {message}")
            }
            other => bail!(
                "adb answered {context:?} with {:?}, which is not a status word",
                String::from_utf8_lossy(other)
            ),
        }
    }

    /// Read a hex-length-prefixed payload.
    pub async fn payload(&mut self) -> Result<String> {
        let mut header = [0u8; 4];
        self.stream.read_exact(&mut header).await?;
        let length = usize::from_str_radix(std::str::from_utf8(&header)?, 16)
            .context("adb sent a non-hex length prefix")?;

        let mut body = vec![0u8; length];
        self.stream.read_exact(&mut body).await?;
        Ok(String::from_utf8_lossy(&body).into_owned())
    }

    /// Read everything until the far end closes. For `shell:`, which streams
    /// its output and then hangs up.
    pub async fn read_to_end(&mut self) -> Result<String> {
        let mut out = Vec::new();
        self.stream.read_to_end(&mut out).await?;
        Ok(String::from_utf8_lossy(&out).into_owned())
    }

    /// The raw socket, for callers that take the stream over — scrcpy's video
    /// and control sockets, and the adb transport proxy.
    pub fn into_inner(self) -> TcpStream {
        self.stream
    }

    pub fn inner_mut(&mut self) -> &mut TcpStream {
        &mut self.stream
    }
}

/// The adb server, addressed by host:port.
#[derive(Clone, Debug)]
pub struct Adb {
    server: String,
}

impl Adb {
    pub fn new(server: impl Into<String>) -> Self {
        Self {
            server: server.into(),
        }
    }

    pub fn server(&self) -> &str {
        &self.server
    }

    /// Protocol version of the running server, and the cheapest liveness check
    /// there is — a failure here means no server, not no device.
    pub async fn version(&self) -> Result<u32> {
        let mut stream = AdbStream::connect(&self.server).await?;
        stream.request("host:version").await?;
        let payload = stream.payload().await?;
        u32::from_str_radix(payload.trim(), 16).context("parsing the adb server version")
    }

    /// Every device the server knows about, in `devices -l` form.
    pub async fn devices(&self) -> Result<Vec<AdbDevice>> {
        let mut stream = AdbStream::connect(&self.server).await?;
        stream.request("host:devices-l").await?;
        Ok(parse_devices(&stream.payload().await?))
    }

    /// A connection switched to one device's transport.
    ///
    /// Everything after this — `shell:`, `sync:`, scrcpy's sockets — is spoken
    /// to the device rather than the server, on this same socket.
    pub async fn transport(&self, serial: &str) -> Result<AdbStream> {
        let mut stream = AdbStream::connect(&self.server).await?;
        stream
            .request(&format!("host:transport:{serial}"))
            .await
            .with_context(|| format!("no adb transport for {serial}"))?;
        Ok(stream)
    }

    /// Run a shell command and collect its output.
    ///
    /// Uses the plain `shell:` service, not `shell,v2:`: v2 multiplexes stdout,
    /// stderr and the exit code into a framed stream, and nothing here needs
    /// them separated enough to pay for the framing.
    pub async fn shell(&self, serial: &str, command: &str) -> Result<String> {
        let mut stream = self.transport(serial).await?;
        stream.request(&format!("shell:{command}")).await?;
        stream.read_to_end().await
    }

    /// A long-lived shell whose output the caller streams itself.
    pub async fn shell_stream(&self, serial: &str, command: &str) -> Result<AdbStream> {
        let mut stream = self.transport(serial).await?;
        stream.request(&format!("shell:{command}")).await?;
        Ok(stream)
    }

    /// Push bytes to a path on the device, over the sync subprotocol.
    ///
    /// In-memory rather than streamed from a file because the one thing this
    /// pushes is the embedded scrcpy server, which is already in the binary.
    pub async fn push(
        &self,
        serial: &str,
        remote_path: &str,
        data: &[u8],
        mode: u32,
    ) -> Result<()> {
        let mut stream = self.transport(serial).await?;
        stream.request("sync:").await?;
        let socket = stream.inner_mut();

        // SEND's argument is "path,mode" — and from here on lengths are
        // little-endian binary, not the hex the host protocol uses.
        let target = format!("{remote_path},{mode}");
        socket.write_all(b"SEND").await?;
        socket
            .write_all(&(target.len() as u32).to_le_bytes())
            .await?;
        socket.write_all(target.as_bytes()).await?;

        for chunk in data.chunks(SYNC_CHUNK) {
            socket.write_all(b"DATA").await?;
            socket
                .write_all(&(chunk.len() as u32).to_le_bytes())
                .await?;
            socket.write_all(chunk).await?;
        }

        let mtime = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|since| since.as_secs() as u32)
            .unwrap_or(0);
        socket.write_all(b"DONE").await?;
        socket.write_all(&mtime.to_le_bytes()).await?;
        socket.flush().await?;

        let mut reply = [0u8; 8];
        socket.read_exact(&mut reply).await?;
        match &reply[..4] {
            b"OKAY" => {
                debug!(serial, remote_path, bytes = data.len(), "pushed to device");
                Ok(())
            }
            b"FAIL" => {
                let length = u32::from_le_bytes([reply[4], reply[5], reply[6], reply[7]]) as usize;
                let mut message = vec![0u8; length];
                socket.read_exact(&mut message).await?;
                bail!(
                    "adb refused the push to {remote_path}: {}",
                    String::from_utf8_lossy(&message)
                )
            }
            other => bail!("unexpected sync reply {:?}", String::from_utf8_lossy(other)),
        }
    }

    /// Ask the device to listen on `remote` and forward to the provider.
    ///
    /// `reverse:` runs on the *device* transport, so it must be requested on a
    /// connection already switched to that device.
    pub async fn reverse(&self, serial: &str, remote: &str, local: &str) -> Result<()> {
        let mut stream = self.transport(serial).await?;
        stream
            .request(&format!("reverse:forward:{remote};{local}"))
            .await?;
        // `reverse:forward:` answers with a second status word once the device
        // has actually bound; without reading it a caller can connect before
        // the listener exists.
        stream.status("reverse:forward").await
    }

    pub async fn reverse_remove(&self, serial: &str, remote: &str) -> Result<()> {
        let mut stream = self.transport(serial).await?;
        stream
            .request(&format!("reverse:killforward:{remote}"))
            .await?;
        stream.status("reverse:killforward").await
    }

    /// Expose a device's adb transport on a TCP port of the provider host, for
    /// remote debugging from a developer's machine.
    pub async fn forward(&self, serial: &str, local_port: u16) -> Result<()> {
        let mut stream = AdbStream::connect(&self.server).await?;
        stream
            .request(&format!(
                "host-serial:{serial}:forward:tcp:{local_port};tcp:5555"
            ))
            .await?;
        stream.status("forward").await
    }

    pub async fn forward_remove(&self, serial: &str, local_port: u16) -> Result<()> {
        let mut stream = AdbStream::connect(&self.server).await?;
        stream
            .request(&format!(
                "host-serial:{serial}:killforward:tcp:{local_port}"
            ))
            .await
    }
}

/// Parse the `host:devices-l` payload.
///
/// Lines are `serial<TAB>state key:value key:value…`, and a device that is
/// merely *present* — unauthorized, offline, still in bootloader — appears here
/// exactly like a usable one, so the state word is the only thing that says
/// whether it can be driven.
pub fn parse_devices(payload: &str) -> Vec<AdbDevice> {
    payload
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let serial = fields.next()?.to_owned();
            let state = fields.next()?.to_owned();
            let model = fields.find_map(|field| {
                field
                    .strip_prefix("model:")
                    .map(|model| model.replace('_', " "))
            });
            Some(AdbDevice {
                serial,
                state,
                model,
            })
        })
        .collect()
}

/// Watch the server's device list, calling `on_change` with every snapshot.
///
/// `host:track-devices` pushes a fresh *whole list* on every change, which is
/// the same reconcile-don't-patch stance the coordinator takes: a dropped or
/// reordered message can never leave the two sides permanently disagreeing.
pub async fn track_devices<F>(adb: Adb, mut on_change: F) -> Result<()>
where
    F: FnMut(Vec<AdbDevice>) + Send,
{
    let mut stream = AdbStream::connect(adb.server()).await?;
    stream.request("host:track-devices-l").await?;

    loop {
        // No timeout on purpose: between hotplugs this socket is legitimately
        // silent for hours.
        let mut header = [0u8; 4];
        stream.inner_mut().read_exact(&mut header).await?;
        let length = usize::from_str_radix(std::str::from_utf8(&header)?, 16)
            .context("track-devices sent a non-hex length")?;

        let mut body = vec![0u8; length];
        stream.inner_mut().read_exact(&mut body).await?;
        on_change(parse_devices(&String::from_utf8_lossy(&body)));
    }
}

/// Turn a `getprop` dump into a lookup table.
///
/// One `getprop` round-trip beats one per property: each is a full transport
/// setup, and the poll loop reads half a dozen of them.
pub fn parse_getprop(dump: &str) -> std::collections::HashMap<String, String> {
    dump.lines()
        .filter_map(|line| {
            let line = line.trim();
            let rest = line.strip_prefix('[')?;
            let (key, rest) = rest.split_once("]: [")?;
            let value = rest.strip_suffix(']')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect()
}

impl std::fmt::Debug for AdbStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdbStream").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_devices_with_and_without_extras() {
        let devices = parse_devices(
            "R5CY82FT35T            device usb:1048576X product:pa2qxeea model:SM_S936B device:pa2q\n\
             emulator-5554          device product:sdk_gphone64 model:sdk_gphone64_arm64\n\
             0123456789ABCDEF       unauthorized usb:1048577X\n",
        );

        assert_eq!(devices.len(), 3);
        assert_eq!(devices[0].serial, "R5CY82FT35T");
        assert_eq!(devices[0].state, "device");
        // Underscores are adb's escaping of spaces in the model name.
        assert_eq!(devices[0].model.as_deref(), Some("SM S936B"));
        assert!(devices[0].is_usable());

        // Present but undrivable: it answers nothing until the user taps
        // "allow", so it must not be offered as a device.
        assert_eq!(devices[2].state, "unauthorized");
        assert!(!devices[2].is_usable());
        assert_eq!(devices[2].model, None);
    }

    #[test]
    fn an_empty_device_list_is_not_an_error() {
        assert!(parse_devices("").is_empty());
        assert!(parse_devices("\n").is_empty());
    }

    #[test]
    fn parses_getprop_dumps() {
        let props = parse_getprop(
            "[ro.build.version.release]: [15]\n\
             [ro.build.version.sdk]: [35]\n\
             [ro.product.model]: [SM-S936B]\n\
             [persist.sys.locale]: []\n",
        );

        assert_eq!(props.get("ro.build.version.sdk").unwrap(), "35");
        assert_eq!(props.get("ro.product.model").unwrap(), "SM-S936B");
        // An empty value is a real answer, not a missing key.
        assert_eq!(props.get("persist.sys.locale").unwrap(), "");
        assert_eq!(props.get("nonsense"), None);
    }
}
