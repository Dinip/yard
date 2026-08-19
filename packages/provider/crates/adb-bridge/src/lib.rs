//! The **device** half of the ADB protocol.
//!
//! A provider runs this so that `adb connect provider:port` talks to *it*
//! rather than to the phone. It answers the connection banner, runs ADB's
//! challenge-response authentication against keys it was given, and translates
//! every service the client opens onto a transport the provider already holds.
//!
//! Why this exists: forwarding raw TCP to a device's own `adbd` makes the
//! *phone* the authenticator, so every developer's key has to be enrolled on
//! every device. Terminating the protocol here means the provider's key is the
//! only one any device ever trusts, and a client's key becomes an identity the
//! coordinator can check. STF reached the same conclusion — `adbkit`'s
//! `tcpusb` bridge is the equivalent, and this follows its shape.
//!
//! The crate knows nothing about this farm. It is given two things: something
//! that can authorize a public key, and something that can open a service.

pub mod auth;
pub mod bridge;
pub mod key;
pub mod message;

pub use auth::{AuthError, Authorizer, Handshake, Identity};
pub use bridge::{Bridge, ServiceOpener, Transport};
pub use key::{KeyError, PublicKey};
pub use message::{Command, FrameError, Message, MAX_PAYLOAD, VERSION};
