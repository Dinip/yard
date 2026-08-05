//! The scrcpy server: pushing it, starting it, and reading its two sockets.
//!
//! The server jar is **embedded in this binary** and pushed to the device at
//! session start. It runs on the phone under `app_process`; nothing is
//! installed on the provider host, and there is no vendor file to ship next to
//! the binary or path to configure.
//!
//! `tunnel_forward=true` makes the *server* listen on a device-side abstract
//! socket and we dial in through the adb transport. The alternative —
//! `adb reverse` — would mean allocating and tracking a host port per device
//! for no benefit, since the adb transport can open a `localabstract:` socket
//! directly.
//!
//! ## The wire, as observed rather than assumed
//!
//! scrcpy's protocol is documented only by its own source, and it changes
//! between releases, so this layout was read off a real device by
//! `examples/scrcpy_probe.rs` rather than inferred:
//!
//! ```text
//!   1  byte   dummy (0x00), so a client can detect a failed connection
//!   64 bytes  device name, UTF-8, NUL-padded
//!   4  bytes  codec id, big-endian — 0x68323634 "h264"
//!   4  bytes  flags (bit 31 set)
//!   4  bytes  width
//!   4  bytes  height
//!   then, per packet:
//!   8  bytes  pts and flags, big-endian: bit 63 session, 62 config, 61 key
//!   4  bytes  payload size
//!   n  bytes  Annex-B payload
//! ```

use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use provider_core::video::{AccessUnit, CodecDescription, VideoPublisher};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpStream;
use tracing::{debug, info, warn};

use crate::adb::{Adb, AdbStream};
use crate::h264;

/// The vendored server, embedded so a provider host needs nothing but adb.
const SERVER_JAR: &[u8] = include_bytes!("../../../vendor/scrcpy-server-v4.1");

/// Must match the jar exactly: the server refuses a client that claims another
/// version, which is the failure mode you want when the pin drifts.
const SERVER_VERSION: &str = "4.1";

const REMOTE_PATH: &str = "/data/local/tmp/farm-scrcpy-server.jar";

/// How long the server gets to bind its socket before we dial in.
const BIND_TIMEOUT: Duration = Duration::from_secs(10);

/// `pts_and_flags` bits. Not an enum: they are flags on a 64-bit field that
/// also carries the timestamp.
const PACKET_FLAG_CONFIG: u64 = 1 << 62;
const PACKET_FLAG_KEY_FRAME: u64 = 1 << 61;

/// H.264. The only codec this backend asks for — `hev1` would save bandwidth,
/// but H.264 is what every Android encoder and every browser agrees on.
const CODEC_H264: u32 = 0x6832_3634;

/// Session ids only have to be unique among live sessions on one device.
static NEXT_SCID: AtomicU32 = AtomicU32::new(1);

pub struct ScrcpySession {
    /// Held open for the lifetime of the session: closing it kills the server
    /// process on the device, which is the only cleanup that matters.
    _server: AdbStream,
    pub video: TcpStream,
    pub control: TcpStream,
    pub device_name: String,
    pub width: i64,
    pub height: i64,
}

/// Options that shape the encode. Everything else scrcpy offers is left at its
/// default on purpose — each one is a way for a device to behave differently
/// from every other device in the farm.
#[derive(Clone, Debug)]
pub struct ScrcpyOptions {
    pub max_size: u32,
    pub bit_rate: u32,
    pub max_fps: u32,
}

impl Default for ScrcpyOptions {
    fn default() -> Self {
        Self {
            // A phone's native panel is far more than a browser window shows,
            // and every extra pixel is bandwidth and encode latency.
            max_size: 1280,
            bit_rate: 8_000_000,
            max_fps: 60,
        }
    }
}

impl ScrcpySession {
    /// Push the server, start it, connect both sockets, and read the handshake.
    pub async fn start(adb: &Adb, serial: &str, options: &ScrcpyOptions) -> Result<Self> {
        adb.push(serial, REMOTE_PATH, SERVER_JAR, 0o644)
            .await
            .context("pushing the scrcpy server")?;

        let scid = NEXT_SCID.fetch_add(1, Ordering::Relaxed) & 0x7FFF_FFFF;
        let command = format!(
            "CLASSPATH={REMOTE_PATH} app_process / com.genymobile.scrcpy.Server {SERVER_VERSION} \
             scid={scid:08x} log_level=info video=true audio=false control=true \
             tunnel_forward=true video_codec=h264 max_size={} video_bit_rate={} max_fps={} \
             cleanup=true",
            options.max_size, options.bit_rate, options.max_fps
        );

        let server = adb
            .shell_stream(serial, &command)
            .await
            .context("starting the scrcpy server")?;

        let socket_name = format!("localabstract:scrcpy_{scid:08x}");
        let video = dial(adb, serial, &socket_name).await?;
        let control = dial(adb, serial, &socket_name).await?;

        let mut session = Self {
            _server: server,
            video,
            control,
            device_name: String::new(),
            width: 0,
            height: 0,
        };
        session.read_handshake().await?;

        info!(
            serial,
            device = %session.device_name,
            width = session.width,
            height = session.height,
            "scrcpy session up"
        );
        Ok(session)
    }

    async fn read_handshake(&mut self) -> Result<()> {
        let mut dummy = [0u8; 1];
        self.video
            .read_exact(&mut dummy)
            .await
            .context("reading the scrcpy dummy byte")?;

        let mut name = [0u8; 64];
        self.video.read_exact(&mut name).await?;
        let end = name
            .iter()
            .position(|&byte| byte == 0)
            .unwrap_or(name.len());
        self.device_name = String::from_utf8_lossy(&name[..end]).into_owned();

        let mut codec = [0u8; 4];
        self.video.read_exact(&mut codec).await?;
        let codec = u32::from_be_bytes(codec);
        if codec != CODEC_H264 {
            bail!(
                "the device negotiated codec {codec:#010x}, not h264 — refusing rather than \
                 feeding the browser a stream it was not told about"
            );
        }

        let mut meta = [0u8; 12];
        self.video.read_exact(&mut meta).await?;
        self.width = u32::from_be_bytes(meta[4..8].try_into().unwrap()) as i64;
        self.height = u32::from_be_bytes(meta[8..12].try_into().unwrap()) as i64;
        Ok(())
    }
}

async fn dial(adb: &Adb, serial: &str, socket_name: &str) -> Result<TcpStream> {
    // The server binds a moment after `app_process` starts; there is no signal
    // for it, so retry rather than sleeping a guessed interval.
    let deadline = tokio::time::Instant::now() + BIND_TIMEOUT;
    let mut attempt = 0;
    loop {
        attempt += 1;
        match try_dial(adb, serial, socket_name).await {
            Ok(stream) => return Ok(stream),
            Err(err) if tokio::time::Instant::now() < deadline => {
                debug!(attempt, error = %err, "scrcpy socket not up yet");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(err) => {
                return Err(err.context(format!(
                    "the scrcpy server never accepted a connection on {socket_name}"
                )))
            }
        }
    }
}

async fn try_dial(adb: &Adb, serial: &str, socket_name: &str) -> Result<TcpStream> {
    let mut stream = adb.transport(serial).await?;
    stream.request(socket_name).await?;
    Ok(stream.into_inner())
}

/// One video packet, as the server frames it.
struct Packet {
    is_config: bool,
    is_key: bool,
    payload: Vec<u8>,
}

async fn read_packet(video: &mut TcpStream) -> Result<Packet> {
    let mut header = [0u8; 12];
    video.read_exact(&mut header).await?;

    let pts_and_flags = u64::from_be_bytes(header[..8].try_into().unwrap());
    let size = u32::from_be_bytes(header[8..].try_into().unwrap()) as usize;

    let mut payload = vec![0u8; size];
    video.read_exact(&mut payload).await?;

    Ok(Packet {
        is_config: pts_and_flags & PACKET_FLAG_CONFIG != 0,
        is_key: pts_and_flags & PACKET_FLAG_KEY_FRAME != 0,
        payload,
    })
}

/// Read the video socket until it ends, publishing access units.
///
/// The config packet is never forwarded as a frame: its parameter sets go out
/// of band as avcC, and every media packet is rewritten from Annex-B to
/// length-prefixed NALUs. See [`crate::h264`] for why.
pub async fn pump_video(mut video: TcpStream, publisher: VideoPublisher) -> Result<()> {
    let mut announced: Option<CodecDescription> = None;

    loop {
        let packet = read_packet(&mut video).await.context("reading video")?;

        if packet.is_config {
            let Some(sets) = h264::ParameterSets::parse(&packet.payload) else {
                warn!("config packet carried no usable SPS/PPS — ignoring");
                continue;
            };
            let (Some(codec), Some(description)) = (sets.codec_string(), sets.avcc()) else {
                warn!("could not build an avcC from the config packet");
                continue;
            };

            let next = CodecDescription { codec, description };
            // The server repeats the config packet before every keyframe;
            // re-announcing it would make every viewer rebuild its decoder
            // several times a second.
            if announced.as_ref() != Some(&next) {
                info!(codec = %next.codec, "codec announced");
                publisher.set_codec(next.clone());
                announced = Some(next);
            }
            continue;
        }

        if announced.is_none() {
            // Nothing can decode this yet, and a viewer that received it would
            // be decoding against parameter sets it never got.
            continue;
        }

        publisher.publish(AccessUnit {
            data: h264::to_length_prefixed(&packet.payload),
            is_key: packet.is_key,
        });
    }
}

/// Ask the encoder for a fresh IDR.
///
/// `RESET_VIDEO` makes the server tear the encoder down and start a new one,
/// which re-sends the config packet and an IDR — the only way scrcpy offers to
/// re-anchor a decoder that lost its reference state.
pub async fn request_keyframe(control: &mut TcpStream) -> Result<()> {
    control
        .write_all(&[control_message::RESET_VIDEO])
        .await
        .map_err(|err| anyhow!("requesting a keyframe: {err}"))
}

/// The control socket's message types, from scrcpy's `ControlMessage`.
pub mod control_message {
    pub const INJECT_KEYCODE: u8 = 0;
    pub const INJECT_TEXT: u8 = 1;
    pub const INJECT_TOUCH_EVENT: u8 = 2;
    pub const BACK_OR_SCREEN_ON: u8 = 4;
    pub const GET_CLIPBOARD: u8 = 8;
    pub const SET_CLIPBOARD: u8 = 9;
    pub const ROTATE_DEVICE: u8 = 11;
    pub const RESET_VIDEO: u8 = 17;
}

/// Android's `MotionEvent` actions.
pub mod touch_action {
    pub const DOWN: u8 = 0;
    pub const UP: u8 = 1;
    pub const MOVE: u8 = 2;
}

/// Full pressure, in the 16-bit fixed-point form scrcpy expects.
const PRESSURE_MAX: u16 = 0xFFFF;

/// Encode a touch event.
///
/// Coordinates are device pixels here, scaled from the normalised 0..1 the
/// session plane carries against the geometry the *server* reported — not the
/// panel's. They differ whenever `max_size` scales the stream down, and using
/// the panel's would put every touch in the wrong place.
pub fn touch(action: u8, pointer_id: u64, x: i32, y: i32, width: i64, height: i64) -> Vec<u8> {
    let mut message = Vec::with_capacity(32);
    message.push(control_message::INJECT_TOUCH_EVENT);
    message.push(action);
    message.extend_from_slice(&pointer_id.to_be_bytes());
    message.extend_from_slice(&x.to_be_bytes());
    message.extend_from_slice(&y.to_be_bytes());
    message.extend_from_slice(&(width as u16).to_be_bytes());
    message.extend_from_slice(&(height as u16).to_be_bytes());
    let pressure = if action == touch_action::UP {
        0
    } else {
        PRESSURE_MAX
    };
    message.extend_from_slice(&pressure.to_be_bytes());
    // action_button and buttons: 0 for a finger, which is what a touchscreen
    // reports. Setting a mouse button here makes long-press behave as a
    // right-click on devices that honour it.
    message.extend_from_slice(&0u32.to_be_bytes());
    message.extend_from_slice(&0u32.to_be_bytes());
    message
}

/// Encode a keycode press or release. `keycode` is an Android `KEYCODE_*`.
pub fn keycode(action: u8, keycode: u32) -> Vec<u8> {
    let mut message = Vec::with_capacity(14);
    message.push(control_message::INJECT_KEYCODE);
    message.push(action);
    message.extend_from_slice(&keycode.to_be_bytes());
    message.extend_from_slice(&0u32.to_be_bytes()); // repeat
    message.extend_from_slice(&0u32.to_be_bytes()); // metaState
    message
}

pub fn text(text: &str) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut message = Vec::with_capacity(5 + bytes.len());
    message.push(control_message::INJECT_TEXT);
    message.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    message.extend_from_slice(bytes);
    message
}

pub fn set_clipboard(sequence: u64, text: &str, paste: bool) -> Vec<u8> {
    let bytes = text.as_bytes();
    let mut message = Vec::with_capacity(14 + bytes.len());
    message.push(control_message::SET_CLIPBOARD);
    message.extend_from_slice(&sequence.to_be_bytes());
    message.push(u8::from(paste));
    message.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    message.extend_from_slice(bytes);
    message
}

pub fn get_clipboard() -> Vec<u8> {
    // copy_key = 0: read what is already there rather than asking the focused
    // app to copy first, which would alter the device's state to answer a read.
    vec![control_message::GET_CLIPBOARD, 0]
}

pub fn rotate_device() -> Vec<u8> {
    vec![control_message::ROTATE_DEVICE]
}

pub fn back_or_screen_on(action: u8) -> Vec<u8> {
    vec![control_message::BACK_OR_SCREEN_ON, action]
}

/// Messages the device sends back on the control socket.
pub mod device_message {
    pub const CLIPBOARD: u8 = 0;
    pub const ACK_CLIPBOARD: u8 = 1;
}

/// Read one device message, returning clipboard text when that is what it was.
pub async fn read_device_message(control: &mut TcpStream) -> Result<Option<String>> {
    let mut kind = [0u8; 1];
    control.read_exact(&mut kind).await?;

    match kind[0] {
        device_message::CLIPBOARD => {
            let mut length = [0u8; 4];
            control.read_exact(&mut length).await?;
            let mut text = vec![0u8; u32::from_be_bytes(length) as usize];
            control.read_exact(&mut text).await?;
            Ok(Some(String::from_utf8_lossy(&text).into_owned()))
        }
        device_message::ACK_CLIPBOARD => {
            let mut sequence = [0u8; 8];
            control.read_exact(&mut sequence).await?;
            Ok(None)
        }
        other => {
            // UHID output and anything a later server adds. The length is not
            // knowable without the type, so the socket cannot be resynced —
            // fail rather than silently misparse everything after it.
            bail!("unknown device message type {other}")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_touch_is_thirty_two_bytes_in_the_order_the_server_parses() {
        let message = touch(touch_action::DOWN, 7, 100, 200, 472, 1024);
        assert_eq!(message.len(), 32);
        assert_eq!(message[0], control_message::INJECT_TOUCH_EVENT);
        assert_eq!(message[1], touch_action::DOWN);
        assert_eq!(u64::from_be_bytes(message[2..10].try_into().unwrap()), 7);
        assert_eq!(i32::from_be_bytes(message[10..14].try_into().unwrap()), 100);
        assert_eq!(i32::from_be_bytes(message[14..18].try_into().unwrap()), 200);
        assert_eq!(u16::from_be_bytes(message[18..20].try_into().unwrap()), 472);
        assert_eq!(
            u16::from_be_bytes(message[20..22].try_into().unwrap()),
            1024
        );
        assert_eq!(
            u16::from_be_bytes(message[22..24].try_into().unwrap()),
            0xFFFF
        );
    }

    #[test]
    fn a_release_carries_no_pressure() {
        let message = touch(touch_action::UP, 1, 0, 0, 100, 100);
        assert_eq!(u16::from_be_bytes(message[22..24].try_into().unwrap()), 0);
    }

    #[test]
    fn text_is_length_prefixed_utf8() {
        let message = text("hé");
        assert_eq!(message[0], control_message::INJECT_TEXT);
        assert_eq!(u32::from_be_bytes(message[1..5].try_into().unwrap()), 3);
        assert_eq!(&message[5..], "hé".as_bytes());
    }

    #[test]
    fn clipboard_writes_carry_a_sequence_and_a_paste_flag() {
        let message = set_clipboard(42, "hi", false);
        assert_eq!(message[0], control_message::SET_CLIPBOARD);
        assert_eq!(u64::from_be_bytes(message[1..9].try_into().unwrap()), 42);
        assert_eq!(message[9], 0);
        assert_eq!(u32::from_be_bytes(message[10..14].try_into().unwrap()), 2);
    }

    #[test]
    fn a_clipboard_read_does_not_ask_the_app_to_copy_first() {
        assert_eq!(get_clipboard(), vec![control_message::GET_CLIPBOARD, 0]);
    }
}
