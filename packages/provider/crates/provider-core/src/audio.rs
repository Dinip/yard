//! Codec-agnostic encoded-audio fan-out.
//!
//! Android publishes the Opus packets scrcpy already encoded. The provider
//! keeps them encoded all the way to each browser, just as it does for video.

use std::sync::Arc;

use tokio::sync::{broadcast, watch};

/// Enough room for an ordinary scheduling hiccup, but not enough to turn a
/// slow browser into seconds of delayed audio.
const BACKLOG: usize = 32;

/// What a browser needs to configure `AudioDecoder`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AudioCodecDescription {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
}

/// One independently decodable encoded audio packet.
#[derive(Debug)]
pub struct AudioPacket {
    pub data: Vec<u8>,
    /// Source presentation timestamp in microseconds.
    pub timestamp_us: u64,
}

#[derive(Clone)]
pub struct AudioHandle {
    codec: watch::Receiver<Option<AudioCodecDescription>>,
    packets: broadcast::Sender<Arc<AudioPacket>>,
}

impl AudioHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AudioPacket>> {
        self.packets.subscribe()
    }

    pub fn codec_watch(&self) -> watch::Receiver<Option<AudioCodecDescription>> {
        self.codec.clone()
    }

    pub fn current_codec(&self) -> Option<AudioCodecDescription> {
        self.codec.borrow().clone()
    }
}

#[derive(Clone)]
pub struct AudioPublisher {
    codec: Arc<watch::Sender<Option<AudioCodecDescription>>>,
    packets: broadcast::Sender<Arc<AudioPacket>>,
}

impl AudioPublisher {
    pub fn set_codec(&self, description: AudioCodecDescription) {
        let _ = self.codec.send(Some(description));
    }

    /// A send error only means nobody is listening yet. Capture stays active
    /// because it is also what routes media away from the device speaker.
    pub fn publish(&self, packet: AudioPacket) -> usize {
        self.packets.send(Arc::new(packet)).unwrap_or(0)
    }

    pub fn mark_stopped(&self) {
        let _ = self.codec.send(None);
    }
}

pub fn channel() -> (AudioHandle, AudioPublisher) {
    let (codec_tx, codec_rx) = watch::channel(None);
    let (packets, _) = broadcast::channel(BACKLOG);

    (
        AudioHandle {
            codec: codec_rx,
            packets: packets.clone(),
        },
        AudioPublisher {
            codec: Arc::new(codec_tx),
            packets,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_encoded_packets() {
        let (handle, publisher) = channel();
        let mut packets = handle.subscribe();

        publisher.set_codec(AudioCodecDescription {
            codec: "opus".into(),
            sample_rate: 48_000,
            channels: 2,
        });
        publisher.publish(AudioPacket {
            data: vec![0xf8, 0xff, 0xfe],
            timestamp_us: 42,
        });

        assert_eq!(handle.current_codec().unwrap().codec, "opus");
        let packet = packets.recv().await.unwrap();
        assert_eq!(packet.timestamp_us, 42);
        assert_eq!(packet.data, vec![0xf8, 0xff, 0xfe]);
    }

    #[tokio::test]
    async fn a_slow_listener_lags_without_blocking_capture() {
        let (handle, publisher) = channel();
        let mut packets = handle.subscribe();

        for timestamp_us in 0..(BACKLOG as u64 + 1) {
            publisher.publish(AudioPacket {
                data: vec![0],
                timestamp_us,
            });
        }

        assert!(matches!(
            packets.recv().await,
            Err(broadcast::error::RecvError::Lagged(1))
        ));
    }
}
