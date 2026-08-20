//! The handshake, driven by a fake `adb` that signs real challenges.

mod common;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use adb_bridge::auth::{authenticate, AuthError, Authorizer};
use adb_bridge::message::{auth, Command, Message};
use adb_bridge::PublicKey;
use async_trait::async_trait;
use common::*;
use tokio::io::AsyncWriteExt;

/// An authorizer with a fixed entitled set and a scripted answer to the prompt.
struct Fake {
    entitled: Vec<PublicKey>,
    answer: Option<String>,
    asked: AtomicUsize,
}

impl Fake {
    fn entitled(keys: Vec<PublicKey>) -> Arc<Self> {
        Arc::new(Self {
            entitled: keys,
            answer: None,
            asked: AtomicUsize::new(0),
        })
    }

    fn asks_and_is(answer: Option<&str>) -> Arc<Self> {
        Arc::new(Self {
            entitled: Vec::new(),
            answer: answer.map(str::to_owned),
            asked: AtomicUsize::new(0),
        })
    }
}

#[async_trait]
impl Authorizer for Fake {
    async fn entitled(&self) -> Vec<PublicKey> {
        self.entitled.clone()
    }

    async fn request(&self, _key: &PublicKey) -> Option<String> {
        self.asked.fetch_add(1, Ordering::SeqCst);
        self.answer.clone()
    }
}

const BANNER: &str = "device::ro.product.name=farm;features=shell_v2,cmd";

#[tokio::test]
async fn a_known_key_is_admitted_without_asking_anyone() {
    let authorizer = Fake::entitled(vec![test_key().with_owner("user-1")]);
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    let token = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &token).await;

    let accepted = Message::read(&mut client).await.unwrap();
    assert_eq!(accepted.command, Command::Cnxn);
    assert_eq!(
        accepted.payload_str(),
        BANNER,
        "the device banner is passed through, so the client keeps shell_v2"
    );

    let handshake = bridge.await.unwrap().expect("the handshake succeeds");
    assert_eq!(handshake.identity.user_id, "user-1");
    assert_eq!(handshake.identity.fingerprint, test_key().fingerprint());
    assert_eq!(
        authorizer.asked.load(Ordering::SeqCst),
        0,
        "a registered key must never prompt the holder"
    );
}

#[tokio::test]
async fn an_unknown_key_is_offered_and_approved() {
    let authorizer = Fake::asks_and_is(Some("user-7"));
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    // What a real client does: sign, get refused, then offer the key itself.
    let token = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &token).await;
    let _second = expect_token(&mut client).await;
    send_public_key(&mut client, &test_key()).await;

    assert_eq!(
        Message::read(&mut client).await.unwrap().command,
        Command::Cnxn
    );
    let handshake = bridge.await.unwrap().expect("approval admits");
    assert_eq!(handshake.identity.user_id, "user-7");
    assert_eq!(authorizer.asked.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_refused_key_does_not_get_in() {
    let authorizer = Fake::asks_and_is(None);
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    let token = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &token).await;
    let _second = expect_token(&mut client).await;
    send_public_key(&mut client, &test_key()).await;

    // The holder said no.
    assert!(matches!(bridge.await.unwrap(), Err(AuthError::Refused)));
}

#[tokio::test]
async fn a_key_that_never_signed_anything_is_not_proof_of_anything() {
    // Anyone can read a public key off someone's laptop and paste it into an
    // `RSAPUBLICKEY` message. `adbd` accepts that and leans on the person
    // standing at the phone; nobody is standing at this one.
    let authorizer = Fake::asks_and_is(Some("user-1"));
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    connect(&mut client).await;
    send_public_key(&mut client, &test_key()).await;

    assert!(matches!(
        bridge.await.unwrap(),
        Err(AuthError::NoProofOfPossession)
    ));
    assert_eq!(
        authorizer.asked.load(Ordering::SeqCst),
        0,
        "the holder is never even shown a key nobody proved they hold"
    );
}

#[tokio::test]
async fn signing_with_one_key_and_offering_another_is_refused() {
    // A laptop with two keys signs with each in turn. The signatures we
    // collected must be checked against the key actually offered, not merely
    // counted.
    let authorizer = Fake::asks_and_is(Some("user-1"));
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    let token = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &token).await;
    let _second = expect_token(&mut client).await;
    send_public_key(&mut client, &other_key()).await;

    assert!(matches!(
        bridge.await.unwrap(),
        Err(AuthError::NoProofOfPossession)
    ));
}

#[tokio::test]
async fn a_second_key_still_gets_its_turn() {
    // The other half of the same story: a client whose first key is unknown
    // keeps signing, and the key it finally offers is one it did sign with.
    let authorizer = Fake::asks_and_is(Some("user-9"));
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    let first = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &first).await;
    let second = expect_token(&mut client).await;
    send_signature(&mut client, &test_private_key(), &second).await;
    let _third = expect_token(&mut client).await;
    send_public_key(&mut client, &test_key()).await;

    assert_eq!(
        Message::read(&mut client).await.unwrap().command,
        Command::Cnxn
    );
    assert!(bridge.await.unwrap().is_ok());
}

#[tokio::test]
async fn a_challenge_is_never_reissued() {
    let authorizer = Fake::asks_and_is(None);
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = {
        let authorizer = authorizer.clone();
        tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await })
    };

    let first = connect(&mut client).await;
    send_signature(&mut client, &test_private_key(), &first).await;
    let second = expect_token(&mut client).await;

    assert_ne!(
        first, second,
        "reusing the token would let a refused signature be replayed"
    );
    assert_eq!(second.len(), 20);
    drop(client);
    let _ = bridge.await.unwrap();
}

#[tokio::test]
async fn a_client_that_does_not_open_with_cnxn_is_refused() {
    let authorizer = Fake::entitled(vec![test_key().with_owner("user-1")]);
    let (mut client, mut server) = tokio::io::duplex(64 * 1024);

    let bridge = tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await });

    Message::empty(Command::Open, 1, 0)
        .write(&mut client)
        .await
        .unwrap();

    assert!(matches!(
        bridge.await.unwrap(),
        Err(AuthError::Unexpected {
            expected: "CNXN",
            ..
        })
    ));
}

#[tokio::test]
async fn an_endless_stream_of_auth_messages_gives_up() {
    let authorizer = Fake::asks_and_is(None);
    let (mut client, mut server) = tokio::io::duplex(1024 * 1024);

    let bridge = tokio::spawn(async move { authenticate(&mut server, &*authorizer, BANNER).await });

    connect(&mut client).await;
    // Whoever connected drives this loop, so it has to be bounded from here.
    for _ in 0..64 {
        let garbage = Message::new(Command::Auth, auth::SIGNATURE, 0, vec![0u8; 256]);
        if garbage.write(&mut client).await.is_err() {
            break;
        }
        let _ = Message::read(&mut client).await;
    }
    let _ = client.shutdown().await;

    assert!(matches!(
        bridge.await.unwrap(),
        Err(AuthError::TooManyAttempts) | Err(AuthError::Frame(_))
    ));
}
