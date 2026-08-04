//! Codec-agnostic access-unit fan-out.
//!
//! Generalised from `stf-ios-provider/src/device/media.rs`'s `MediaHandle`,
//! which assumed HEVC. Nothing here knows or cares which codec is flowing: the
//! backend supplies a codec string and a decoder-configuration record, and the
//! browser's `VideoDecoder` does the rest. That is what lets one session server
//! serve HEVC from iOS and H.264 from Android with no branch in between.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{broadcast, watch, Notify};
use tokio::time::timeout;

/// How many access units a slow viewer may fall behind before it is shed.
///
/// Small on purpose: a viewer that is this far behind is not going to catch up,
/// and holding more only delays the keyframe that actually fixes it.
const BACKLOG: usize = 64;

/// What a viewer needs before it can decode anything.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecDescription {
    /// WebCodecs codec string — `hev1.1.6.L120.90`, `avc1.640028`, …
    pub codec: String,
    /// hvcC (HEVC) or avcC (H.264) decoder-configuration record.
    ///
    /// Sent out of band so the payload stays length-prefixed NALUs. The Annex-B
    /// start-code path tears under motion in Chrome — see
    /// `stf-ios-provider/src/screen_ws.rs`.
    pub description: Vec<u8>,
}

/// One encoded frame.
#[derive(Debug)]
pub struct AccessUnit {
    /// Concatenated 4-byte-length-prefixed NALUs (hvcC/avcC sample format).
    pub data: Vec<u8>,
    pub is_key: bool,
}

/// The read side of a live video stream, as viewers see it.
#[derive(Clone)]
pub struct VideoHandle {
    codec: watch::Receiver<Option<CodecDescription>>,
    frames: broadcast::Sender<Arc<AccessUnit>>,
    keyframe: Arc<Notify>,
    streaming: Arc<AtomicBool>,
}

impl VideoHandle {
    /// Access units from now on.
    ///
    /// A viewer that falls behind sees `RecvError::Lagged`, which is exactly the
    /// shed signal: the deltas it missed reference a frame it never decoded, so
    /// it must hold everything until the next keyframe and treat that keyframe
    /// as a decoder reset.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<AccessUnit>> {
        self.frames.subscribe()
    }

    /// Waits until the parameter sets have landed.
    pub async fn wait_for_codec(&self, wait: Duration) -> Option<CodecDescription> {
        let mut codec = self.codec.clone();
        if let Some(description) = codec.borrow().clone() {
            return Some(description);
        }
        timeout(wait, async {
            loop {
                if codec.changed().await.is_err() {
                    return None;
                }
                if let Some(description) = codec.borrow().clone() {
                    return Some(description);
                }
            }
        })
        .await
        .ok()
        .flatten()
    }

    /// Asks the device for an immediate IDR.
    ///
    /// Only the client can detect that its decoder lost reference state: on a
    /// long-GOP stream a stale decoder otherwise sits frozen until the next
    /// natural IDR, which may be minutes away. Backends rate-limit these, so
    /// calling it freely is safe.
    pub fn request_keyframe(&self) {
        self.keyframe.notify_one();
    }

    /// True once a keyframe has been broadcast, so a fresh viewer knows whether
    /// waiting for one is worthwhile.
    pub fn is_streaming(&self) -> bool {
        self.streaming.load(Ordering::Relaxed)
    }
}

/// The write side, owned by a backend's capture task.
#[derive(Clone)]
pub struct VideoPublisher {
    codec: Arc<watch::Sender<Option<CodecDescription>>>,
    frames: broadcast::Sender<Arc<AccessUnit>>,
    keyframe: Arc<Notify>,
    streaming: Arc<AtomicBool>,
}

impl VideoPublisher {
    pub fn set_codec(&self, description: CodecDescription) {
        let _ = self.codec.send(Some(description));
    }

    /// Publishes one access unit. Returns the number of live viewers.
    ///
    /// A send error only means nobody is watching, which is not a fault — the
    /// capture pipeline keeps running so a viewer that arrives next second gets
    /// a stream that is already warm.
    pub fn publish(&self, au: AccessUnit) -> usize {
        if au.is_key {
            self.streaming.store(true, Ordering::Relaxed);
        }
        self.frames.send(Arc::new(au)).unwrap_or(0)
    }

    /// Resolves when a viewer asks for a keyframe.
    pub async fn keyframe_requested(&self) {
        self.keyframe.notified().await;
    }

    /// The stream has stopped; viewers should stop expecting frames.
    pub fn mark_stopped(&self) {
        self.streaming.store(false, Ordering::Relaxed);
        let _ = self.codec.send(None);
    }

    pub fn viewer_count(&self) -> usize {
        self.frames.receiver_count()
    }
}

pub fn channel() -> (VideoHandle, VideoPublisher) {
    let (codec_tx, codec_rx) = watch::channel(None);
    let (frames, _) = broadcast::channel(BACKLOG);
    let keyframe = Arc::new(Notify::new());
    let streaming = Arc::new(AtomicBool::new(false));

    (
        VideoHandle {
            codec: codec_rx,
            frames: frames.clone(),
            keyframe: keyframe.clone(),
            streaming: streaming.clone(),
        },
        VideoPublisher {
            codec: Arc::new(codec_tx),
            frames,
            keyframe,
            streaming,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn subscribers_receive_published_units() {
        let (handle, publisher) = channel();
        let mut rx = handle.subscribe();

        publisher.publish(AccessUnit {
            data: vec![1, 2, 3],
            is_key: true,
        });

        let au = rx.recv().await.unwrap();
        assert!(au.is_key);
        assert_eq!(au.data, vec![1, 2, 3]);
        assert!(handle.is_streaming());
    }

    #[tokio::test]
    async fn publishing_with_no_viewers_is_not_an_error() {
        let (_handle, publisher) = channel();
        assert_eq!(
            publisher.publish(AccessUnit {
                data: vec![0],
                is_key: true
            }),
            0
        );
    }

    #[tokio::test]
    async fn a_slow_viewer_lags_rather_than_blocking_the_publisher() {
        let (handle, publisher) = channel();
        let mut rx = handle.subscribe();

        for i in 0..(BACKLOG + 10) {
            publisher.publish(AccessUnit {
                data: vec![i as u8],
                is_key: false,
            });
        }

        // The publisher never blocked; the viewer is told it fell behind, which
        // is its cue to shed and wait for a keyframe.
        assert!(matches!(
            rx.recv().await,
            Err(broadcast::error::RecvError::Lagged(_))
        ));
    }

    #[tokio::test]
    async fn codec_is_awaited_then_cached() {
        let (handle, publisher) = channel();
        let want = CodecDescription {
            codec: "hev1.1.6.L93.B0".into(),
            description: vec![1, 2],
        };

        let waiter = {
            let handle = handle.clone();
            tokio::spawn(async move { handle.wait_for_codec(Duration::from_secs(1)).await })
        };
        tokio::time::sleep(Duration::from_millis(20)).await;
        publisher.set_codec(want.clone());

        assert_eq!(waiter.await.unwrap().as_ref(), Some(&want));
        // A later arrival gets it immediately, without waiting.
        assert_eq!(
            handle
                .wait_for_codec(Duration::from_millis(1))
                .await
                .as_ref(),
            Some(&want)
        );
    }

    #[tokio::test]
    async fn wait_for_codec_times_out_rather_than_hanging() {
        let (handle, _publisher) = channel();
        assert!(handle
            .wait_for_codec(Duration::from_millis(30))
            .await
            .is_none());
    }
}
