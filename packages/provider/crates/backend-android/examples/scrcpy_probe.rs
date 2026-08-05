//! Push the scrcpy server, start it, and dump what actually comes back.
//!
//! `cargo run -p backend-android --example scrcpy_probe`
//!
//! The handshake is the highest-uncertainty part of the Android backend, and
//! scrcpy's wire format is documented only by its own source — which changes
//! between releases. So this reads the real bytes off a real device rather
//! than trusting a reading of that source, and everything else is built on
//! what it prints.

use anyhow::{Context as _, Result};
use backend_android::adb::{Adb, AdbStream, DEFAULT_ADB_SERVER};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

const SERVER_JAR: &[u8] = include_bytes!("../../../vendor/scrcpy-server-v4.1");
const SERVER_VERSION: &str = "4.1";
const REMOTE_PATH: &str = "/data/local/tmp/farm-scrcpy-server.jar";

#[tokio::main]
async fn main() -> Result<()> {
    let adb = Adb::new(DEFAULT_ADB_SERVER);
    let device = adb
        .devices()
        .await?
        .into_iter()
        .find(|d| d.is_usable())
        .context("no usable device attached")?;
    let serial = device.serial.clone();
    println!("device: {serial} ({:?})", device.model);

    adb.push(&serial, REMOTE_PATH, SERVER_JAR, 0o644).await?;
    println!("pushed {} bytes", SERVER_JAR.len());

    // tunnel_forward=true means the *server* listens on a device-side abstract
    // socket and we dial in. That avoids `adb reverse` and any host port
    // allocation: the adb transport can open a localabstract socket directly.
    let scid = 0x0000_0042u32;
    let command = format!(
        "CLASSPATH={REMOTE_PATH} app_process / com.genymobile.scrcpy.Server {SERVER_VERSION} \
         scid={scid:08x} log_level=info video=true audio=false control=true tunnel_forward=true \
         video_codec=h264 max_size=1024 video_bit_rate=4000000 max_fps=30 cleanup=true"
    );
    println!("starting: {command}");

    let mut server = adb.shell_stream(&serial, &command).await?;

    // Give the server a moment to bind before dialling in.
    tokio::time::sleep(std::time::Duration::from_millis(800)).await;

    let socket_name = format!("localabstract:scrcpy_{scid:08x}");
    let mut video = connect(&adb, &serial, &socket_name).await?;
    let mut control = connect(&adb, &serial, &socket_name).await?;
    println!("both sockets connected");

    // Everything from here is what we are trying to learn. Read exactly, never
    // `read()` once: a socket is free to hand over one byte at a time, and the
    // first version of this probe misparsed the whole handshake because of it.
    let mut dummy = [0u8; 1];
    video.read_exact(&mut dummy).await?;

    let mut name = [0u8; 64];
    video.read_exact(&mut name).await?;
    let name_end = name.iter().position(|&b| b == 0).unwrap_or(64);

    let mut codec = [0u8; 4];
    video.read_exact(&mut codec).await?;

    let mut meta = [0u8; 12];
    video.read_exact(&mut meta).await?;

    println!("\ndummy byte      : {:#04x}", dummy[0]);
    println!(
        "device name     : {:?}",
        String::from_utf8_lossy(&name[..name_end])
    );
    println!(
        "codec id        : {:#010x} ({:?})",
        u32::from_be_bytes(codec),
        String::from_utf8_lossy(&codec)
    );
    println!("session meta    :");
    hexdump(&meta);
    println!(
        "  flags={:#010x} width={} height={}",
        u32::from_be_bytes(meta[0..4].try_into().unwrap()),
        u32::from_be_bytes(meta[4..8].try_into().unwrap()),
        u32::from_be_bytes(meta[8..12].try_into().unwrap()),
    );

    // A frame header, to confirm the per-packet framing.
    let mut header = [0u8; 12];
    video.read_exact(&mut header).await?;
    let pts_flags = u64::from_be_bytes(header[..8].try_into().unwrap());
    let size = u32::from_be_bytes(header[8..].try_into().unwrap());
    println!(
        "\nfirst packet    : flags={:#05b} pts={} size={size}",
        pts_flags >> 61,
        pts_flags & ((1 << 61) - 1)
    );

    let mut payload = vec![0u8; size as usize];
    video.read_exact(&mut payload).await?;
    println!(
        "payload head    : {:02x?}",
        &payload[..payload.len().min(40)]
    );

    // And the next one, to see a real frame after the config packet.
    let mut header = [0u8; 12];
    video.read_exact(&mut header).await?;
    let pts_flags = u64::from_be_bytes(header[..8].try_into().unwrap());
    let size = u32::from_be_bytes(header[8..].try_into().unwrap());
    println!(
        "second packet   : flags={:#05b} pts={} size={size}",
        pts_flags >> 61,
        pts_flags & ((1 << 61) - 1)
    );
    let mut payload = vec![0u8; size as usize];
    video.read_exact(&mut payload).await?;
    println!(
        "payload head    : {:02x?}",
        &payload[..payload.len().min(16)]
    );

    // A touch, to confirm the control socket is live. Bottom-centre, well away
    // from anything: a tap at 50% x, 90% y on the navigation area.
    println!("\nsending a no-op control message (get clipboard)");
    control.write_all(&[8, 0]).await?;
    control.flush().await?;

    let mut reply = [0u8; 16];
    match tokio::time::timeout(std::time::Duration::from_secs(2), control.read(&mut reply)).await {
        Ok(Ok(n)) => {
            println!("control replied with {n} bytes:");
            hexdump(&reply[..n]);
        }
        Ok(Err(err)) => println!("control read failed: {err}"),
        Err(_) => println!("control stayed silent (clipboard may be empty)"),
    }

    // The server's own log, which says what it decided to do.
    println!("\n--- server log ---");
    let mut log = vec![0u8; 2048];
    if let Ok(Ok(n)) = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        server.inner_mut().read(&mut log),
    )
    .await
    {
        print!("{}", String::from_utf8_lossy(&log[..n]));
    }

    Ok(())
}

async fn connect(adb: &Adb, serial: &str, socket_name: &str) -> Result<tokio::net::TcpStream> {
    let mut stream = adb.transport(serial).await?;
    stream
        .request(socket_name)
        .await
        .with_context(|| format!("connecting to {socket_name} on the device"))?;
    Ok(AdbStream::into_inner(stream))
}

fn hexdump(bytes: &[u8]) {
    for (offset, chunk) in bytes.chunks(16).enumerate() {
        let hex: Vec<String> = chunk.iter().map(|b| format!("{b:02x}")).collect();
        let ascii: String = chunk
            .iter()
            .map(|&b| {
                if (0x20..0x7f).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();
        println!("{:04x}  {:<48}  {ascii}", offset * 16, hex.join(" "));
    }
}
