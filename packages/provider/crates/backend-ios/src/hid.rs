//! HID input: the tables, and the actor that owns the device's HID clients.
//!
//! Ported from `stf-ios-provider/src/device/hid.rs`. The tables and the
//! single-owner actor are unchanged; only the key *names* differ, because this
//! farm speaks the browser's `KeyboardEvent.key` vocabulary rather than STF's.
//!
//! The HID surfaces authenticate against the *live media stream*, so their
//! clients are created once per session, after the stream is up, and are owned
//! by a single task. That ownership is not incidental — a second HID client on
//! the same RSD would be posting reports into a session the media loop can
//! invalidate on any restart.
//!
//! Running every report through one task also gives the pointer state machine
//! the ordering it needs for free. A reordered move-before-down turns a swipe
//! into a tap; a reordered up-before-move pins a contact to the glass.

use std::time::Duration;

use anyhow::{anyhow, Result};
use idevice::core_device::hid::{
    ButtonState, IndigoHidClient, UniversalHidServiceClient, TOUCHSCREEN_STATE_CONTACT,
    TOUCHSCREEN_STATE_RELEASE,
};
use idevice::core_device::{Orientation, OrientationServiceClient, RotationDirection};
use idevice::rsd::RsdHandshake;
use idevice::tcp::handle::AdapterHandle;
use idevice::ReadWrite;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, info, warn};

/// CoreDevice HID touchscreen reports carry UInt16 coordinates normalised
/// across the screen — *not* pixels. The session plane speaks normalised floats
/// too, so the two meet with a plain scale and no display geometry anywhere in
/// between. (The WebDriverAgent path did need real pixels plus an
/// orientation-dependent axis swap; none of that survives here.)
pub const HID_MAX: u16 = 65535;

/// Scale a normalised coordinate (0..1) to a HID UInt16, clamped.
pub fn to_hid(value: f64) -> u16 {
    let scaled = (value * HID_MAX as f64).round();
    scaled.clamp(0.0, HID_MAX as f64) as u16
}

/// HID usage page for the consumer controls the hardware buttons live on.
const USAGE_PAGE_CONSUMER: u64 = 0x0C;

/// Left shift, for typing characters that need it.
const KEY_LEFT_SHIFT: u64 = 0xE1;

/// Hardware buttons → `(usage page, usage, hold)`.
///
/// The hold durations are not cosmetic: the device distinguishes a lock press
/// from a power-off hold, and Siri needs a long press to trigger at all.
///
/// Names are matched case-insensitively against the `key` field of a session
/// `key` message, so a browser can send either "Home" or "home".
pub fn named_button(name: &str) -> Option<(u64, u64, Duration)> {
    let (usage, hold_ms) = match name.to_ascii_lowercase().as_str() {
        "home" => (0x40, 50),
        "power" | "lock" => (0x30, 500),
        "volumeup" | "volume_up" | "volume-up" => (0xE9, 50),
        "volumedown" | "volume_down" | "volume-down" => (0xEA, 50),
        "mute" => (0xE2, 50),
        "siri" => (0xCF, 1000),
        _ => return None,
    };
    Some((USAGE_PAGE_CONSUMER, usage, Duration::from_millis(hold_ms)))
}

/// Non-printable keys → HID usage.
///
/// The names are the browser's own `KeyboardEvent.key` values, so what the web
/// control surface sends maps straight through with no translation table in
/// between. Resolves to usages directly rather than round-tripping through the
/// control characters WebDriverAgent's `typeKey` happened to understand.
pub fn special_key(name: &str) -> Option<u64> {
    Some(match name {
        "Enter" => 0x28,
        "Escape" => 0x29,
        "Backspace" => 0x2A,
        "Tab" => 0x2B,
        "CapsLock" => 0x39,
        "Delete" => 0x4C,
        "ArrowRight" => 0x4F,
        "ArrowLeft" => 0x50,
        "ArrowDown" => 0x51,
        "ArrowUp" => 0x52,
        _ => return None,
    })
}

/// ASCII → `(HID keyboard usage, needs shift)`.
///
/// `idevice` has no equivalent table, so this is the one piece of the input
/// path carried over wholesale.
pub fn ascii_to_hid(character: char) -> Option<(u64, bool)> {
    let usage = match character {
        'a'..='z' => return Some((0x04 + (character as u64 - 'a' as u64), false)),
        'A'..='Z' => return Some((0x04 + (character as u64 - 'A' as u64), true)),
        '1'..='9' => return Some((0x1E + (character as u64 - '1' as u64), false)),
        '0' => 0x27,
        '\n' | '\r' => 0x28,
        '\x1b' => 0x29,
        '\x08' | '\x7f' => 0x2A,
        '\t' => 0x2B,
        ' ' => 0x2C,
        '-' => 0x2D,
        '=' => 0x2E,
        '[' => 0x2F,
        ']' => 0x30,
        '\\' => 0x31,
        ';' => 0x33,
        '\'' => 0x34,
        '`' => 0x35,
        ',' => 0x36,
        '.' => 0x37,
        '/' => 0x38,
        // Shifted variants of the above.
        '!' => return Some((0x1E, true)),
        '@' => return Some((0x1F, true)),
        '#' => return Some((0x20, true)),
        '$' => return Some((0x21, true)),
        '%' => return Some((0x22, true)),
        '^' => return Some((0x23, true)),
        '&' => return Some((0x24, true)),
        '*' => return Some((0x25, true)),
        '(' => return Some((0x26, true)),
        ')' => return Some((0x27, true)),
        '_' => return Some((0x2D, true)),
        '+' => return Some((0x2E, true)),
        '{' => return Some((0x2F, true)),
        '}' => return Some((0x30, true)),
        '|' => return Some((0x31, true)),
        ':' => return Some((0x33, true)),
        '"' => return Some((0x34, true)),
        '~' => return Some((0x35, true)),
        '<' => return Some((0x36, true)),
        '>' => return Some((0x37, true)),
        '?' => return Some((0x38, true)),
        _ => return None,
    };
    Some((usage, false))
}

/// CoreDevice orientation → degrees clockwise from portrait.
///
/// Apple's landscape names say where the device's *left edge* went, not how the
/// picture turned: `landscapeLeft` is the phone rotated so its home edge is on
/// the right, which reads as the UI having turned 90° clockwise.
pub fn orientation_degrees(orientation: &Orientation) -> Option<i64> {
    Some(match orientation {
        Orientation::Portrait => 0,
        Orientation::LandscapeLeft => 90,
        Orientation::PortraitUpsideDown => 180,
        Orientation::LandscapeRight => 270,
        // Flat on a desk says nothing about how the UI is drawn.
        Orientation::FaceUp | Orientation::FaceDown | Orientation::Unknown(_) => return None,
    })
}

/// One HID action, applied in the order it was queued.
pub enum Input {
    /// An in-contact touchscreen sample.
    Contact { x: u16, y: u16 },
    /// Lift the contact at this point.
    Release { x: u16, y: u16 },
    /// Type one character through the virtual keyboard.
    Character(char),
    /// Press and release a bare HID keyboard usage.
    KeyUsage(u64),
    /// Press and release a named hardware button.
    Button(&'static str, u64, u64, Duration),
    /// Step the orientation 90° and report where the device landed, in degrees.
    ///
    /// Degrees rather than the service's own enum because this is the only
    /// place that can see that enum: there is no orientation *query* in
    /// CoreDevice, so a rotate's answer is the sole source of truth about which
    /// way up an iPhone is.
    Rotate {
        direction: RotationDirection,
        reply: oneshot::Sender<Option<i64>>,
    },
}

/// Queue depth for pending input.
///
/// Deliberately generous: the queue exists to absorb a browser's pointer burst,
/// and the consumer drains it as fast as the device accepts reports.
const INPUT_QUEUE: usize = 256;

/// Send half of the input actor.
#[derive(Clone)]
pub struct InputHandle {
    sender: mpsc::Sender<Input>,
}

impl InputHandle {
    /// Queue an action. Dropped with a warning if the device has fallen so far
    /// behind that the queue is full — blocking here would stall the caller's
    /// task (and, for the router, every other message behind it).
    pub fn send(&self, input: Input) {
        if let Err(err) = self.sender.try_send(input) {
            match err {
                mpsc::error::TrySendError::Full(_) => {
                    warn!("input queue full — dropping a HID report")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    debug!("input queue closed — the session is being rebuilt")
                }
            }
        }
    }

    /// Queue an action and await its reply.
    pub async fn request<T>(&self, make: impl FnOnce(oneshot::Sender<T>) -> Input) -> Option<T> {
        let (tx, rx) = oneshot::channel();
        self.sender.send(make(tx)).await.ok()?;
        rx.await.ok()
    }
}

pub fn channel() -> (InputHandle, mpsc::Receiver<Input>) {
    let (sender, receiver) = mpsc::channel(INPUT_QUEUE);
    (InputHandle { sender }, receiver)
}

/// The HID clients for one session.
pub struct HidClients {
    touch: UniversalHidServiceClient<Box<dyn ReadWrite>>,
    indigo: IndigoHidClient<Box<dyn ReadWrite>>,
    orientation: OrientationServiceClient<Box<dyn ReadWrite>>,
}

impl HidClients {
    /// Connect every HID surface.
    ///
    /// Must be called *after* the media stream is up: the surfaces only
    /// authenticate once it is, and backboardd needs a moment to re-match them.
    /// Connecting first yields clients that succeed and then silently drop
    /// every report.
    pub async fn connect(adapter: &mut AdapterHandle, handshake: &RsdHandshake) -> Result<Self> {
        let touch = crate::device::connect_service!(
            UniversalHidServiceClient<Box<dyn ReadWrite>>,
            adapter,
            handshake
        )?;
        let indigo = crate::device::connect_service!(
            IndigoHidClient<Box<dyn ReadWrite>>,
            adapter,
            handshake
        )?;
        let orientation = crate::device::connect_service!(
            OrientationServiceClient<Box<dyn ReadWrite>>,
            adapter,
            handshake
        )?;

        info!("HID surfaces connected");
        Ok(Self {
            touch,
            indigo,
            orientation,
        })
    }

    /// Drain the input queue until the session ends.
    pub async fn run(mut self, mut inputs: mpsc::Receiver<Input>) -> Result<()> {
        while let Some(input) = inputs.recv().await {
            // A failing HID surface means the session is gone; propagate so the
            // supervisor rebuilds rather than dropping input silently.
            self.apply(input).await?;
        }
        Ok(())
    }

    async fn apply(&mut self, input: Input) -> Result<()> {
        match input {
            Input::Contact { x, y } => self
                .touch
                .send_touchscreen(TOUCHSCREEN_STATE_CONTACT, x, y, None)
                .await
                .map_err(|err| anyhow!("touch contact: {err:?}"))?,
            Input::Release { x, y } => self
                .touch
                .send_touchscreen(TOUCHSCREEN_STATE_RELEASE, x, y, None)
                .await
                .map_err(|err| anyhow!("touch release: {err:?}"))?,
            Input::Character(character) => {
                let Some((usage, needs_shift)) = ascii_to_hid(character) else {
                    debug!(?character, "skipping untypeable character");
                    return Ok(());
                };
                if needs_shift {
                    self.key(KEY_LEFT_SHIFT, ButtonState::Down).await?;
                }
                self.key(usage, ButtonState::Down).await?;
                self.key(usage, ButtonState::Up).await?;
                if needs_shift {
                    self.key(KEY_LEFT_SHIFT, ButtonState::Up).await?;
                }
            }
            Input::KeyUsage(usage) => {
                self.key(usage, ButtonState::Down).await?;
                self.key(usage, ButtonState::Up).await?;
            }
            Input::Button(name, page, usage, hold) => {
                self.button(page, usage, ButtonState::Down).await?;
                tokio::time::sleep(hold).await;
                self.button(page, usage, ButtonState::Up).await?;
                debug!(name, "pressed hardware button");
            }
            Input::Rotate { direction, reply } => {
                let state = self
                    .orientation
                    .rotate(direction)
                    .await
                    .map_err(|err| anyhow!("rotate: {err:?}"))?;
                // `non_flat_orientation`, not `orientation`: a phone lying on a
                // desk answers `faceUp`, which says nothing about which way the
                // UI is drawn — and that is the farm's normal resting state.
                let degrees = orientation_degrees(&state.non_flat_orientation);
                if degrees.is_none() {
                    warn!(?state, "unrecognised orientation");
                }
                let _ = reply.send(degrees);
            }
        }
        Ok(())
    }

    async fn key(&mut self, usage: u64, state: ButtonState) -> Result<()> {
        self.indigo
            .send_keyboard(usage, state)
            .await
            .map_err(|err| anyhow!("keyboard usage {usage:#x}: {err:?}"))
    }

    async fn button(&mut self, page: u64, usage: u64, state: ButtonState) -> Result<()> {
        self.indigo
            .send_button(page, usage, state)
            .await
            .map_err(|err| anyhow!("button {page:#x}/{usage:#x}: {err:?}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalised_coordinates_scale_and_clamp() {
        assert_eq!(to_hid(0.0), 0);
        assert_eq!(to_hid(1.0), HID_MAX);
        assert_eq!(to_hid(0.5), 32768);
        assert_eq!(to_hid(-1.0), 0, "clamps below the screen");
        assert_eq!(to_hid(2.0), HID_MAX, "clamps beyond the screen");
    }

    #[test]
    fn key_names_are_the_browser_ones() {
        assert_eq!(special_key("Enter"), Some(0x28));
        assert_eq!(special_key("Backspace"), Some(0x2A));
        assert_eq!(special_key("ArrowUp"), Some(0x52));
        // STF's vocabulary is gone; nothing should still answer to it.
        assert_eq!(special_key("dpad_up"), None);
        assert_eq!(special_key("del"), None);
    }

    #[test]
    fn hardware_buttons_ignore_case() {
        assert_eq!(named_button("Home"), named_button("home"));
        assert_eq!(named_button("VolumeUp"), named_button("volume_up"));
    }

    #[test]
    fn typing_resolves_case_and_symbols() {
        assert_eq!(ascii_to_hid('a'), Some((0x04, false)));
        assert_eq!(ascii_to_hid('A'), Some((0x04, true)));
        assert_eq!(ascii_to_hid('1'), Some((0x1E, false)));
        assert_eq!(ascii_to_hid('!'), Some((0x1E, true)));
        assert_eq!(ascii_to_hid('0'), Some((0x27, false)));
        assert_eq!(ascii_to_hid(')'), Some((0x27, true)));
        assert_eq!(ascii_to_hid(' '), Some((0x2C, false)));
        assert_eq!(ascii_to_hid('é'), None);
    }

    #[test]
    fn power_and_lock_are_the_same_button() {
        assert_eq!(named_button("power"), named_button("lock"));
        let (page, usage, hold) = named_button("siri").unwrap();
        assert_eq!((page, usage), (USAGE_PAGE_CONSUMER, 0xCF));
        assert_eq!(hold, Duration::from_secs(1), "Siri needs a long press");
    }
}
