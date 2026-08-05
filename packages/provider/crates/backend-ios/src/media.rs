//! The CoreDevice display media session: RTP in, RTCP out, access units out.
//!
//! Ported from `stf-ios-provider/src/device/media.rs`. The session loop below is
//! carried over verbatim — every constant and every branch marks a field
//! failure, and the comments say which. The only structural change is that the
//! fan-out now belongs to `provider-core::video`, which generalised this file's
//! `MediaHandle` so Android can share it; this module publishes into that
//! instead of owning its own.
//!
//! `idevice` supplies the protocol primitives — the negotiation offer, RTP
//! parsing, the RTCP builders — but not the session loop that keeps a real
//! device streaming. Everything below exists because the device misbehaves
//! without it:
//!
//! * **Receiver reports.** The `streamConfig` the device returns sets
//!   `RTCPTimeoutEnabled`, so without a periodic RR the encoder stalls after
//!   roughly 20–25 s. This is not optional.
//! * **Audio brought up and then ignored.** Xcode's mirror pairs audio and
//!   video under one `clientSessionID`, and iOS throttles a lone video client.
//!   We start the audio stream, keep its RR alive, and never read its payload.
//! * **Corrupt access units are dropped whole.** A missing reference does not
//!   make VideoToolbox raise — it silently renders the mispredicted blocks as a
//!   mosaic tear that persists until the next IDR.
//! * **Keyframe requests are rate limited.** The iOS 26/27 encoder degrades
//!   under a PLI barrage and can wedge outright, so a loss burst must not
//!   become a PLI storm.
//! * **A decoder-refresh loop.** Under sustained motion the encoder drops
//!   capture resolution and composites the shrunk screen top-left with grey
//!   padding. Recovering needs a fresh full-resolution IDR, and forcing one
//!   *while still moving* beats any client-side detect-then-request loop.
//! * **A stall watchdog.** An encoder that stops emitting entirely only comes
//!   back with a new session.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context as _, Result};
use idevice::core_device::{
    build_keyframe_request, build_liveness, build_screen_audio_offer, build_screen_video_offer,
    build_start_audio_parameters, build_start_video_parameters, is_rtcp, CallInfoBlob,
    DisplayServiceClient, ReportBlock, RtpPacket,
};
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::{AdapterHandle, UdpSocketHandle};
use idevice::ReadWrite;
use tokio::sync::watch;
use tokio::time::{interval, MissedTickBehavior};
use tracing::{debug, info, warn};

use provider_core::video::{AccessUnit, CodecDescription, VideoPublisher};

use crate::hevc::{self, Depacketizer, ParameterSets};
use crate::{Geometry, IosOptions};

/// `clientSupportedFeatures` is what *we* support, not the device's mask.
/// Sending the device's larger mask makes the negotiator produce an invalid
/// video config.
const CLIENT_SUPPORTED_FEATURES: u64 = 140;

/// Floor between keyframe requests, covering both the refresh loop and the
/// recovery paths.
const KEYFRAME_MIN_INTERVAL: Duration = Duration::from_millis(700);
/// Cadence of the mid-motion refresh while the byte rate says the screen moves.
const MOTION_IDR_INTERVAL: Duration = Duration::from_secs(1);
/// The byte rate over a one-second window above which the screen counts as
/// moving.
const MOTION_THRESHOLD_BPS: u64 = 200_000;
/// The byte rate must stay *continuously* below the threshold this long before
/// a settle refresh fires, so an inter-swipe micro-dip cannot retrigger it.
const SETTLE_QUIET: Duration = Duration::from_millis(1500);
/// Idle backstop cadence.
const REFRESH_HEARTBEAT: Duration = Duration::from_secs(10);

/// RTCP receiver-report cadence.
const RTCP_INTERVAL: Duration = Duration::from_secs(1);

/// No access unit for this long and the session is considered wedged.
const STALL_TIMEOUT: Duration = Duration::from_secs(5);

/// How long the *first* access unit gets before the watchdog counts it as a
/// stall. Bring-up is legitimately slower than steady state — the negotiation
/// round-trip, the encoder spinning up, and a locked screen all eat into it —
/// and firing here would turn a slow start into a rebuild loop, since the
/// supervisor's own backoff is shorter than a cold start.
const FIRST_FRAME_TIMEOUT: Duration = Duration::from_secs(30);

/// Everything the RTCP loops need to describe the stream back to the device.
#[derive(Default)]
struct RtpStats {
    packets: AtomicU64,
    /// Extended (cycle-shifted) highest sequence number received.
    highest_seq: AtomicU64,
    /// The first sequence number seen, plus one so zero means "not yet set".
    /// Loss is measured from here rather than from zero — the device's stream
    /// starts at an arbitrary sequence, and treating that offset as loss would
    /// report a catastrophic figure to an encoder that acts on it.
    base_seq: AtomicU64,
    /// RFC 3550 interarrival jitter in the 24 kHz media clock.
    jitter: AtomicU64,
    lost: AtomicU64,
}

#[derive(Clone, Copy)]
struct StreamIdentity {
    /// Our SSRC. The device's `streamConfig` names it `RemoteSSRC`, because the
    /// names in that dictionary are from the device's point of view.
    local_ssrc: u32,
    /// The device's SSRC (`LocalSSRC` in `streamConfig`).
    remote_ssrc: u32,
    /// The device's RTCP port; feedback is muxed onto the RTP socket.
    rtcp_port: u16,
}

fn stream_identity(answer: &plist::Value, label: &str) -> Option<StreamIdentity> {
    let config = answer
        .as_dictionary()?
        .get("connection")?
        .as_dictionary()?
        .get("streamConfig")?
        .as_dictionary()?;

    let field = |name: &str| config.get(name).and_then(plist::Value::as_unsigned_integer);
    let identity = StreamIdentity {
        local_ssrc: field("RemoteSSRC")? as u32,
        remote_ssrc: field("LocalSSRC")? as u32,
        rtcp_port: field("SourcePort")? as u16,
    };

    if identity.rtcp_port == 0 || identity.local_ssrc == 0 || identity.remote_ssrc == 0 {
        warn!(
            label,
            "streamConfig is missing SSRCs or SourcePort — RTCP feedback disabled"
        );
        return None;
    }
    Some(identity)
}

/// Run one media session to completion.
///
/// Returns `Ok(())` only on a clean shutdown; a stalled or broken stream is an
/// error so the device supervisor rebuilds the whole session — the HID surfaces
/// authenticate against the live media stream, so they cannot outlive it.
pub async fn run(
    mut adapter: AdapterHandle,
    mut handshake: RsdHandshake,
    options: IosOptions,
    publisher: VideoPublisher,
    geometry: Geometry,
) -> Result<()> {
    let adapter = &mut adapter;
    let handshake = &mut handshake;

    let mut client = crate::device::connect_service!(
        DisplayServiceClient<Box<dyn ReadWrite>>,
        adapter,
        handshake
    )
    .context("connect displayservice")?;

    let audio_udp = Arc::new(adapter.bind_udp(0).await.context("bind audio RTP socket")?);
    let video_udp = Arc::new(adapter.bind_udp(0).await.context("bind video RTP socket")?);

    let receiver_ip = adapter.host_ip().to_string();
    let sender_ip = adapter.peer_ip().to_string();

    // The string values mirror a captured Device Hub offer the device accepted.
    let call_info = CallInfoBlob {
        call_id: 0,
        client_version: 1,
        device_type: "Mac17,7".into(),
        framework_version: "2205.3.1".into(),
        os_version: "25F71".into(),
        device_name: None,
        audio_device_uid: None,
    };

    // Audio and video share one clientSessionID so the device sees a single
    // mirror client, exactly as Xcode does.
    let client_session_id = uuid::Uuid::new_v4();

    // Audio first: it establishes the screen-sharing session. Without it the
    // video stream fails, because the device negotiator finds no local screen
    // video rules.
    let audio_offer =
        build_screen_audio_offer(&uuid::Uuid::new_v4().to_string().to_uppercase(), &call_info)
            .map_err(|err| anyhow!("build audio offer: {err:?}"))?;
    let audio_answer = client
        .start_media_stream(build_start_audio_parameters(
            &receiver_ip,
            audio_udp.local_port(),
            &sender_ip,
            50000,
            audio_offer,
            CLIENT_SUPPORTED_FEATURES,
            client_session_id,
        ))
        .await
        .map_err(|err| anyhow!("start audio stream: {err:?}"))?;
    info!(session = %client_session_id, "audio stream started");

    let our_video_ssrc = uuid::Uuid::new_v4().as_u128() as u32;
    let video_offer = build_screen_video_offer(
        &uuid::Uuid::new_v4().to_string().to_uppercase(),
        &call_info,
        our_video_ssrc,
    )
    .map_err(|err| anyhow!("build video offer: {err:?}"))?;
    let video_answer = client
        .start_media_stream(build_start_video_parameters(
            &receiver_ip,
            video_udp.local_port(),
            &sender_ip,
            50001,
            video_offer,
            CLIENT_SUPPORTED_FEATURES,
            options.display_id,
            client_session_id,
        ))
        .await
        .map_err(|err| anyhow!("start video stream: {err:?}"))?;
    info!(session = %client_session_id, "video stream started");

    let video = stream_identity(&video_answer, "video")
        .ok_or_else(|| anyhow!("video streamConfig carried no RTCP endpoint"))?;
    let audio = stream_identity(&audio_answer, "audio");

    let stats = Arc::new(RtpStats::default());
    let streaming = Arc::new(AtomicBool::new(false));
    // Seeded to now so the watchdog gives a fresh stream its full grace period
    // rather than firing on a zero-initialised value.
    let (au_tx, au_rx) = watch::channel(Instant::now());

    let mut tasks = tokio::task::JoinSet::new();

    tasks.spawn(receive_video(
        video_udp.clone(),
        publisher,
        geometry,
        streaming.clone(),
        stats.clone(),
        au_tx,
        video,
        options.motion_idr,
    ));

    tasks.spawn(rtcp_liveness(
        video_udp.clone(),
        video.rtcp_port,
        video.local_ssrc,
        video.remote_ssrc,
        stats.clone(),
    ));

    if let Some(audio) = audio {
        // The audio session has the same RTCPTimeoutInterval as video, and the
        // device reaps the whole screen-sharing session with it.
        tasks.spawn(rtcp_liveness(
            audio_udp.clone(),
            audio.rtcp_port,
            audio.local_ssrc,
            audio.remote_ssrc,
            Arc::new(RtpStats::default()),
        ));
    }

    tasks.spawn(stall_watchdog(au_rx, streaming));

    let outcome = tasks.join_next().await;

    // Whatever ended first ends the session; the supervisor rebuilds.
    tasks.shutdown().await;
    if let Err(err) = client.stop_media_stream().await {
        debug!(?err, "stop_media_stream on teardown");
    }

    match outcome {
        Some(Ok(result)) => result,
        Some(Err(err)) => Err(anyhow!("media task panicked: {err}")),
        None => Err(anyhow!("media session ended with no tasks")),
    }
}

/// Receive RTP, reassemble access units, and fan them out.
///
/// The argument list is long because this loop is the one place that needs all
/// of it; bundling them into a struct would only move the same fields.
#[allow(clippy::too_many_arguments)]
async fn receive_video(
    socket: Arc<UdpSocketHandle>,
    publisher: VideoPublisher,
    geometry: Geometry,
    streaming: Arc<AtomicBool>,
    stats: Arc<RtpStats>,
    last_au: watch::Sender<Instant>,
    identity: StreamIdentity,
    motion_idr: bool,
) -> Result<()> {
    let mut depacketizer = Depacketizer::new();
    let mut parameter_sets = ParameterSets::default();
    let mut last_codec: Option<CodecDescription> = None;

    let mut current_au: Vec<Vec<u8>> = Vec::new();
    let mut au_is_key = false;
    let mut au_corrupt = false;
    let mut last_seq: Option<u16> = None;
    let mut nals: Vec<Vec<u8>> = Vec::new();

    // Rolling one-second window of *delta* byte sizes, driving the motion
    // detector. Keyframes are excluded deliberately: a forced IDR is
    // 200–300 KB and alone exceeds the motion threshold, so counting them would
    // make each refresh trigger the next — a self-reinforcing PLI storm on a
    // static screen that eventually stalls the encoder.
    let mut byte_window: std::collections::VecDeque<(Instant, u64)> = Default::default();

    let mut refresh = RefreshController::new(socket.clone(), identity, stats.clone(), motion_idr);
    let mut refresh_tick = interval(Duration::from_millis(100));
    refresh_tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        let datagram = tokio::select! {
            received = socket.recv() => received.context("video RTP recv")?,
            _ = refresh_tick.tick() => {
                refresh.tick(&mut byte_window).await;
                continue;
            }
            _ = publisher.keyframe_requested() => {
                refresh.request("viewer").await;
                continue;
            }
        };

        if is_rtcp(&datagram.data) {
            continue;
        }
        let Some(packet) = RtpPacket::parse(&datagram.data) else {
            debug!(
                bytes = datagram.data.len(),
                "non-RTP datagram on the video socket"
            );
            continue;
        };

        stats.packets.fetch_add(1, Ordering::Relaxed);
        track_sequence(&stats, packet.sequence_number);

        // Any gap breaks the in-flight fragment and poisons the whole access
        // unit: a NAL stitched across a gap is structurally valid and
        // semantically garbage, and the deltas that follow a dropped AU
        // reference a frame the viewer never decoded.
        if let Some(previous) = last_seq {
            if packet.sequence_number != previous.wrapping_add(1) {
                depacketizer.reset_fragment();
                au_corrupt = true;
            }
        }
        // Only advance forward, so one reordered packet does not reset our
        // notion of the newest sequence seen.
        if last_seq.is_none_or(|previous| packet.sequence_number.wrapping_sub(previous) < 0x8000) {
            last_seq = Some(packet.sequence_number);
        }

        nals.clear();
        depacketizer.push(packet.payload, &mut nals);
        for nal in nals.drain(..) {
            if nal.len() < 2 {
                continue;
            }
            let nal_type = hevc::nal_type(&nal);
            if parameter_sets.observe(&nal) {
                if let Some(size) = parameter_sets.dimensions {
                    geometry.set(size);
                    // Live viewers re-shape from this. No rotation: the SPS
                    // gives dimensions, never orientation.
                    publisher.set_geometry(size.0, size.1, None);
                }
            }
            if parameter_sets.is_complete() {
                if let Some((codec, description)) = parameter_sets.description() {
                    let announced = CodecDescription { codec, description };
                    // Re-announcing an unchanged description would make every
                    // viewer rebuild its decoder on each parameter-set repeat,
                    // and the device sends those in-band before every IDR.
                    if last_codec.as_ref() != Some(&announced) {
                        publisher.set_codec(announced.clone());
                        last_codec = Some(announced);
                    }
                }
            }
            if hevc::is_key_nal(nal_type) {
                au_is_key = true;
            }
            current_au.push(nal);
        }

        if !packet.marker {
            continue;
        }

        if au_corrupt {
            // Ask for a fresh IDR: without one every later delta references
            // slices we never delivered, and the browser freezes waiting for a
            // keyframe that on a long-GOP stream may never come naturally.
            refresh.request("au-drop").await;
        } else if !current_au.is_empty() {
            let data = hevc::pack_access_unit(&current_au);
            let now = Instant::now();

            if !au_is_key {
                byte_window.push_back((now, data.len() as u64));
            }
            while byte_window
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(1))
            {
                byte_window.pop_front();
            }

            if au_is_key {
                streaming.store(true, Ordering::Relaxed);
            }
            let _ = last_au.send(now);
            // A publish with no viewers is not an error: the device streams
            // whether or not anyone is watching.
            publisher.publish(AccessUnit {
                data,
                is_key: au_is_key,
            });
        }

        current_au.clear();
        au_is_key = false;
        au_corrupt = false;
    }
}

/// Maintain the extended highest-sequence counter and the loss estimate that
/// receiver reports carry.
fn track_sequence(stats: &RtpStats, seq: u16) {
    let _ =
        stats
            .base_seq
            .compare_exchange(0, seq as u64 + 1, Ordering::Relaxed, Ordering::Relaxed);

    let current = stats.highest_seq.load(Ordering::Relaxed) as u32;
    let mut cycles = (current >> 16) as u16;
    let last = current as u16;

    if seq < last && last.wrapping_sub(seq) > 0x8000 {
        cycles = cycles.wrapping_add(1);
    }
    let extended = ((cycles as u32) << 16) | seq as u32;

    if current == 0 || extended.wrapping_sub(current) < 0x8000_0000 {
        stats.highest_seq.store(extended as u64, Ordering::Relaxed);

        let base = stats.base_seq.load(Ordering::Relaxed).saturating_sub(1);
        let expected = (extended as u64).saturating_sub(base) + 1;
        let received = stats.packets.load(Ordering::Relaxed);
        stats
            .lost
            .store(expected.saturating_sub(received), Ordering::Relaxed);
    }
}

/// Rate-limited keyframe requests, shared by the refresh loop and the recovery
/// paths.
///
/// Lives entirely on the receive task, so its state is plain fields — the
/// rate limit only has to hold against itself.
struct RefreshController {
    socket: Arc<UdpSocketHandle>,
    rtcp_port: u16,
    local_ssrc: u32,
    remote_ssrc: u32,
    stats: Arc<RtpStats>,
    motion_idr: bool,
    last_refresh: Instant,
    fir_seq: u8,
    quiet_since: Option<Instant>,
    motion_since: Option<Instant>,
}

impl RefreshController {
    fn new(
        socket: Arc<UdpSocketHandle>,
        identity: StreamIdentity,
        stats: Arc<RtpStats>,
        motion_idr: bool,
    ) -> Self {
        Self {
            socket,
            rtcp_port: identity.rtcp_port,
            local_ssrc: identity.local_ssrc,
            remote_ssrc: identity.remote_ssrc,
            stats,
            motion_idr,
            last_refresh: Instant::now(),
            fir_seq: 0,
            quiet_since: None,
            motion_since: None,
        }
    }

    async fn request(&mut self, reason: &str) {
        if self.last_refresh.elapsed() < KEYFRAME_MIN_INTERVAL {
            return;
        }
        self.last_refresh = Instant::now();

        let seq = self.fir_seq;
        self.fir_seq = self.fir_seq.wrapping_add(1);

        let packet = build_keyframe_request(
            self.local_ssrc,
            "",
            self.remote_ssrc,
            &[report_block(self.remote_ssrc, &self.stats)],
            seq,
        );
        if let Err(err) = self.socket.send_to(self.rtcp_port, packet).await {
            debug!(%err, reason, "keyframe request failed");
        } else {
            info!(reason, "requested a fresh IDR");
        }
    }

    /// One tick of the decoder-refresh state machine.
    async fn tick(&mut self, byte_window: &mut std::collections::VecDeque<(Instant, u64)>) {
        if !self.motion_idr {
            return;
        }

        let now = Instant::now();
        while byte_window
            .front()
            .is_some_and(|(at, _)| now.duration_since(*at) > Duration::from_secs(1))
        {
            byte_window.pop_front();
        }
        let window_bytes: u64 = byte_window.iter().map(|(_, size)| size).sum();

        if window_bytes >= MOTION_THRESHOLD_BPS {
            self.motion_since.get_or_insert(now);
            self.quiet_since = None;
        } else {
            self.motion_since = None;
            self.quiet_since.get_or_insert(now);
        }

        let since_refresh = self.last_refresh.elapsed();
        if since_refresh < KEYFRAME_MIN_INTERVAL {
            return;
        }

        // Active: the screen is moving, so snap the collapsed resolution back.
        let active = self
            .motion_since
            .is_some_and(|started| now.duration_since(started) >= MOTION_IDR_INTERVAL)
            && since_refresh >= MOTION_IDR_INTERVAL;
        // Settled: a genuine pause, so make the static screen crisp.
        let settled = self
            .quiet_since
            .is_some_and(|since| now.duration_since(since) >= SETTLE_QUIET)
            && since_refresh >= SETTLE_QUIET;
        // Heartbeat: an idle backstop that also unsticks a viewer waiting on a
        // key that never came.
        let heartbeat = self.quiet_since.is_some() && since_refresh >= REFRESH_HEARTBEAT;

        let reason = match (active, settled, heartbeat) {
            (true, _, _) => "motion",
            (_, true, _) => "settled",
            (_, _, true) => "heartbeat",
            _ => return,
        };
        self.request(reason).await;
    }
}

fn report_block(source_ssrc: u32, stats: &RtpStats) -> ReportBlock {
    ReportBlock {
        source_ssrc,
        fraction_lost: 0,
        cumulative_lost: stats.lost.load(Ordering::Relaxed).min(0xFF_FFFF) as u32,
        highest_seq: stats.highest_seq.load(Ordering::Relaxed) as u32,
        jitter: stats.jitter.load(Ordering::Relaxed) as u32,
        lsr: 0,
        dlsr: 0,
    }
}

/// Keep the device's encoder alive.
///
/// `RTCPTimeoutEnabled` is set in the `streamConfig` the device returns: with
/// no periodic receiver report it reaps the stream after ~20 s.
async fn rtcp_liveness(
    socket: Arc<UdpSocketHandle>,
    rtcp_port: u16,
    local_ssrc: u32,
    remote_ssrc: u32,
    stats: Arc<RtpStats>,
) -> Result<()> {
    let mut tick = interval(RTCP_INTERVAL);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tick.tick().await;
        let packet = build_liveness(local_ssrc, "", &[report_block(remote_ssrc, &stats)]);
        if let Err(err) = socket.send_to(rtcp_port, packet).await {
            // The socket going away means the tunnel did; let the supervisor
            // rebuild rather than spinning here.
            return Err(anyhow!("RTCP send failed: {err}"));
        }
    }
}

/// Fail the session when the encoder stops emitting entirely.
///
/// A wedged encoder — one that choked on a keyframe request, or whose display
/// slept — never recovers within a session. The supervisor's rebuild is the
/// only reliable way to get it streaming again.
async fn stall_watchdog(
    last_au: watch::Receiver<Instant>,
    streaming: Arc<AtomicBool>,
) -> Result<()> {
    let mut tick = interval(STALL_TIMEOUT / 4);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);

    loop {
        tick.tick().await;

        let budget = if streaming.load(Ordering::Relaxed) {
            STALL_TIMEOUT
        } else {
            FIRST_FRAME_TIMEOUT
        };

        let since = last_au.borrow().elapsed();
        if since > budget {
            return Err(anyhow!(
                "no access unit for {:.1}s — restarting the session",
                since.as_secs_f32()
            ));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loss_is_measured_from_the_first_sequence_seen() {
        let stats = RtpStats::default();

        // The device's stream starts at an arbitrary sequence; that offset is
        // not loss.
        for seq in 40_000u16..40_010 {
            stats.packets.fetch_add(1, Ordering::Relaxed);
            track_sequence(&stats, seq);
        }
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);

        // Skip three, then carry on.
        for seq in 40_013u16..40_016 {
            stats.packets.fetch_add(1, Ordering::Relaxed);
            track_sequence(&stats, seq);
        }
        assert_eq!(stats.lost.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn sequence_wrap_does_not_read_as_massive_loss() {
        let stats = RtpStats::default();

        for seq in [65_534u16, 65_535, 0, 1] {
            stats.packets.fetch_add(1, Ordering::Relaxed);
            track_sequence(&stats, seq);
        }
        assert_eq!(stats.lost.load(Ordering::Relaxed), 0);
        assert_eq!(
            stats.highest_seq.load(Ordering::Relaxed),
            (1 << 16) | 1,
            "one cycle, sequence 1"
        );
    }

    #[test]
    fn a_late_straggler_does_not_rewind_the_highest_sequence() {
        let stats = RtpStats::default();
        for seq in [100u16, 101, 102] {
            stats.packets.fetch_add(1, Ordering::Relaxed);
            track_sequence(&stats, seq);
        }
        let highest = stats.highest_seq.load(Ordering::Relaxed);

        stats.packets.fetch_add(1, Ordering::Relaxed);
        track_sequence(&stats, 99);
        assert_eq!(stats.highest_seq.load(Ordering::Relaxed), highest);
    }
}
