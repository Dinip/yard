//! Codec-agnostic encoded-audio fan-out.
//!
//! Android publishes scrcpy Opus; iOS converts AAC-ELD to Opus. The provider
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioStatus {
    Pending,
    Available(AudioCodecDescription),
    Unavailable(String),
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
    status: watch::Receiver<AudioStatus>,
    packets: broadcast::Sender<Arc<AudioPacket>>,
}

impl AudioHandle {
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AudioPacket>> {
        self.packets.subscribe()
    }

    pub fn status_watch(&self) -> watch::Receiver<AudioStatus> {
        self.status.clone()
    }

    pub fn current_codec(&self) -> Option<AudioCodecDescription> {
        match &*self.status.borrow() {
            AudioStatus::Available(codec) => Some(codec.clone()),
            _ => None,
        }
    }
}

#[derive(Clone)]
pub struct AudioPublisher {
    status: Arc<watch::Sender<AudioStatus>>,
    packets: broadcast::Sender<Arc<AudioPacket>>,
}

impl AudioPublisher {
    pub fn set_codec(&self, description: AudioCodecDescription) {
        self.set_status(AudioStatus::Available(description));
    }

    pub fn set_unavailable(&self, reason: String) {
        self.set_status(AudioStatus::Unavailable(reason));
    }

    fn set_status(&self, next: AudioStatus) {
        self.status.send_if_modified(|current| {
            if *current == next {
                return false;
            }
            *current = next;
            true
        });
    }

    /// A send error only means nobody is listening yet. Capture stays active
    /// because it is also what routes media away from the device speaker.
    pub fn publish(&self, packet: AudioPacket) -> usize {
        if matches!(*self.status.borrow(), AudioStatus::Available(_)) {
            self.packets.send(Arc::new(packet)).unwrap_or(0)
        } else {
            0
        }
    }

    pub fn mark_stopped(&self) {
        self.set_status(AudioStatus::Pending);
    }
}

pub fn channel() -> (AudioHandle, AudioPublisher) {
    let (status_tx, status_rx) = watch::channel(AudioStatus::Pending);
    let (packets, _) = broadcast::channel(BACKLOG);

    (
        AudioHandle {
            status: status_rx,
            packets: packets.clone(),
        },
        AudioPublisher {
            status: Arc::new(status_tx),
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
        publisher.set_codec(AudioCodecDescription {
            codec: "opus".into(),
            sample_rate: 48_000,
            channels: 2,
        });

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

    #[tokio::test]
    async fn unavailable_audio_drops_packets_and_can_recover() {
        let (handle, publisher) = channel();
        let mut status = handle.status_watch();
        let mut packets = handle.subscribe();
        publisher.set_unavailable("capture failed".into());
        status.changed().await.unwrap();
        assert_eq!(
            *status.borrow_and_update(),
            AudioStatus::Unavailable("capture failed".into())
        );
        assert_eq!(
            publisher.publish(AudioPacket {
                data: vec![1],
                timestamp_us: 0
            }),
            0
        );
        assert!(packets.try_recv().is_err());

        let codec = AudioCodecDescription {
            codec: "opus".into(),
            sample_rate: 48_000,
            channels: 2,
        };
        publisher.set_codec(codec.clone());
        status.changed().await.unwrap();
        status.borrow_and_update();
        assert_eq!(handle.current_codec(), Some(codec.clone()));
        publisher.set_codec(codec);
        assert!(!status.has_changed().unwrap());
        assert_eq!(
            publisher.publish(AudioPacket {
                data: vec![2],
                timestamp_us: 1
            }),
            1
        );
        assert_eq!(packets.recv().await.unwrap().data, vec![2]);
    }
}
