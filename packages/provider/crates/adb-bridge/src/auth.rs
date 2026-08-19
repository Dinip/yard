//! ADB's challenge-response handshake, run by the provider instead of the phone.
//!
//! The device normally does this against `/data/misc/adb/adb_keys`, which is
//! why every developer's key has to be enrolled on every phone. Here the set of
//! acceptable keys comes from the coordinator, so the phone's own list never
//! grows past the provider's key.

use async_trait::async_trait;
use rand::Rng as _;
use tokio::io::{AsyncRead, AsyncWrite};
use tracing::{debug, warn};

use crate::key::{KeyError, PublicKey};
use crate::message::{auth, Command, FrameError, Message, MAX_PAYLOAD, VERSION};

/// Bytes in the challenge, matching what `adbd` issues.
const TOKEN_LEN: usize = 20;

/// How many authentication messages we will take before giving up.
///
/// `adb` sends one signature per key it holds and then offers one public key,
/// so a handful is generous. The cap is here because the loop is otherwise
/// driven entirely by whoever connected to the port.
const MAX_ATTEMPTS: usize = 16;

/// Who a connection turned out to belong to.
#[derive(Debug, Clone)]
pub struct Identity {
    pub user_id: String,
    pub fingerprint: String,
}

/// The farm's answer to "may this key drive this device?".
#[async_trait]
pub trait Authorizer: Send + Sync {
    /// Keys already entitled to this device's session.
    ///
    /// Fetched per connection rather than cached, so a key removed while a port
    /// is exposed stops working on the next connect.
    async fn entitled(&self) -> Vec<PublicKey>;

    /// Ask about a key none of the entitled ones matched.
    ///
    /// Implementations park here — the holder is being asked in a browser —
    /// and return the owning user id on approval, `None` on refusal.
    async fn request(&self, key: &PublicKey) -> Option<String>;
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("expected {expected} from the client, got {got:?}")]
    Unexpected { expected: &'static str, got: Command },
    #[error("the offered key is malformed: {0}")]
    BadKey(#[from] KeyError),
    #[error("the client offered a key it never signed with")]
    NoProofOfPossession,
    #[error("this key is not allowed to use this device")]
    Refused,
    #[error("the client sent {MAX_ATTEMPTS} authentication messages without authenticating")]
    TooManyAttempts,
}

/// What the handshake settled on.
pub struct Handshake {
    pub identity: Identity,
    /// The smaller of the two sides' limits; every later `WRTE` respects it.
    pub max_payload: u32,
}

/// Run the handshake, leaving the stream ready for `OPEN`.
///
/// `banner` is what the client sees as the device's identity and features —
/// proxied from the real device rather than invented, because a client that is
/// not told about `shell_v2` silently falls back to a shell with no exit codes.
/// Where the `device::…` banner comes from.
///
/// Indirect, rather than a `&str`, so it is resolved only once a client is
/// admitted. Reading it costs a round trip to the device, and an
/// unauthenticated connection must not be able to cause one.
#[async_trait]
pub trait BannerSource {
    async fn banner(&self) -> String;
}

#[async_trait]
impl BannerSource for &str {
    async fn banner(&self) -> String {
        (*self).to_owned()
    }
}

pub async fn authenticate<S, B>(
    stream: &mut S,
    authorizer: &dyn Authorizer,
    banner: B,
) -> Result<Handshake, AuthError>
where
    S: AsyncRead + AsyncWrite + Unpin,
    B: BannerSource,
{
    let connect = Message::read(stream).await?;
    if connect.command != Command::Cnxn {
        return Err(AuthError::Unexpected {
            expected: "CNXN",
            got: connect.command,
        });
    }
    let max_payload = connect.arg1.clamp(1, MAX_PAYLOAD);

    let entitled = authorizer.entitled().await;
    let mut token = new_token();
    // Every challenge we issued and what the client signed it with. The client
    // only reveals a public key *after* its signatures are refused, so proving
    // it holds the private half means going back over what it sent earlier.
    let mut attempted: Vec<([u8; TOKEN_LEN], Vec<u8>)> = Vec::new();

    Message::new(Command::Auth, auth::TOKEN, 0, token.to_vec())
        .write(stream)
        .await?;

    for _ in 0..MAX_ATTEMPTS {
        let msg = Message::read(stream).await?;
        debug!(?msg, "auth frame");
        if msg.command != Command::Auth {
            return Err(AuthError::Unexpected {
                expected: "AUTH",
                got: msg.command,
            });
        }

        match msg.arg0 {
            auth::SIGNATURE => {
                let signature = msg.payload.to_vec();
                if let Some(key) = entitled.iter().find(|k| k.verify(&token, &signature)) {
                    // The common case: a key the coordinator already told us
                    // about. Nobody is asked anything.
                    let identity = Identity {
                        user_id: owner_of(key),
                        fingerprint: key.fingerprint().to_owned(),
                    };
                    accept(stream, &banner.banner().await, max_payload).await?;
                    return Ok(Handshake {
                        identity,
                        max_payload,
                    });
                }

                attempted.push((token, signature));
                // A fresh challenge makes the client move on to its next key,
                // and once it runs out, offer a public key instead. Reusing the
                // token would let it replay the signature we just refused.
                token = new_token();
                Message::new(Command::Auth, auth::TOKEN, 0, token.to_vec())
                    .write(stream)
                    .await?;
            }

            auth::RSAPUBLICKEY => {
                let key = PublicKey::parse(&msg.payload_str())?;

                // `adbd` skips this and trusts the person tapping "allow" at the
                // device. Nobody is standing next to the phone here, so the key
                // has to have signed one of our challenges to count as offered
                // by whoever holds it.
                if !attempted
                    .iter()
                    .any(|(token, signature)| key.verify(token, signature))
                {
                    warn!(
                        fingerprint = key.fingerprint(),
                        "a client offered a key it had not signed with"
                    );
                    return Err(AuthError::NoProofOfPossession);
                }

                debug!(fingerprint = key.fingerprint(), "asking about an unknown key");
                let Some(user_id) = authorizer.request(&key).await else {
                    return Err(AuthError::Refused);
                };

                let identity = Identity {
                    user_id,
                    fingerprint: key.fingerprint().to_owned(),
                };
                accept(stream, &banner.banner().await, max_payload).await?;
                return Ok(Handshake {
                    identity,
                    max_payload,
                });
            }

            other => {
                debug!(arg0 = other, "ignoring an unknown AUTH type");
            }
        }
    }

    Err(AuthError::TooManyAttempts)
}

/// The `CNXN` that tells the client it is through.
async fn accept<S>(stream: &mut S, banner: &str, max_payload: u32) -> Result<(), FrameError>
where
    S: AsyncWrite + Unpin,
{
    Message::new(
        Command::Cnxn,
        VERSION,
        max_payload,
        format!("{banner}\0").into_bytes(),
    )
    .write(stream)
    .await
}

/// Which user an entitled key belongs to.
///
/// The coordinator sends the owner alongside the key; a key with no owner
/// cannot happen, but attributing it to nobody is better than guessing.
fn owner_of(key: &PublicKey) -> String {
    key.owner().unwrap_or_default().to_owned()
}

fn new_token() -> [u8; TOKEN_LEN] {
    let mut token = [0u8; TOKEN_LEN];
    // `ThreadRng` is a CSPRNG seeded from the OS. The token is the only thing
    // standing between a replayed signature and a shell, so this must not be a
    // convenience RNG.
    rand::rng().fill_bytes(&mut token);
    token
}
