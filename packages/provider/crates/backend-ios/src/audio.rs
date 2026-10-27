//! CoreDevice AAC-ELD to browser Opus. Conversion never blocks the RTP drain.

use std::process::Stdio;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Context as _, Result};
use idevice::core_device::{is_rtcp, RtpPacket};
use idevice::tcp::handle::UdpSocketHandle;
use provider_core::audio::{AudioCodecDescription, AudioPacket, AudioPublisher};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::media::{track_sequence, RtpStats};

// Streaming Matroska with one AAC-ELD track: 48 kHz stereo, 480 samples/AU.
// ADTS cannot represent ELD. CodecPrivate f8e65000 sets the 480-sample flag;
// f8e64000 instead decodes 512 samples and makes playback drift and crackle.
const HEADER: &[u8] = &[
    0x1a, 0x45, 0xdf, 0xa3, 0xa3, 0x42, 0x86, 0x81, 0x01, 0x42, 0xf7, 0x81, 0x01, 0x42, 0xf2, 0x81,
    0x04, 0x42, 0xf3, 0x81, 0x08, 0x42, 0x82, 0x88, 0x6d, 0x61, 0x74, 0x72, 0x6f, 0x73, 0x6b, 0x61,
    0x42, 0x87, 0x81, 0x04, 0x42, 0x85, 0x81, 0x02, 0x18, 0x53, 0x80, 0x67, 0x01, 0xff, 0xff, 0xff,
    0xff, 0xff, 0xff, 0xff, 0x15, 0x49, 0xa9, 0x66, 0x95, 0x2a, 0xd7, 0xb1, 0x83, 0x0f, 0x42, 0x40,
    0x4d, 0x80, 0x84, 0x79, 0x61, 0x72, 0x64, 0x57, 0x41, 0x84, 0x79, 0x61, 0x72, 0x64, 0x16, 0x54,
    0xae, 0x6b, 0xa9, 0xae, 0xa7, 0xd7, 0x81, 0x01, 0x73, 0xc5, 0x81, 0x01, 0x83, 0x81, 0x02, 0x86,
    0x85, 0x41, 0x5f, 0x41, 0x41, 0x43, 0x63, 0xa2, 0x84, 0xf8, 0xe6, 0x50, 0x00, 0xe1, 0x8d, 0xb5,
    0x88, 0x40, 0xe7, 0x70, 0x00, 0x00, 0x00, 0x00, 0x00, 0x9f, 0x81, 0x02,
];

fn supported(answer: &plist::Value) -> bool {
    let config = answer
        .as_dictionary()
        .and_then(|d| d.get("connection"))
        .and_then(plist::Value::as_dictionary)
        .and_then(|d| d.get("streamConfig"))
        .and_then(plist::Value::as_dictionary);
    let field = |name| {
        config
            .and_then(|d| d.get(name))
            .and_then(plist::Value::as_unsigned_integer)
    };
    // shortcut: mode 8/PT 101 is the verified 48 kHz stereo ELD format;
    // add negotiation parsing if another iOS audio mode is observed.
    field("AudioStreamMode") == Some(8)
        && field("RxPayloadType") == Some(101)
        && field("SRTPCipherSuite") == Some(0)
}

pub(crate) async fn receive(
    socket: Arc<UdpSocketHandle>,
    answer: plist::Value,
    publisher: AudioPublisher,
    stats: Arc<RtpStats>,
) -> Result<()> {
    let (packets, input) = mpsc::channel(16);
    let mut converting = supported(&answer);
    if !converting {
        publisher.set_unavailable("Unsupported iOS audio format".into());
    }
    let conversion = convert(input, publisher.clone());
    tokio::pin!(conversion);
    let mut last_seq: Option<u16> = None;
    let mut last_timestamp: Option<u32> = None;
    let mut samples = 0u64;

    loop {
        let datagram = tokio::select! {
            result = &mut conversion, if converting => {
                let reason = match result {
                    Ok(()) => "iOS audio conversion stopped".into(),
                    Err(err) => format!("iOS audio conversion failed: {err:#}"),
                };
                warn!(%reason);
                publisher.set_unavailable(reason);
                converting = false;
                continue;
            }
            received = socket.recv() => received.context("audio RTP recv")?,
        };
        if is_rtcp(&datagram.data) {
            continue;
        }
        let Some(packet) = RtpPacket::parse(&datagram.data) else {
            continue;
        };
        stats.packets.fetch_add(1, Ordering::Relaxed);
        track_sequence(&stats, packet.sequence_number);
        if !converting || packet.payload_type != 101 || packet.payload.is_empty() {
            continue;
        }
        if last_seq.is_some_and(|seq| {
            let delta = packet.sequence_number.wrapping_sub(seq);
            delta == 0 || delta >= 0x8000
        }) {
            continue;
        }
        last_seq = Some(packet.sequence_number);
        if let Some(previous) = last_timestamp {
            samples += u64::from(packet.timestamp.wrapping_sub(previous));
        }
        last_timestamp = Some(packet.timestamp);
        // A stalled converter sheds packets rather than queuing audio on the
        // same tunnel adapter as video. AAC-ELD AUs need no reassembly.
        let _ = packets.try_send(AudioPacket {
            data: packet.payload.to_vec(),
            timestamp_us: samples * 1_000_000 / 48_000,
        });
    }
}

async fn convert(mut input: mpsc::Receiver<AudioPacket>, publisher: AudioPublisher) -> Result<()> {
    let mut child = Command::new("ffmpeg")
        .args([
            "-hide_banner",
            "-loglevel",
            "error",
            "-nostdin",
            "-probesize",
            "32",
            "-analyzeduration",
            "0",
            "-f",
            "matroska",
            "-i",
            "pipe:0",
            "-map",
            "0:a:0",
            "-c:a",
            "libopus",
            "-application",
            "lowdelay",
            "-frame_duration",
            "20",
            "-b:a",
            "96000",
            "-f",
            "opus",
            "-page_duration",
            "20000",
            "-flush_packets",
            "1",
            "pipe:1",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("starting ffmpeg; install FFmpeg with libopus on the provider host")?;
    let mut stdin = child.stdin.take().context("ffmpeg stdin")?;
    let mut stdout = child.stdout.take().context("ffmpeg stdout")?;
    let write = async {
        stdin.write_all(HEADER).await?;
        while let Some(packet) = input.recv().await {
            stdin.write_all(&matroska_packet(&packet)?).await?;
        }
        Result::<()>::Ok(())
    };
    let read = publish_opus(&mut stdout, publisher);
    let outcome = tokio::select! {
        result = write => result.context("writing iOS audio to ffmpeg"),
        result = read => result.context("reading Opus from ffmpeg"),
        status = child.wait() => Err(anyhow!("ffmpeg exited: {}", status?)),
    };
    let _ = child.kill().await;
    outcome.and_then(|()| Err(anyhow!("audio converter stopped")))
}

fn matroska_packet(packet: &AudioPacket) -> Result<Vec<u8>> {
    // Four-byte EBML sizes bound every element, including untrusted device AUs.
    fn element(id: &[u8], data: &[u8]) -> Result<Vec<u8>> {
        if data.len() >= 0x0fff_ffff {
            bail!("audio element exceeds Matroska size limit");
        }
        let mut out = id.to_vec();
        out.extend_from_slice(&(0x1000_0000 | data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        Ok(out)
    }
    let mut cluster = element(&[0xe7], &(packet.timestamp_us / 1000).to_be_bytes())?;
    let mut block = vec![0x81, 0, 0, 0x80];
    block.extend_from_slice(&packet.data);
    cluster.extend(element(&[0xa3], &block)?);
    element(&[0x1f, 0x43, 0xb6, 0x75], &cluster)
}

async fn publish_opus(
    reader: &mut (impl AsyncRead + Unpin),
    publisher: AudioPublisher,
) -> Result<()> {
    let mut packet = Vec::new();
    let mut timestamp_us = 0;
    let mut announced = false;
    loop {
        let mut header = [0; 27];
        tokio::time::timeout(Duration::from_secs(5), reader.read_exact(&mut header))
            .await
            .context("ffmpeg stopped producing audio")??;
        if &header[..5] != b"OggS\0" {
            bail!("invalid ffmpeg Ogg header");
        }
        let mut lacing = vec![0; header[26] as usize];
        reader.read_exact(&mut lacing).await?;
        for size in lacing {
            let start = packet.len();
            if start + size as usize > 65_536 {
                bail!("ffmpeg Opus packet exceeds size limit");
            }
            packet.resize(start + size as usize, 0);
            reader.read_exact(&mut packet[start..]).await?;
            if size == 255 {
                continue;
            }
            if !packet.starts_with(b"OpusHead") && !packet.starts_with(b"OpusTags") {
                if !announced {
                    publisher.set_codec(AudioCodecDescription {
                        codec: "opus".into(),
                        sample_rate: 48_000,
                        channels: 2,
                    });
                    announced = true;
                    info!("iOS browser audio ready: AAC-ELD to Opus");
                }
                publisher.publish(AudioPacket {
                    data: std::mem::take(&mut packet),
                    timestamp_us,
                });
                timestamp_us += 20_000;
            }
            packet.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_verified_unencrypted_audio_format_is_accepted() {
        let answer = |mode: u64, pt: u64, cipher: u64| {
            let config: plist::Dictionary = [
                (
                    "AudioStreamMode".to_owned(),
                    plist::Value::Integer(mode.into()),
                ),
                ("RxPayloadType".to_owned(), plist::Value::Integer(pt.into())),
                (
                    "SRTPCipherSuite".to_owned(),
                    plist::Value::Integer(cipher.into()),
                ),
            ]
            .into_iter()
            .collect();
            let connection = [("streamConfig".to_owned(), plist::Value::Dictionary(config))]
                .into_iter()
                .collect();
            plist::Value::Dictionary(
                [(
                    "connection".to_owned(),
                    plist::Value::Dictionary(connection),
                )]
                .into_iter()
                .collect(),
            )
        };
        assert!(supported(&answer(8, 101, 0)));
        assert!(!supported(&answer(9, 101, 0)));
        assert!(!supported(&answer(8, 102, 0)));
        assert!(!supported(&answer(8, 101, 1)));
        assert!(!supported(&plist::Value::Dictionary(Default::default())));
    }

    #[tokio::test]
    async fn malformed_or_oversized_ogg_output_is_rejected() {
        let (_, publisher) = provider_core::audio::channel();
        assert!(publish_opus(&mut [0; 27].as_slice(), publisher.clone())
            .await
            .is_err());
        let mut page = vec![0; 27];
        page[..5].copy_from_slice(b"OggS\0");
        page[26] = 255;
        page.extend([255; 255]);
        page.extend([0; 255 * 255]);
        let mut data = page.clone();
        data.extend(page);
        assert!(publish_opus(&mut data.as_slice(), publisher)
            .await
            .unwrap_err()
            .to_string()
            .contains("size limit"));
    }

    #[tokio::test]
    async fn ogg_lacing_preserves_packets_across_pages_and_skips_metadata() {
        fn page(parts: &[&[u8]]) -> Vec<u8> {
            let mut out = vec![0; 27];
            out[..5].copy_from_slice(b"OggS\0");
            out[26] = parts.len() as u8;
            out.extend(parts.iter().map(|p| p.len() as u8));
            for part in parts {
                out.extend_from_slice(part);
            }
            out
        }
        let (handle, publisher) = provider_core::audio::channel();
        let mut packets = handle.subscribe();
        let mut data = page(&[b"OpusHead", b"OpusTags", &[7; 255]]);
        data.extend(page(&[&[8; 3], &[9; 4]]));
        assert!(publish_opus(&mut data.as_slice(), publisher).await.is_err());
        assert_eq!(handle.current_codec().unwrap().codec, "opus");
        let first = packets.try_recv().unwrap();
        assert_eq!(first.data, [&[7; 255][..], &[8; 3]].concat());
        assert_eq!(first.timestamp_us, 0);
        let second = packets.try_recv().unwrap();
        assert_eq!(second.data, vec![9; 4]);
        assert_eq!(second.timestamp_us, 20_000);
        assert!(packets.try_recv().is_err());
    }

    #[tokio::test]
    #[ignore = "requires ffmpeg with libopus on PATH"]
    async fn ffmpeg_converts_480_sample_eld_frames_to_browser_opus() {
        let (handle, publisher) = provider_core::audio::channel();
        let mut packets = handle.subscribe();
        let (tx, rx) = mpsc::channel(100);
        for i in 0..100 {
            tx.try_send(AudioPacket {
                data: vec![0, 104, 52, 0],
                timestamp_us: i * 10_000,
            })
            .unwrap();
        }
        let conversion = convert(rx, publisher);
        tokio::pin!(conversion);
        tokio::select! {
            result = &mut conversion => panic!("converter stopped: {result:?}"),
            packet = tokio::time::timeout(std::time::Duration::from_secs(5), packets.recv()) => {
                assert!(!packet.unwrap().unwrap().data.is_empty());
                assert_eq!(handle.current_codec().unwrap().sample_rate, 48_000);
            }
        }
        drop(tx);
    }
}
