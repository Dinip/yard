//! Listing installed apps on iOS 26 and later.
//!
//! `AppServiceClient::list_apps` cannot talk to these devices. The request's
//! options dictionary is decoded on the device into a struct that gained a
//! required `requireContainerAccess` key, and a request without it is refused
//! before it ever reaches the app list:
//!
//! ```text
//! NSCocoaErrorDomain 4865 — "Expected to find key requireContainerAccess."
//! ```
//!
//! idevice 0.1.65 sends the five older keys only and keeps `AppServiceClient`'s
//! transport private, so there is no way to add one from outside it — hence
//! this second, much smaller client onto the same service. It is a stopgap:
//! delete it the day the crate sends the key itself.
//!
//! Older iOS is unaffected. The device decodes the keys it knows about and
//! ignores the rest, which is how the key could be added to the protocol in the
//! first place.

use std::borrow::Cow;

use idevice::core_device::{AppListEntry, CoreDeviceServiceClient};
use idevice::{IdeviceError, ReadWrite, RsdService};
use tracing::debug;

/// What `AppServiceClient` would be if its transport were reachable.
pub struct AppList(CoreDeviceServiceClient<Box<dyn ReadWrite>>);

impl RsdService for AppList {
    fn rsd_service_name() -> Cow<'static, str> {
        // The same service `AppServiceClient` connects to; only the request
        // body below differs.
        Cow::Borrowed("com.apple.coredevice.appservice")
    }

    async fn from_stream(stream: Box<dyn ReadWrite>) -> Result<Self, IdeviceError> {
        Ok(Self(CoreDeviceServiceClient::new(stream).await?))
    }
}

impl AppList {
    /// Third-party apps, which is every caller's question here: the farm
    /// uninstalls what a session installed and clears what it is told to, and
    /// neither can touch a system app.
    pub async fn list_apps(&mut self) -> Result<Vec<AppListEntry>, IdeviceError> {
        let started = std::time::Instant::now();
        let mut options = plist::Dictionary::new();
        options.insert("includeAppClips".into(), false.into());
        options.insert("includeRemovableApps".into(), true.into());
        options.insert("includeHiddenApps".into(), false.into());
        options.insert("includeInternalApps".into(), false.into());
        options.insert("includeDefaultApps".into(), false.into());
        // The three iOS 26 additions, all false: the farm wants bundle ids, not
        // an app's data container, its app groups or its container paths. They
        // are sent because the device requires the *keys*, not because it wants
        // what they ask for.
        options.insert("requireContainerAccess".into(), false.into());
        options.insert("includeAppGroupIdentifiers".into(), false.into());
        options.insert("includeContainerPaths".into(), false.into());

        // Deliberately unbounded here: the deadline lives in `IosBackend::apps`
        // so that it also covers opening the stream, which is the step that
        // actually hangs on an unwell device.
        let response = self
            .0
            .invoke_with_plist("com.apple.coredevice.feature.listapps", options)
            .await?;

        debug!(elapsed_ms = started.elapsed().as_millis(), "listed apps");

        let Some(entries) = response.as_array() else {
            return Err(IdeviceError::UnexpectedResponse(
                "list apps result was not an array".into(),
            ));
        };

        entries
            .iter()
            .map(|entry| {
                plist::from_value::<AppListEntry>(entry).map_err(|_| {
                    IdeviceError::UnexpectedResponse("failed to parse app list entry".into())
                })
            })
            .collect()
    }
}
