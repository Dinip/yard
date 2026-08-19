//! A fake `adb` client, for driving the bridge over an in-memory stream.
//!
//! Cargo compiles this into every integration-test binary, so anything only one
//! of them uses looks dead to the others.
#![allow(dead_code)]

use adb_bridge::message::{auth, Command, Message, MAX_PAYLOAD, VERSION};
use adb_bridge::PublicKey;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use sha1::Sha1;
use tokio::io::{AsyncRead, AsyncWrite};

pub const VECTORS: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../../protocol/test/vectors/"
);

pub fn vector(name: &str) -> String {
    std::fs::read_to_string(format!("{VECTORS}{name}")).expect("the vector exists")
}

/// The shared throwaway key, public half.
pub fn test_key() -> PublicKey {
    PublicKey::parse(&vector("adbkey.pub")).expect("the vector parses")
}

/// The shared throwaway key, private half — so tests produce the signatures a
/// real `adb` would produce.
pub fn test_private_key() -> RsaPrivateKey {
    RsaPrivateKey::from_pkcs8_pem(&vector("adbkey.testonly.pem")).expect("the private key parses")
}

/// A second key, standing in for somebody else's laptop.
pub fn other_key() -> PublicKey {
    PublicKey::parse(&vector("adbkey-other.pub")).expect("the second vector parses")
}

pub fn sign(key: &RsaPrivateKey, token: &[u8]) -> Vec<u8> {
    // What `adb` does: the 20-byte challenge is treated as a SHA-1 digest and
    // signed with the prefixed PKCS#1 v1.5 scheme.
    key.sign(Pkcs1v15Sign::new::<Sha1>(), token)
        .expect("signing succeeds")
}

/// Send the opening `CNXN` and read the challenge back.
pub async fn connect<S>(stream: &mut S) -> Vec<u8>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    Message::new(
        Command::Cnxn,
        VERSION,
        MAX_PAYLOAD,
        &b"host::features=cmd\0"[..],
    )
    .write(stream)
    .await
    .unwrap();
    expect_token(stream).await
}

pub async fn expect_token<S>(stream: &mut S) -> Vec<u8>
where
    S: AsyncRead + Unpin,
{
    let msg = Message::read(stream).await.expect("a challenge arrives");
    assert_eq!(msg.command, Command::Auth);
    assert_eq!(msg.arg0, auth::TOKEN, "expected a token");
    msg.payload.to_vec()
}

pub async fn send_signature<S>(stream: &mut S, key: &RsaPrivateKey, token: &[u8])
where
    S: AsyncWrite + Unpin,
{
    Message::new(Command::Auth, auth::SIGNATURE, 0, sign(key, token))
        .write(stream)
        .await
        .unwrap();
}

pub async fn send_public_key<S>(stream: &mut S, key: &PublicKey)
where
    S: AsyncWrite + Unpin,
{
    let payload = format!("{}\0", key.blob()).into_bytes();
    Message::new(Command::Auth, auth::RSAPUBLICKEY, 0, payload)
        .write(stream)
        .await
        .unwrap();
}
