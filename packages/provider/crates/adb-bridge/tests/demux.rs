//! Streams, once the handshake is out of the way.

mod common;

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use adb_bridge::auth::Authorizer;
use adb_bridge::bridge::{Bridge, ServiceOpener, Transport};
use adb_bridge::message::{Command, Message};
use adb_bridge::PublicKey;
use async_trait::async_trait;
use common::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct KnownKey(PublicKey);

#[async_trait]
impl Authorizer for KnownKey {
    async fn entitled(&self) -> Vec<PublicKey> {
        vec![self.0.clone()]
    }
    async fn request(&self, _key: &PublicKey) -> Option<String> {
        panic!("a registered key must never reach the prompt");
    }
}

/// A device that echoes, plus a record of what was asked for.
#[derive(Default)]
struct FakeDevice {
    opened: Mutex<Vec<String>>,
    /// Bytes a service emits the moment it is opened.
    greetings: HashMap<String, Vec<u8>>,
    fails: Mutex<Vec<String>>,
    activity: AtomicUsize,
}

#[async_trait]
impl ServiceOpener for FakeDevice {
    async fn open(&self, service: &str) -> anyhow::Result<Box<dyn Transport>> {
        self.opened.lock().unwrap().push(service.to_owned());
        if self.fails.lock().unwrap().iter().any(|f| f == service) {
            anyhow::bail!("no such service");
        }

        let (near, mut far) = tokio::io::duplex(64 * 1024);
        let greeting = self.greetings.get(service).cloned();
        tokio::spawn(async move {
            if let Some(greeting) = greeting {
                let _ = far.write_all(&greeting).await;
            }
            // Echo, so a test can prove bytes reached the device and came back.
            let mut buf = [0u8; 1024];
            while let Ok(n) = far.read(&mut buf).await {
                if n == 0 || far.write_all(&buf[..n]).await.is_err() {
                    break;
                }
            }
        });
        Ok(Box::new(near))
    }

    async fn banner(&self) -> String {
        "device::ro.product.name=farm;features=shell_v2,cmd".into()
    }

    async fn activity(&self) {
        self.activity.fetch_add(1, Ordering::SeqCst);
    }
}

/// A test client that keeps frames it was not looking for.
///
/// With two streams live, the answer to an `OPEN` can arrive behind another
/// stream's output — that interleaving is the whole point of the multiplexer,
/// so the client has to tolerate it rather than assume frame order.
struct Client {
    stream: tokio::io::DuplexStream,
    pending: Vec<Message>,
    _task: tokio::task::JoinHandle<()>,
}

impl Client {
    /// Authenticate against a bridge backed by `device`.
    async fn connected(device: Arc<FakeDevice>) -> Self {
        let bridge = Bridge::new(
            Arc::new(KnownKey(test_key().with_owner("user-1"))),
            device.clone(),
        );
        let (mut stream, server) = tokio::io::duplex(256 * 1024);
        let task = tokio::spawn(async move {
            let _ = bridge.serve(server, "test").await;
        });

        let token = connect(&mut stream).await;
        send_signature(&mut stream, &test_private_key(), &token).await;
        assert_eq!(
            Message::read(&mut stream).await.unwrap().command,
            Command::Cnxn
        );
        Self {
            stream,
            pending: Vec::new(),
            _task: task,
        }
    }

    /// Ask for a service and return the frame that answers, ignoring traffic
    /// belonging to other streams.
    async fn open(&mut self, remote: u32, service: &str) -> Message {
        Message::new(
            Command::Open,
            remote,
            0,
            format!("{service}\0").into_bytes(),
        )
        .write(&mut self.stream)
        .await
        .unwrap();
        self.next_where(|msg| {
            msg.arg1 == remote && matches!(msg.command, Command::Okay | Command::Clse)
        })
        .await
    }

    async fn next(&mut self) -> Message {
        if !self.pending.is_empty() {
            return self.pending.remove(0);
        }
        Message::read(&mut self.stream).await.unwrap()
    }

    async fn next_where(&mut self, want: impl Fn(&Message) -> bool) -> Message {
        if let Some(i) = self.pending.iter().position(&want) {
            return self.pending.remove(i);
        }
        loop {
            let msg = Message::read(&mut self.stream).await.unwrap();
            if want(&msg) {
                return msg;
            }
            self.pending.push(msg);
        }
    }

    async fn write(&mut self, remote: u32, local: u32, data: &[u8]) {
        Message::new(Command::Wrte, remote, local, data.to_vec())
            .write(&mut self.stream)
            .await
            .unwrap();
    }

    async fn send(&mut self, msg: Message) {
        msg.write(&mut self.stream).await.unwrap();
    }
}

#[tokio::test]
async fn a_service_is_opened_upstream_and_bytes_flow_both_ways() {
    let device = Arc::new(FakeDevice::default());
    let mut client = Client::connected(device.clone()).await;

    let okay = client.open(11, "shell:echo hi").await;
    assert_eq!(okay.command, Command::Okay);
    assert_eq!(okay.arg1, 11, "the client's own id comes back to it");
    let local = okay.arg0;

    assert_eq!(
        device.opened.lock().unwrap().as_slice(),
        ["shell:echo hi"],
        "the service string is passed through untouched"
    );

    client.write(11, local, b"ping").await;

    let echoed = client
        .next_where(|msg| msg.command == Command::Wrte && msg.arg0 == local)
        .await;
    assert_eq!(echoed.payload, &b"ping"[..], "bytes reached the device and came back");
}

#[tokio::test]
async fn a_stream_is_acknowledged_before_it_says_anything() {
    // The bug this replaces: the pump task started before the `OKAY` went out,
    // so a service that speaks the instant it opens — `logcat`, a shell
    // printing its prompt — could land its first `WRTE` in front of the frame
    // that tells the client the stream exists at all. The client has no id to
    // match it to and drops it.
    let mut device = FakeDevice::default();
    device
        .greetings
        .insert("shell:".into(), b"immediately".to_vec());
    let device = Arc::new(device);
    let mut client = Client::connected(device.clone()).await;

    Message::new(Command::Open, 77, 0, &b"shell:\0"[..])
        .write(&mut client.stream)
        .await
        .unwrap();

    let first = client.next().await;
    assert_eq!(
        first.command,
        Command::Okay,
        "the acknowledgement has to come first, whatever the service does"
    );
    assert_eq!(client.next().await.payload_str(), "immediately");
}

#[tokio::test]
async fn two_streams_do_not_interfere() {
    let mut device = FakeDevice::default();
    device.greetings.insert("shell:one".into(), b"first".to_vec());
    device.greetings.insert("shell:two".into(), b"second".to_vec());
    let device = Arc::new(device);
    let mut client = Client::connected(device.clone()).await;

    let a = client.open(100, "shell:one").await.arg0;
    let b = client.open(200, "shell:two").await.arg0;
    assert_ne!(a, b, "each stream gets its own id");

    let first = client
        .next_where(|m| m.command == Command::Wrte && m.arg0 == a)
        .await;
    let second = client
        .next_where(|m| m.command == Command::Wrte && m.arg0 == b)
        .await;
    assert_eq!(first.payload_str(), "first");
    assert_eq!(second.payload_str(), "second");
    assert_eq!(first.arg1, 100, "each frame carries the client's id for its own stream");
    assert_eq!(second.arg1, 200);
}

#[tokio::test]
async fn closing_one_stream_leaves_the_other_alone() {
    let mut device = FakeDevice::default();
    device.greetings.insert("shell:two".into(), b"still here".to_vec());
    let device = Arc::new(device);
    let mut client = Client::connected(device.clone()).await;

    let doomed = client.open(100, "shell:one").await.arg0;
    client.send(Message::empty(Command::Clse, 100, doomed)).await;

    let b = client.open(200, "shell:two").await.arg0;
    let msg = client
        .next_where(|m| m.command == Command::Wrte && m.arg0 == b)
        .await;
    assert_eq!(msg.payload_str(), "still here");
}

#[tokio::test]
async fn a_service_the_device_refuses_closes_only_that_stream() {
    let device = Arc::new(FakeDevice::default());
    device.fails.lock().unwrap().push("jdwp:9999".into());
    let mut client = Client::connected(device.clone()).await;

    let answer = client.open(42, "jdwp:9999").await;
    assert_eq!(answer.command, Command::Clse);
    assert_eq!(answer.arg0, 0, "a zero local id means the open failed");

    // The connection is still usable.
    assert_eq!(client.open(43, "shell:").await.command, Command::Okay);
}

#[tokio::test]
async fn root_remount_and_unroot_are_refused_with_an_explanation() {
    for service in ["root:", "unroot:", "remount:"] {
        let device = Arc::new(FakeDevice::default());
        let mut client = Client::connected(device.clone()).await;

        assert_eq!(client.open(1, service).await.command, Command::Okay);

        let told = client.next().await;
        assert_eq!(told.command, Command::Wrte);
        assert!(
            told.payload_str().contains("farm device"),
            "the person running `adb {service}` has to be told why, not just cut off"
        );
        assert_eq!(client.next().await.command, Command::Clse);

        assert!(
            device.opened.lock().unwrap().is_empty(),
            "a refused service must never reach the device"
        );
    }
}

#[tokio::test]
async fn traffic_counts_as_using_the_device() {
    // The coordinator's idle timeout is the reason this matters: someone
    // working entirely over `adb` is using the device, and nothing else in the
    // system can see it happen.
    let device = Arc::new(FakeDevice::default());
    let mut client = Client::connected(device.clone()).await;

    let local = client.open(1, "shell:").await.arg0;
    client.write(1, local, b"ls\n").await;
    client
        .next_where(|m| m.command == Command::Wrte || m.command == Command::Okay)
        .await;

    assert!(
        device.activity.load(Ordering::SeqCst) >= 2,
        "opening a stream and writing to it are both activity"
    );
}

#[tokio::test]
async fn the_device_hanging_up_closes_the_stream() {
    let mut device = FakeDevice::default();
    device.greetings.insert("shell:echo hi".into(), b"hi\n".to_vec());
    let device = Arc::new(device);
    let mut client = Client::connected(device.clone()).await;

    let local = client.open(5, "shell:echo hi").await.arg0;
    let data = client
        .next_where(|m| m.command == Command::Wrte && m.arg0 == local)
        .await;
    assert_eq!(data.payload_str(), "hi\n");

    // A stream the client walks away from must not leave the connection stuck.
    client.send(Message::empty(Command::Clse, 5, local)).await;
    assert_eq!(client.open(6, "shell:").await.command, Command::Okay);
}
