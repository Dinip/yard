//! The bridge against a real `adb` client.
//!
//! Ignored by default: it needs the `adb` binary and it talks to the machine's
//! adb server, which a CI runner has no reason to have. Run it by hand when
//! touching the handshake or the multiplexer:
//!
//! ```sh
//! cargo test -p adb-bridge --test real_adb -- --ignored --nocapture
//! ```
//!
//! It earns its keep because nothing else here proves the protocol against the
//! only client that matters. The unit tests drive a fake client built from the
//! same code as the bridge, so a shared misreading of ADB would pass all of
//! them: the banner, the flow control and the framing are only really settled
//! by `adb` itself accepting them.

use std::process::Command as Exec;
use std::sync::Arc;

use adb_bridge::auth::Authorizer;
use adb_bridge::bridge::{Bridge, ServiceOpener, Transport};
use adb_bridge::PublicKey;
use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

/// Accepts whatever key the developer's own `adb` offers.
///
/// Fine here and nowhere else: the point of this test is the protocol, and the
/// real authorization path is covered by `authentication.rs`.
struct AcceptAnyone;

#[async_trait]
impl Authorizer for AcceptAnyone {
    async fn entitled(&self) -> Vec<PublicKey> {
        Vec::new()
    }
    async fn request(&self, key: &PublicKey) -> Option<String> {
        println!("approving {}", key.fingerprint());
        Some("local-developer".into())
    }
}

/// A device whose every service prints one line and hangs up.
struct Canned;

#[async_trait]
impl ServiceOpener for Canned {
    async fn open(&self, service: &str) -> anyhow::Result<Box<dyn Transport>> {
        println!("open {service}");
        let (near, mut far) = tokio::io::duplex(64 * 1024);
        let line = format!("served {service}\n");
        tokio::spawn(async move {
            let _ = far.write_all(line.as_bytes()).await;
            let _ = far.shutdown().await;
        });
        Ok(Box::new(near))
    }

    async fn banner(&self) -> String {
        "device::ro.product.name=farm;ro.product.model=bridge;features=cmd,shell_v2".into()
    }

    async fn activity(&self) {}
}

// Multi-threaded on purpose: `adb` is driven with a blocking `Command`, and on
// the default single-threaded test runtime that starves the accept loop it is
// trying to reach.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "needs the adb binary and the machine's adb server"]
async fn a_real_adb_client_connects_and_runs_a_shell() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter("adb_bridge=debug")
        .try_init();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = format!("127.0.0.1:{port}");

    tokio::spawn(async move {
        while let Ok((socket, peer)) = listener.accept().await {
            tokio::spawn(async move {
                let bridge = Bridge::new(Arc::new(AcceptAnyone), Arc::new(Canned));
                if let Err(err) = bridge.serve(socket, &peer.to_string()).await {
                    eprintln!("bridge refused {peer}: {err}");
                }
            });
        }
    });

    // `adb connect` reports whatever state the transport is in when it
    // returns, which can be mid-authentication. What matters is where it lands.
    println!("connect: {}", adb(&["connect", &target]));
    println!("wait: {}", adb(&["-s", &target, "wait-for-device"]));

    let devices = adb(&["devices"]);
    assert!(
        devices.contains(&format!("{target}\tdevice")),
        "the bridge never came up as an authorized device:\n{devices}"
    );

    let shell = adb(&["-s", &target, "shell", "echo", "hi"]);
    assert!(
        shell.contains("served shell"),
        "the shell service was not reached: {shell}"
    );

    adb(&["disconnect", &target]);
}

fn adb(args: &[&str]) -> String {
    let out = Exec::new("adb")
        .args(args)
        .output()
        .expect("the adb binary is on PATH");
    format!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    )
}
