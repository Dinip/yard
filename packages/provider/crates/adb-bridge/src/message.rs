//! ADB's wire framing.
//!
//! Every message is a 24-byte header of six little-endian `u32` — command,
//! two arguments, payload length, payload checksum, and the command's own
//! ones' complement — optionally followed by that many payload bytes.

use std::fmt;

use bytes::Bytes;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

pub const HEADER_LEN: usize = 24;

/// The largest payload we accept or send, and what we advertise in `CNXN`.
///
/// A client may propose more; we take the smaller of the two. It also bounds
/// what a single header can make us allocate, which matters because anyone who
/// can reach the port can send one.
pub const MAX_PAYLOAD: u32 = 256 * 1024;

/// The protocol version we speak.
///
/// Deliberately the older of the two: `0x01000001` adds delayed acknowledgements
/// — a client may then send several `WRTE`s without waiting — and there is no
/// reason to take that on when every stream here is bounded by an adb server
/// connection anyway. Clients negotiate down without complaint.
pub const VERSION: u32 = 0x0100_0000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Connection banner, both directions.
    Cnxn,
    /// Authentication: token, signature, or public key.
    Auth,
    /// Open a stream to a service.
    Open,
    /// Ready for more on a stream. Also the answer that a stream opened.
    Okay,
    /// Close a stream.
    Clse,
    /// Stream payload.
    Wrte,
    /// TLS upgrade, which we never offer.
    Stls,
    /// Legacy, never sent.
    Sync,
}

impl Command {
    pub fn code(self) -> u32 {
        match self {
            Command::Sync => 0x434e_5953,
            Command::Cnxn => 0x4e58_4e43,
            Command::Open => 0x4e45_504f,
            Command::Okay => 0x5941_4b4f,
            Command::Clse => 0x4553_4c43,
            Command::Wrte => 0x4554_5257,
            Command::Auth => 0x4854_5541,
            Command::Stls => 0x534c_5453,
        }
    }

    fn from_code(code: u32) -> Option<Self> {
        Some(match code {
            0x434e_5953 => Command::Sync,
            0x4e58_4e43 => Command::Cnxn,
            0x4e45_504f => Command::Open,
            0x5941_4b4f => Command::Okay,
            0x4553_4c43 => Command::Clse,
            0x4554_5257 => Command::Wrte,
            0x4854_5541 => Command::Auth,
            0x534c_5453 => Command::Stls,
            _ => return None,
        })
    }
}

/// `AUTH` argument 0.
pub mod auth {
    pub const TOKEN: u32 = 1;
    pub const SIGNATURE: u32 = 2;
    pub const RSAPUBLICKEY: u32 = 3;
}

#[derive(Clone, PartialEq, Eq)]
pub struct Message {
    pub command: Command,
    pub arg0: u32,
    pub arg1: u32,
    pub payload: Bytes,
}

#[derive(Debug, thiserror::Error)]
pub enum FrameError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("unknown adb command {0:#010x}")]
    UnknownCommand(u32),
    #[error("adb header magic does not match its command")]
    BadMagic,
    #[error("adb payload of {0} bytes exceeds the {MAX_PAYLOAD}-byte maximum")]
    PayloadTooLarge(u32),
}

impl Message {
    pub fn new(command: Command, arg0: u32, arg1: u32, payload: impl Into<Bytes>) -> Self {
        Self {
            command,
            arg0,
            arg1,
            payload: payload.into(),
        }
    }

    pub fn empty(command: Command, arg0: u32, arg1: u32) -> Self {
        Self::new(command, arg0, arg1, Bytes::new())
    }

    /// The payload as a string with any trailing NUL removed.
    ///
    /// Service names and connection banners are NUL-terminated on the wire, and
    /// leaving the NUL on turns `shell:` into a service nothing matches.
    pub fn payload_str(&self) -> String {
        String::from_utf8_lossy(&self.payload)
            .trim_end_matches('\0')
            .to_owned()
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(HEADER_LEN + self.payload.len());
        let code = self.command.code();
        out.extend_from_slice(&code.to_le_bytes());
        out.extend_from_slice(&self.arg0.to_le_bytes());
        out.extend_from_slice(&self.arg1.to_le_bytes());
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&checksum(&self.payload).to_le_bytes());
        out.extend_from_slice(&(!code).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out
    }

    pub async fn read<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Self, FrameError> {
        let mut header = [0u8; HEADER_LEN];
        reader.read_exact(&mut header).await?;

        let code = u32::from_le_bytes(header[0..4].try_into().unwrap());
        let arg0 = u32::from_le_bytes(header[4..8].try_into().unwrap());
        let arg1 = u32::from_le_bytes(header[8..12].try_into().unwrap());
        let length = u32::from_le_bytes(header[12..16].try_into().unwrap());
        let magic = u32::from_le_bytes(header[20..24].try_into().unwrap());

        if magic != !code {
            return Err(FrameError::BadMagic);
        }
        let command = Command::from_code(code).ok_or(FrameError::UnknownCommand(code))?;
        if length > MAX_PAYLOAD {
            return Err(FrameError::PayloadTooLarge(length));
        }

        // The checksum in bytes 16..20 is read past on purpose. Clients from
        // ADB 0x01000001 onwards send zero and never verify ours, so enforcing
        // it would refuse every modern `adb` while proving nothing about a
        // stream that is already framed by an explicit length.
        let mut payload = vec![0u8; length as usize];
        reader.read_exact(&mut payload).await?;

        Ok(Self {
            command,
            arg0,
            arg1,
            payload: Bytes::from(payload),
        })
    }

    pub async fn write<W: AsyncWrite + Unpin>(&self, writer: &mut W) -> Result<(), FrameError> {
        // One write: a header that reaches the client without its payload is a
        // client blocked reading bytes we have not sent.
        writer.write_all(&self.encode()).await?;
        writer.flush().await?;
        Ok(())
    }
}

fn checksum(payload: &[u8]) -> u32 {
    payload.iter().fold(0u32, |sum, b| sum.wrapping_add(*b as u32))
}

/// Payloads are arbitrary bytes; dumping them into a log is noise at best.
impl fmt::Debug for Message {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Message")
            .field("command", &self.command)
            .field("arg0", &self.arg0)
            .field("arg1", &self.arg1)
            .field("payload_len", &self.payload.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_message_survives_a_round_trip() {
        let msg = Message::new(Command::Wrte, 7, 9, Bytes::from_static(b"hello"));
        let mut cursor = std::io::Cursor::new(msg.encode());
        let back = Message::read(&mut cursor).await.unwrap();
        assert_eq!(back, msg);
    }

    #[tokio::test]
    async fn an_empty_payload_round_trips() {
        let msg = Message::empty(Command::Okay, 1, 2);
        let mut cursor = std::io::Cursor::new(msg.encode());
        assert_eq!(Message::read(&mut cursor).await.unwrap(), msg);
    }

    #[test]
    fn the_header_is_what_adb_expects() {
        let encoded = Message::new(Command::Cnxn, VERSION, MAX_PAYLOAD, &b"host::"[..]).encode();
        assert_eq!(encoded.len(), HEADER_LEN + 6);
        assert_eq!(&encoded[0..4], b"CNXN");
        assert_eq!(
            u32::from_le_bytes(encoded[12..16].try_into().unwrap()),
            6,
            "the length field is the payload length"
        );
        assert_eq!(
            u32::from_le_bytes(encoded[20..24].try_into().unwrap()),
            !u32::from_le_bytes(encoded[0..4].try_into().unwrap()),
            "magic is the command's ones' complement"
        );
    }

    #[tokio::test]
    async fn a_mismatched_magic_is_refused() {
        let mut encoded = Message::empty(Command::Okay, 0, 0).encode();
        encoded[20] ^= 0xff;
        let mut cursor = std::io::Cursor::new(encoded);
        assert!(matches!(
            Message::read(&mut cursor).await,
            Err(FrameError::BadMagic)
        ));
    }

    #[tokio::test]
    async fn an_oversized_length_is_refused_before_allocating() {
        // Anyone who can reach the port can claim a 4GB payload. Refusing on
        // the header is what keeps that from being an allocation.
        let mut header = Message::empty(Command::Wrte, 0, 0).encode();
        header[12..16].copy_from_slice(&u32::MAX.to_le_bytes());
        let mut cursor = std::io::Cursor::new(header);
        assert!(matches!(
            Message::read(&mut cursor).await,
            Err(FrameError::PayloadTooLarge(u32::MAX))
        ));
    }

    #[tokio::test]
    async fn an_unknown_command_is_refused() {
        let mut encoded = Message::empty(Command::Okay, 0, 0).encode();
        let bogus: u32 = 0x1234_5678;
        encoded[0..4].copy_from_slice(&bogus.to_le_bytes());
        encoded[20..24].copy_from_slice(&(!bogus).to_le_bytes());
        let mut cursor = std::io::Cursor::new(encoded);
        assert!(matches!(
            Message::read(&mut cursor).await,
            Err(FrameError::UnknownCommand(_))
        ));
    }

    #[test]
    fn a_service_name_loses_its_terminator() {
        let msg = Message::new(Command::Open, 1, 0, &b"shell:ls\0"[..]);
        assert_eq!(msg.payload_str(), "shell:ls");
    }
}
