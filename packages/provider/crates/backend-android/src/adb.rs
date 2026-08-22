//! A client for the adb *server*'s host protocol.
//!
//! The provider never shells out to the `adb` binary. It speaks the same TCP
//! protocol adb's own client uses, which means one dependency (an adb server
//! owning the USB transport) instead of a subprocess per operation, and it
//! means `track-devices` gives us hotplug as a stream rather than a poll.
//!
//! ```text
//!   yard-provider ──TCP :5037──> adb server ──USB──> device
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

/// Reads the message body of a sync-protocol `FAIL` whose id has been consumed.
///
/// The device's own words — "Permission denied", "No such file or directory" —
/// and the only thing that tells an unreadable directory from an empty one.
async fn read_fail(socket: &mut TcpStream) -> Result<String> {
    let mut length = [0u8; 4];
    socket.read_exact(&mut length).await?;
    let mut message = vec![0u8; u32::from_le_bytes(length) as usize];
    socket.read_exact(&mut message).await?;
    Ok(String::from_utf8_lossy(&message).trim().to_owned())
}

/// The adb server's own port, and the default everywhere.
pub const DEFAULT_ADB_SERVER: &str = "127.0.0.1:5037";

/// Sync-protocol chunks are capped by adb itself; a larger DATA is rejected.
const SYNC_CHUNK: usize = 64 * 1024;

/// How long to wait for the server to answer a request.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// One `DENT` record from the sync protocol's `LIST`.
///
/// `mode` is a POSIX `st_mode`, which is what makes a directory tellable from a
/// file without a second round-trip per entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncDirEntry {
    pub name: String,
    pub mode: u32,
    pub size: u32,
    /// Seconds since the epoch. Zero on a filesystem that does not track it.
    pub mtime: u32,
}

/// `S_IFMT` — the bits of `st_mode` that say what kind of thing this is.
const S_IFMT: u32 = 0o170000;
const S_IFDIR: u32 = 0o040000;
const S_IFREG: u32 = 0o100000;

impl SyncDirEntry {
    pub fn is_dir(&self) -> bool {
        self.mode & S_IFMT == S_IFDIR
    }

    pub fn is_file(&self) -> bool {
        self.mode & S_IFMT == S_IFREG
    }
}

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

    /// One directory, over the sync subprotocol's `LIST`.
    ///
    /// `adb shell ls -la` would be the obvious alternative and is a trap: its
    /// output differs between toybox and toolbox builds and has to be
    /// re-guessed per device. `LIST` answers fixed-width records, and carries
    /// the mode and size with them — so a listing costs no extra `stat` calls.
    ///
    /// The 32-bit `size` and `mtime` are what the v1 protocol offers; a file
    /// over 4 GiB reports a truncated size here. Universally supported beats
    /// `LIS2` for a directory listing on a farm phone.
    pub async fn list(&self, serial: &str, path: &str) -> Result<Vec<SyncDirEntry>> {
        let mut stream = self.transport(serial).await?;
        stream.request("sync:").await?;
        let socket = stream.inner_mut();

        socket.write_all(b"LIST").await?;
        socket.write_all(&(path.len() as u32).to_le_bytes()).await?;
        socket.write_all(path.as_bytes()).await?;
        socket.flush().await?;

        let mut entries = Vec::new();
        loop {
            let mut id = [0u8; 4];
            socket.read_exact(&mut id).await?;
            match &id {
                b"DENT" => {
                    let mut header = [0u8; 16];
                    socket.read_exact(&mut header).await?;
                    let mode = u32::from_le_bytes(header[0..4].try_into().unwrap());
                    let size = u32::from_le_bytes(header[4..8].try_into().unwrap());
                    let mtime = u32::from_le_bytes(header[8..12].try_into().unwrap());
                    let name_len = u32::from_le_bytes(header[12..16].try_into().unwrap()) as usize;

                    let mut name = vec![0u8; name_len];
                    socket.read_exact(&mut name).await?;
                    let name = String::from_utf8_lossy(&name).into_owned();

                    // `.` and `..` are the caller's business, not the listing's
                    // — the browser gets `parent` for that.
                    if name == "." || name == ".." {
                        continue;
                    }
                    entries.push(SyncDirEntry {
                        name,
                        mode,
                        size,
                        mtime,
                    });
                }
                b"DONE" => {
                    // DONE carries a 16-byte empty stat block.
                    let mut trailer = [0u8; 16];
                    socket.read_exact(&mut trailer).await?;
                    break;
                }
                // An unreadable directory answers FAIL rather than an empty
                // DONE, which is the only reason a permission problem is
                // distinguishable from an empty folder.
                b"FAIL" => bail!("adb refused to list {path}: {}", read_fail(socket).await?),
                other => bail!("unexpected sync reply {:?}", String::from_utf8_lossy(other)),
            }
        }
        Ok(entries)
    }

    /// Copy a file off the device into `dest`, over the sync subprotocol's
    /// `RECV`. The mirror of [`Self::push`].
    ///
    /// Written straight to the file as it arrives: this is the path a 400 MB
    /// screen recording takes off a phone, and buffering it would put it in the
    /// provider's memory for no reason.
    pub async fn pull(
        &self,
        serial: &str,
        remote_path: &str,
        dest: &std::path::Path,
    ) -> Result<u64> {
        let mut stream = self.transport(serial).await?;
        stream.request("sync:").await?;
        let socket = stream.inner_mut();

        socket.write_all(b"RECV").await?;
        socket
            .write_all(&(remote_path.len() as u32).to_le_bytes())
            .await?;
        socket.write_all(remote_path.as_bytes()).await?;
        socket.flush().await?;

        let mut file = tokio::fs::File::create(dest)
            .await
            .with_context(|| format!("creating {}", dest.display()))?;
        let mut written: u64 = 0;
        let mut buf = vec![0u8; SYNC_CHUNK];

        loop {
            let mut header = [0u8; 8];
            socket.read_exact(&mut header).await?;
            let length = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;

            match &header[..4] {
                b"DATA" => {
                    let mut remaining = length;
                    while remaining > 0 {
                        let take = remaining.min(buf.len());
                        socket.read_exact(&mut buf[..take]).await?;
                        file.write_all(&buf[..take]).await?;
                        written += take as u64;
                        remaining -= take;
                    }
                }
                // DONE's "length" is the mtime, not a payload.
                b"DONE" => break,
                b"FAIL" => {
                    let mut message = vec![0u8; length];
                    socket.read_exact(&mut message).await?;
                    bail!(
                        "adb refused to read {remote_path}: {}",
                        String::from_utf8_lossy(&message).trim()
                    )
                }
                other => bail!("unexpected sync reply {:?}", String::from_utf8_lossy(other)),
            }
        }

        file.flush().await?;
        debug!(serial, remote_path, written, "pulled from device");
        Ok(written)
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

    /// Make the device's own `adbd` listen on TCP.
    ///
    /// This is a *transport service*, not a shell command. `setprop
    /// service.adb.tcp.port` plus an adbd restart is the recipe that circulates
    /// for this, and it silently does nothing without root — the property does
    /// not stick, the daemon never listens, and a forward to it connects to
    /// nothing. `tcpip:` is what `adb tcpip` itself uses and it needs no root.
    ///
    /// The device drops and re-enumerates its transport when adbd restarts, so
    /// callers must expect a brief window where the serial is unreachable.
    pub async fn tcpip(&self, serial: &str, port: u16) -> Result<()> {
        let mut stream = self.transport(serial).await?;
        stream.request(&format!("tcpip:{port}")).await?;
        // The daemon answers with a human-readable line before restarting.
        let message = stream.read_to_end().await.unwrap_or_default();
        debug!(
            serial,
            port,
            message = message.trim(),
            "adbd restarting in TCP mode"
        );
        Ok(())
    }

    /// Put `adbd` back on USB only, so the device stops listening on the
    /// network. The counterpart to [`Adb::tcpip`], and what `adb usb` does.
    pub async fn usb_only(&self, serial: &str) -> Result<()> {
        let mut stream = self.transport(serial).await?;
        stream.request("usb:").await?;
        let message = stream.read_to_end().await.unwrap_or_default();
        debug!(
            serial,
            message = message.trim(),
            "adbd restarting in USB mode"
        );
        Ok(())
    }

    /// Wait for a device to be usable, up to `timeout`.
    ///
    /// Needed after anything that restarts `adbd`: the transport disappears and
    /// re-enumerates a second or two later, and a request in that window is
    /// refused outright rather than queued.
    pub async fn wait_for_device(&self, serial: &str, timeout: Duration) -> Result<()> {
        let deadline = tokio::time::Instant::now() + timeout;
        loop {
            if let Ok(devices) = self.devices().await {
                if devices
                    .iter()
                    .any(|device| device.serial == serial && device.is_usable())
                {
                    return Ok(());
                }
            }
            if tokio::time::Instant::now() >= deadline {
                bail!("{serial} did not come back within {timeout:?}");
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    /// Open a stream to a TCP port on the *device*, over its USB transport.
    ///
    /// This is what `adb forward` does internally, without the listener adb
    /// would own: `forward`'s local socket binds the adb server's loopback, so
    /// in a container nothing outside it can ever reach the port. The provider
    /// binds its own listener instead and splices it here.
    pub async fn device_tcp(&self, serial: &str, port: u16) -> Result<TcpStream> {
        let mut stream = self.transport(serial).await?;
        stream.request(&format!("tcp:{port}")).await?;
        Ok(stream.into_inner())
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

#[cfg(test)]
mod sync_tests {
    use super::*;

    /// A fake adb server that answers one `host:transport:` and one `sync:`,
    /// then plays back a canned reply.
    ///
    /// **It writes one byte at a time.** The scrcpy reader in this crate was
    /// first written assuming a single `read()` returns a whole header, which
    /// is not something a socket promises — the bug that caused took a black
    /// screen and a 3.9 GB allocation to find. Framing readers get tested
    /// against the hostile case here.
    async fn fake_adb(reply: Vec<u8>) -> (String, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();

        let handle = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();

            // Two hex-framed host requests, each answered OKAY.
            for _ in 0..1 {
                let mut length = [0u8; 4];
                socket.read_exact(&mut length).await.unwrap();
                let n = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16).unwrap();
                let mut service = vec![0u8; n];
                socket.read_exact(&mut service).await.unwrap();
                socket.write_all(b"OKAY").await.unwrap();
            }
            // `sync:` is also hex-framed and status-answered.
            let mut length = [0u8; 4];
            socket.read_exact(&mut length).await.unwrap();
            let n = usize::from_str_radix(std::str::from_utf8(&length).unwrap(), 16).unwrap();
            let mut service = vec![0u8; n];
            socket.read_exact(&mut service).await.unwrap();
            socket.write_all(b"OKAY").await.unwrap();

            // The sync command the client sends: id + le32 length + path.
            let mut header = [0u8; 8];
            socket.read_exact(&mut header).await.unwrap();
            let path_len = u32::from_le_bytes(header[4..8].try_into().unwrap()) as usize;
            let mut path = vec![0u8; path_len];
            socket.read_exact(&mut path).await.unwrap();

            for byte in &reply {
                socket.write_all(&[*byte]).await.unwrap();
                socket.flush().await.unwrap();
            }
            let _ = socket.shutdown().await;
            path
        });

        (addr, handle)
    }

    fn dent(mode: u32, size: u32, mtime: u32, name: &str) -> Vec<u8> {
        let mut out = b"DENT".to_vec();
        out.extend_from_slice(&mode.to_le_bytes());
        out.extend_from_slice(&size.to_le_bytes());
        out.extend_from_slice(&mtime.to_le_bytes());
        out.extend_from_slice(&(name.len() as u32).to_le_bytes());
        out.extend_from_slice(name.as_bytes());
        out
    }

    #[tokio::test]
    async fn list_reads_dents_a_byte_at_a_time_and_drops_dot_entries() {
        let mut reply = Vec::new();
        reply.extend(dent(0o040755, 0, 0, "."));
        reply.extend(dent(0o040755, 0, 0, ".."));
        reply.extend(dent(0o040755, 4096, 1_754_524_800, "Camera"));
        reply.extend(dent(0o100644, 2_489_301, 1_754_524_801, "IMG_0001.jpg"));
        reply.extend(b"DONE");
        reply.extend([0u8; 16]);

        let (addr, server) = fake_adb(reply).await;
        let entries = Adb::new(addr).list("serial", "/sdcard/DCIM").await.unwrap();

        assert_eq!(server.await.unwrap(), b"/sdcard/DCIM");
        assert_eq!(entries.len(), 2, "`.` and `..` must not reach the browser");

        assert!(entries[0].is_dir());
        assert_eq!(entries[0].name, "Camera");

        assert!(entries[1].is_file());
        assert_eq!(entries[1].size, 2_489_301);
        assert_eq!(entries[1].mtime, 1_754_524_801);
    }

    #[tokio::test]
    async fn an_unreadable_directory_surfaces_the_devices_own_message() {
        let message = b"Permission denied";
        let mut reply = b"FAIL".to_vec();
        reply.extend_from_slice(&(message.len() as u32).to_le_bytes());
        reply.extend_from_slice(message);

        let (addr, _server) = fake_adb(reply).await;
        let err = Adb::new(addr)
            .list("serial", "/data/data")
            .await
            .expect_err("an unreadable directory must not read as an empty one");

        assert!(format!("{err:#}").contains("Permission denied"), "{err:#}");
    }

    #[tokio::test]
    async fn pull_reassembles_data_chunks_into_the_file() {
        let first = vec![0xABu8; 300];
        let second = vec![0xCDu8; 120];

        let mut reply = b"DATA".to_vec();
        reply.extend_from_slice(&(first.len() as u32).to_le_bytes());
        reply.extend_from_slice(&first);
        reply.extend_from_slice(b"DATA");
        reply.extend_from_slice(&(second.len() as u32).to_le_bytes());
        reply.extend_from_slice(&second);
        // DONE's trailing 4 bytes are an mtime, not a payload — reading them as
        // a length would swallow the next reply.
        reply.extend_from_slice(b"DONE");
        reply.extend_from_slice(&1_754_524_801u32.to_le_bytes());

        let dest = std::env::temp_dir().join(format!("farm-pull-test-{}", std::process::id()));
        let (addr, _server) = fake_adb(reply).await;
        let written = Adb::new(addr)
            .pull("serial", "/sdcard/x.bin", &dest)
            .await
            .unwrap();

        assert_eq!(written, 420);
        let contents = std::fs::read(&dest).unwrap();
        std::fs::remove_file(&dest).ok();
        assert_eq!(contents.len(), 420);
        assert_eq!(&contents[..300], &first[..]);
        assert_eq!(&contents[300..], &second[..]);
    }

    #[tokio::test]
    async fn a_refused_pull_leaves_an_error_not_a_truncated_file() {
        let message = b"remote object '/data/data/x' does not exist";
        let mut reply = b"FAIL".to_vec();
        reply.extend_from_slice(&(message.len() as u32).to_le_bytes());
        reply.extend_from_slice(message);

        let dest = std::env::temp_dir().join(format!("farm-pull-fail-{}", std::process::id()));
        let (addr, _server) = fake_adb(reply).await;
        let err = Adb::new(addr)
            .pull("serial", "/data/data/x", &dest)
            .await
            .expect_err("a refused pull must not look like an empty file");

        std::fs::remove_file(&dest).ok();
        assert!(format!("{err:#}").contains("does not exist"), "{err:#}");
    }
}
