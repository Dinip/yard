//! A fake `adb` client, for driving the bridge over an in-memory stream.
//!
//! Cargo compiles this into every integration-test binary, so anything only one
//! of them uses looks dead to the others.
#![allow(dead_code)]

use std::sync::LazyLock;

use adb_bridge::message::{auth, Command, Message, MAX_PAYLOAD, VERSION};
use adb_bridge::PublicKey;
use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::traits::PublicKeyParts as _;
use rsa::{RsaPrivateKey, RsaPublicKey};
use sha1::Sha1;
use tokio::io::{AsyncRead, AsyncWrite};

/// Keys these tests sign with are generated, not committed.
///
/// The alternative was a private key in the repository, and a throwaway one is
/// still a private key in the history forever. Generating costs a couple of
/// seconds once per test binary and nothing is lost: what these tests need is a
/// *real* RSA signature over our challenge, not one particular key. The one
/// place a fixed key genuinely matters is the cross-language fingerprint
/// vector, and that only needs the public half — `adbkey.pub`, which is
/// committed and asserted against in `vectors.rs`.
///
/// `LazyLock`, because generation is the slow part and `test_key()` is called
/// several times per test.
static KEY: LazyLock<(PublicKey, RsaPrivateKey)> = LazyLock::new(generate);
static OTHER: LazyLock<(PublicKey, RsaPrivateKey)> = LazyLock::new(generate);

fn generate() -> (PublicKey, RsaPrivateKey) {
    // `rsa`'s own re-export: the workspace `rand` is a major version ahead, and
    // the two `rand_core` traits do not unify.
    let mut rng = rsa::rand_core::OsRng;
    let private = RsaPrivateKey::new(&mut rng, 2048).expect("keygen succeeds");
    let public = PublicKey::parse(&android_blob(&private.to_public_key(), "dev@example.test"))
        .expect("our own blob parses");
    (public, private)
}

/// Encode a public key the way `~/.android/adbkey.pub` does.
///
/// Android's own struct: `modulus_size_words`, `n0inv`, a little-endian
/// modulus, R², then the exponent. `n0inv` and R² are derivable from the
/// modulus and exist only to save the device's bootloader the work, so the
/// parser reads past them and this leaves them zero — writing them would be
/// inventing a second implementation of arithmetic nothing checks.
fn android_blob(key: &RsaPublicKey, comment: &str) -> String {
    const MODULUS_SIZE: usize = 256;

    let mut blob = vec![0u8; 524];
    blob[0..4].copy_from_slice(&((MODULUS_SIZE / 4) as u32).to_le_bytes());

    // Left-padded to the full width first: a modulus with a leading zero byte
    // is shorter big-endian, and reversing it unpadded would shift every byte.
    let mut modulus = [0u8; MODULUS_SIZE];
    let be = key.n().to_bytes_be();
    modulus[MODULUS_SIZE - be.len()..].copy_from_slice(&be);
    modulus.reverse();
    blob[8..8 + MODULUS_SIZE].copy_from_slice(&modulus);

    let mut exponent = [0u8; 4];
    let be = key.e().to_bytes_be();
    exponent[4 - be.len()..].copy_from_slice(&be);
    let exponent = u32::from_be_bytes(exponent);
    blob[8 + MODULUS_SIZE * 2..8 + MODULUS_SIZE * 2 + 4].copy_from_slice(&exponent.to_le_bytes());

    format!("{} {comment}", STANDARD.encode(&blob))
}

/// The throwaway key, public half.
pub fn test_key() -> PublicKey {
    KEY.0.clone()
}

/// The throwaway key, private half — so tests produce the signatures a real
/// `adb` would produce.
pub fn test_private_key() -> RsaPrivateKey {
    KEY.1.clone()
}

/// A second key, standing in for somebody else's laptop.
pub fn other_key() -> PublicKey {
    OTHER.0.clone()
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
