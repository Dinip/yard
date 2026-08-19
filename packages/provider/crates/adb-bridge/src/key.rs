//! ADB public keys: Android's on-disk format, and the fingerprint we show.

use base64::engine::general_purpose::STANDARD;
use base64::Engine as _;
use rsa::pkcs1v15::Pkcs1v15Sign;
use rsa::pkcs8::EncodePublicKey;
use rsa::{BigUint, RsaPublicKey};
use sha1::Sha1;
use sha2::{Digest, Sha256};

/// Bytes in the on-disk blob: two `u32` headers, modulus, R², exponent.
const BLOB_SIZE: usize = 524;
/// 2048-bit keys only, which is all `adb` has ever generated.
const MODULUS_WORDS: u32 = 64;
const MODULUS_SIZE: usize = MODULUS_WORDS as usize * 4;

#[derive(Debug, thiserror::Error)]
pub enum KeyError {
    #[error("the key is empty")]
    Empty,
    #[error("the key is not valid base64")]
    NotBase64,
    #[error("expected a {BLOB_SIZE}-byte ADB public key, got {0} bytes")]
    WrongSize(usize),
    #[error("only 2048-bit ADB keys are supported")]
    UnsupportedSize,
    #[error("the key's modulus and exponent do not form an RSA key")]
    NotAnRsaKey,
}

/// A developer's ADB public key.
#[derive(Debug, Clone)]
pub struct PublicKey {
    inner: RsaPublicKey,
    blob: String,
    fingerprint: String,
    comment: Option<String>,
    /// Whose key this is, when the coordinator has told us. A key offered by a
    /// client has nobody attached to it yet — that is the question being asked.
    owner: Option<String>,
}

impl PublicKey {
    /// Parse one line of `~/.android/adbkey.pub`, or the payload of an
    /// `AUTH RSAPUBLICKEY` message, which carry the same thing.
    ///
    /// The format is Android's own: base64 of a little-endian `RSAPublicKey`
    /// struct — `modulus_size_words`, `n0inv`, a 256-byte little-endian
    /// modulus, a 256-byte R², then the exponent — optionally followed by a
    /// space and a comment like `dev@example.test`. It is neither PEM nor an
    /// OpenSSH key, so nothing off the shelf reads it.
    ///
    /// `n0inv` and R² are derivable from the modulus and exist only to save the
    /// device's Montgomery reduction the work, so they are read past rather
    /// than checked.
    pub fn parse(contents: &str) -> Result<Self, KeyError> {
        let mut parts = contents.split_whitespace();
        let encoded = parts.next().filter(|s| !s.is_empty()).ok_or(KeyError::Empty)?;
        let comment = {
            let rest: Vec<&str> = parts.collect();
            (!rest.is_empty()).then(|| rest.join(" "))
        };

        let blob = STANDARD.decode(encoded).map_err(|_| KeyError::NotBase64)?;
        if blob.len() != BLOB_SIZE {
            return Err(KeyError::WrongSize(blob.len()));
        }
        if u32::from_le_bytes(blob[0..4].try_into().unwrap()) != MODULUS_WORDS {
            return Err(KeyError::UnsupportedSize);
        }

        // Little-endian on disk, big-endian everywhere else in cryptography.
        let mut modulus = blob[8..8 + MODULUS_SIZE].to_vec();
        modulus.reverse();
        let exponent = u32::from_le_bytes(
            blob[8 + MODULUS_SIZE * 2..8 + MODULUS_SIZE * 2 + 4]
                .try_into()
                .unwrap(),
        );

        let inner = RsaPublicKey::new(
            BigUint::from_bytes_be(&modulus),
            BigUint::from(exponent),
        )
        .map_err(|_| KeyError::NotAnRsaKey)?;

        Ok(Self {
            fingerprint: fingerprint_of(&inner),
            inner,
            blob: encoded.to_owned(),
            comment,
            owner: None,
        })
    }

    /// Attach the owner the coordinator named.
    pub fn with_owner(mut self, user_id: impl Into<String>) -> Self {
        self.owner = Some(user_id.into());
        self
    }

    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }

    /// `SHA256:<base64>`, unpadded, over the DER SubjectPublicKeyInfo.
    ///
    /// The coordinator derives the same string in TypeScript and they must
    /// agree exactly, or a key someone already registered looks unknown and the
    /// holder is asked to approve it again. `tests/vectors.rs` and
    /// `protocol/test/adbkey.test.ts` assert against one shared key.
    pub fn fingerprint(&self) -> &str {
        &self.fingerprint
    }

    /// The base64 blob alone, which is what the coordinator stores.
    pub fn blob(&self) -> &str {
        &self.blob
    }

    pub fn comment(&self) -> Option<&str> {
        self.comment.as_deref()
    }

    /// Does `signature` prove possession of this key's private half?
    ///
    /// `token` is the 20 random bytes we challenged with. ADB treats them as a
    /// SHA-1 digest and signs with PKCS#1 v1.5 including the SHA-1 DigestInfo
    /// prefix, so verification uses the prefixed scheme even though nothing was
    /// actually hashed.
    pub fn verify(&self, token: &[u8], signature: &[u8]) -> bool {
        self.inner
            .verify(Pkcs1v15Sign::new::<Sha1>(), token, signature)
            .is_ok()
    }
}

fn fingerprint_of(key: &RsaPublicKey) -> String {
    let der = key
        .to_public_key_der()
        // Infallible for a key that already parsed: the encoding cannot fail
        // once the modulus and exponent are in hand.
        .expect("a parsed RSA key encodes to DER");
    let digest = Sha256::digest(der.as_bytes());
    format!("SHA256:{}", base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest))
}
