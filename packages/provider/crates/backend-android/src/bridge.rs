//! Wiring the ADB bridge to a real device.
//!
//! The bridge speaks the protocol; this supplies the two things it cannot know
//! on its own — which keys may drive this device, and how to open a service on
//! it. Opening one is the same request the provider makes for everything else:
//! `host:transport:<serial>` on the adb server, then the service string.

use std::sync::Arc;

use adb_bridge::bridge::{ServiceOpener, Transport};
use adb_bridge::{Authorizer, PublicKey};
use anyhow::{Context, Result};
use async_trait::async_trait;
use provider_core::adb_auth::AdbAuthority;
use tokio::sync::Mutex;
use tracing::{debug, warn};

use crate::adb::{Adb, AdbStream};

/// The farm's answer to "may this key drive this device?", for one device.
pub struct DeviceAuthorizer {
    authority: Arc<AdbAuthority>,
}

impl DeviceAuthorizer {
    pub fn new(authority: Arc<AdbAuthority>) -> Self {
        Self { authority }
    }
}

#[async_trait]
impl Authorizer for DeviceAuthorizer {
    async fn entitled(&self) -> Vec<PublicKey> {
        self.authority
            .entitled_keys()
            .await
            .into_iter()
            .filter_map(|key| match PublicKey::parse(&key.public_key) {
                Ok(parsed) => Some(parsed.with_owner(key.user_id)),
                // A key the coordinator accepted that we cannot parse is a bug
                // on one side or the other; dropping one key beats refusing
                // every connection to this device.
                Err(err) => {
                    warn!(
                        fingerprint = %key.fingerprint,
                        error = %err,
                        "ignoring an entitled key that will not parse"
                    );
                    None
                }
            })
            .collect()
    }

    async fn request(&self, key: &PublicKey) -> Option<String> {
        self.authority
            .approve(key.fingerprint(), key.blob(), key.comment())
            .await
    }
}

/// Opens ADB services on one device, through the provider's own adb server.
pub struct DeviceServices {
    adb: Adb,
    serial: String,
    authority: Arc<AdbAuthority>,
    /// The banner, built once. It costs a `getprop` round trip and cannot
    /// change while the device is attached.
    banner: Mutex<Option<String>>,
}

impl DeviceServices {
    pub fn new(adb: Adb, serial: String, authority: Arc<AdbAuthority>) -> Self {
        Self {
            adb,
            serial,
            authority,
            banner: Mutex::new(None),
        }
    }

    /// What a real `adbd` would have answered `CNXN` with.
    ///
    /// The feature list is the part that matters: it is how a client learns it
    /// may use `shell,v2:` — separate stderr and a real exit code — and `cmd:`.
    /// Inventing one silently downgrades every `adb shell` on the farm, so it
    /// comes from the device.
    async fn build_banner(&self) -> Result<String> {
        let features = self.features().await.unwrap_or_else(|err| {
            debug!(error = %format!("{err:#}"), "no feature list; falling back to the basics");
            // What every `adbd` since Android 7 has. A short list costs a
            // client conveniences; a wrong one makes it speak a protocol the
            // device does not.
            "shell_v2,cmd,stat_v2".to_owned()
        });

        let props = self
            .adb
            .shell(
                &self.serial,
                "getprop ro.product.name; getprop ro.product.model; getprop ro.product.device",
            )
            .await
            .unwrap_or_default();
        let mut lines = props.lines().map(str::trim);
        let name = lines.next().unwrap_or("").to_owned();
        let model = lines.next().unwrap_or("").to_owned();
        let device = lines.next().unwrap_or("").to_owned();

        Ok(format!(
            "device::ro.product.name={name};ro.product.model={model};\
             ro.product.device={device};features={features}"
        ))
    }

    async fn features(&self) -> Result<String> {
        let mut stream = AdbStream::connect(self.adb.server()).await?;
        stream
            .request(&format!("host-serial:{}:features", self.serial))
            .await
            .context("asking the adb server for the device's features")?;
        stream.payload().await
    }
}

#[async_trait]
impl ServiceOpener for DeviceServices {
    async fn open(&self, service: &str) -> Result<Box<dyn Transport>> {
        let mut stream = self.adb.transport(&self.serial).await?;
        stream
            .request(service)
            .await
            .with_context(|| format!("opening {service:?} on {}", self.serial))?;
        Ok(Box::new(stream.into_inner()))
    }

    async fn banner(&self) -> String {
        let mut cached = self.banner.lock().await;
        if let Some(banner) = cached.as_ref() {
            return banner.clone();
        }
        let banner = self.build_banner().await.unwrap_or_else(|err| {
            warn!(error = %format!("{err:#}"), "could not read the device banner");
            "device::features=shell_v2,cmd,stat_v2".to_owned()
        });
        *cached = Some(banner.clone());
        banner
    }

    async fn activity(&self) {
        self.authority.note_activity().await;
    }
}
