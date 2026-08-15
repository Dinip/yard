//! The Developer Disk Image: fetch it, cache it, mount it.
//!
//! A device offers no `com.apple.coredevice.*` services — no screen, no HID, no
//! app service — until a DDI is mounted, and **the mount is lost on every
//! reboot**. Developer Mode staying on in Settings does not preserve it. Before
//! this module the provider left that to whoever ran the host (`docs/PROVIDER.md`
//! told them to run `devicectl`), which is a hands-on step in a farm whose point
//! is unattended devices.
//!
//! Two halves, deliberately separable:
//!
//! * [`DdiCache`] — the payload on disk, fetched from a mirror once per process
//!   and shared by every device on the host. Testable with no hardware.
//! * [`ensure_mounted`] — the lockdown conversation with one phone.
//!
//! Since iOS 17 there is **one** image for every device and iOS version; what
//! makes it device-specific is a personalization ticket signed by Apple's TSS
//! server, which `idevice` requests during the mount. So the cache holds exactly
//! one payload, not one per iOS version, and the network is touched twice at
//! most: once ever for the image, and once per device for its ticket.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context as _, Result};
use idevice::lockdown::LockdownClient;
use idevice::mobile_image_mounter::ImageMounter;
use idevice::provider::IdeviceProvider;
use idevice::IdeviceService as _;
use tokio::sync::OnceCell;
use tracing::{debug, info, warn};

pub use provider_core::config::DdiConfig;

/// `LookupImage`'s type for iOS 17+. The pre-17 `Developer` image is not
/// supported here, and neither is pre-17 anywhere else in this backend.
const IMAGE_TYPE: &str = "Personalized";

/// The three files, named as the mirror names them.
const IMAGE_FILE: &str = "Image.dmg";
const MANIFEST_FILE: &str = "BuildManifest.plist";
const TRUST_CACHE_FILE: &str = "Image.dmg.trustcache";

/// How long a failed mount is left alone.
///
/// The session supervisor retries every 5s forever. The cheap half of
/// [`ensure_mounted`] runs on every one of those, but the expensive half ends in
/// a request to Apple's TSS server, and a device that cannot mount at all — an
/// iOS newer than the mirror's image, most likely — must not turn that into a
/// request every five seconds for as long as the provider runs.
pub const MOUNT_RETRY_BACKOFF: Duration = Duration::from_secs(300);

/// The mountable payload, held in memory once per provider process.
///
/// ~16 MB. Small enough to keep resident, and keeping it resident is what lets
/// a phone that reboots mid-shift come back without touching the disk again.
pub struct DdiPayload {
    pub image: Vec<u8>,
    pub build_manifest: Vec<u8>,
    pub trust_cache: Vec<u8>,
}

impl std::fmt::Debug for DdiPayload {
    /// Byte vectors, so the derived form would print 16 MB of numbers into a
    /// log line the first time anything traced an `IosOptions`.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DdiPayload")
            .field("image", &self.image.len())
            .field("build_manifest", &self.build_manifest.len())
            .field("trust_cache", &self.trust_cache.len())
            .finish()
    }
}

/// The disk cache, and the fetch that fills it.
#[derive(Debug)]
pub struct DdiCache {
    config: DdiConfig,
    /// One fetch however many devices ask at once, and never a second one.
    payload: OnceCell<Arc<DdiPayload>>,
}

impl DdiCache {
    pub fn new(config: DdiConfig) -> Arc<Self> {
        Arc::new(Self {
            config,
            payload: OnceCell::new(),
        })
    }

    /// The payload, from memory, then from `cache_dir`, then from the mirror.
    pub async fn payload(&self) -> Result<Arc<DdiPayload>> {
        self.payload
            .get_or_try_init(|| async {
                let payload = self.load_or_fetch().await?;
                Ok(Arc::new(payload))
            })
            .await
            .cloned()
    }

    async fn load_or_fetch(&self) -> Result<DdiPayload> {
        if let Some(payload) = self.read_cache().await? {
            info!(
                cache = %self.config.cache_dir.display(),
                build = payload.build_version().unwrap_or_else(|| "unknown".into()),
                "using the cached developer disk image"
            );
            return Ok(payload);
        }

        let payload = self.fetch().await?;
        // A farm host with a read-only or unwritable cache path should still
        // mount — it just pays for the download again next start.
        if let Err(err) = self.write_cache(&payload).await {
            warn!(
                cache = %self.config.cache_dir.display(),
                %err,
                "could not cache the developer disk image; it will be downloaded again next start"
            );
        }
        Ok(payload)
    }

    /// `Some` only when all three files are there and non-empty.
    ///
    /// Partial is treated as absent rather than as an error: a half-written
    /// cache should heal itself on the next start, not need an operator.
    async fn read_cache(&self) -> Result<Option<DdiPayload>> {
        let mut files = Vec::new();
        for name in [IMAGE_FILE, MANIFEST_FILE, TRUST_CACHE_FILE] {
            let path = self.config.cache_dir.join(name);
            match tokio::fs::read(&path).await {
                Ok(bytes) if !bytes.is_empty() => files.push(bytes),
                Ok(_) => {
                    warn!(path = %path.display(), "cached DDI file is empty; refetching");
                    return Ok(None);
                }
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
                Err(err) => return Err(err).with_context(|| format!("reading {}", path.display())),
            }
        }

        let mut files = files.into_iter();
        Ok(Some(DdiPayload {
            image: files.next().expect("three files read"),
            build_manifest: files.next().expect("three files read"),
            trust_cache: files.next().expect("three files read"),
        }))
    }

    async fn fetch(&self) -> Result<DdiPayload> {
        let base = self.config.base();
        info!(%base, "downloading the developer disk image");

        // Its own client rather than one shared with the control plane: this
        // runs at most once per process and wants a generous timeout for a
        // 16 MB body over whatever link a lab has.
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(120))
            .build()
            .context("building the DDI http client")?;

        let mut files = Vec::new();
        for name in [IMAGE_FILE, MANIFEST_FILE, TRUST_CACHE_FILE] {
            let url = format!("{base}/{name}");
            let response = client
                .get(&url)
                .send()
                .await
                .with_context(|| format!("fetching {url}"))?;
            let status = response.status();
            if !status.is_success() {
                bail!("fetching {url}: {status}");
            }
            let bytes = response
                .bytes()
                .await
                .with_context(|| format!("reading the body of {url}"))?;
            if bytes.is_empty() {
                bail!("{url} returned an empty body");
            }
            debug!(%url, bytes = bytes.len(), "fetched");
            files.push(bytes.to_vec());
        }

        let mut files = files.into_iter();
        let payload = DdiPayload {
            image: files.next().expect("three files fetched"),
            build_manifest: files.next().expect("three files fetched"),
            trust_cache: files.next().expect("three files fetched"),
        };

        // The one field worth an operator's attention: an image older than a
        // just-updated phone is the failure this whole path can't fix itself.
        info!(
            build = payload.build_version().unwrap_or_else(|| "unknown".into()),
            image_bytes = payload.image.len(),
            "downloaded the developer disk image"
        );
        Ok(payload)
    }

    /// Write through a `.part` file, so a process killed mid-write cannot leave
    /// a truncated file that the next start reads back as a valid cache.
    async fn write_cache(&self, payload: &DdiPayload) -> Result<()> {
        tokio::fs::create_dir_all(&self.config.cache_dir)
            .await
            .with_context(|| format!("creating {}", self.config.cache_dir.display()))?;

        for (name, bytes) in [
            (IMAGE_FILE, &payload.image),
            (MANIFEST_FILE, &payload.build_manifest),
            (TRUST_CACHE_FILE, &payload.trust_cache),
        ] {
            write_atomic(&self.config.cache_dir.join(name), bytes).await?;
        }
        Ok(())
    }
}

impl DdiPayload {
    /// `ProductBuildVersion` out of the BuildManifest, e.g. `27A5228h`.
    fn build_version(&self) -> Option<String> {
        let manifest: plist::Dictionary = plist::from_bytes(&self.build_manifest).ok()?;
        manifest
            .get("ProductBuildVersion")?
            .as_string()
            .map(str::to_owned)
    }
}

async fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    // Appended, not `with_extension`: `Image.dmg.trustcache` and `Image.dmg`
    // both reduce to `Image.dmg.part` under that.
    let temporary = path.with_file_name(format!(
        "{}.part",
        path.file_name().unwrap_or_default().to_string_lossy()
    ));
    tokio::fs::write(&temporary, bytes)
        .await
        .with_context(|| format!("writing {}", temporary.display()))?;
    tokio::fs::rename(&temporary, path)
        .await
        .with_context(|| format!("renaming {} into place", temporary.display()))
}

/// Whether the expensive half of a mount may run again.
///
/// Split out from [`ensure_mounted`] so the backoff is testable without a phone.
pub fn may_attempt_mount(last_failure: Option<Instant>, now: Instant) -> bool {
    match last_failure {
        Some(failed_at) => now.duration_since(failed_at) >= MOUNT_RETRY_BACKOFF,
        None => true,
    }
}

/// The outcome of one [`ensure_mounted`] call, for the caller's logs.
#[derive(Debug, PartialEq, Eq)]
pub enum MountOutcome {
    /// `LookupImage` found one. The common case, and one plist round trip.
    AlreadyMounted,
    Mounted,
    /// Backed off after an earlier failure; see [`MOUNT_RETRY_BACKOFF`].
    Skipped,
}

/// Mount the DDI on one device, if it is not already mounted.
///
/// Runs over plain lockdown, not the tunnel — which is the point: the mounter
/// service is reachable exactly when the CoreDevice services are not.
pub async fn ensure_mounted(
    provider: &dyn IdeviceProvider,
    cache: &DdiCache,
    last_failure: Option<Instant>,
) -> Result<MountOutcome> {
    let mut mounter = ImageMounter::connect(provider)
        .await
        .map_err(|err| anyhow!("image mounter connect: {err:?}"))?;

    if mounter.lookup_image(IMAGE_TYPE).await.is_ok() {
        // The crate's own warning: the device stops answering unless a lockdown
        // query follows a mounter client. Cheap, and this is the hot path.
        settle_lockdown(provider).await;
        return Ok(MountOutcome::AlreadyMounted);
    }

    if !may_attempt_mount(last_failure, Instant::now()) {
        settle_lockdown(provider).await;
        return Ok(MountOutcome::Skipped);
    }

    match mounter.query_developer_mode_status().await {
        Ok(true) => {}
        Ok(false) => {
            settle_lockdown(provider).await;
            bail!(
                "Developer Mode is off. Nothing can mount a developer disk image until it is on: \
                 Settings → Privacy & Security → Developer Mode, then reboot and unlock the device."
            );
        }
        // Not fatal. Some devices answer this poorly and still mount; the mount
        // itself is the real test.
        Err(err) => debug!(?err, "could not read the developer mode status"),
    }

    let unique_chip_id = unique_chip_id(provider).await?;
    let payload = cache.payload().await?;

    info!("mounting the developer disk image");
    // The callback fires per chunk of a 16 MB upload, so quarters are logged
    // rather than every chunk. Shared state because the callback is `Fn`.
    let reported = Arc::new(AtomicUsize::new(0));
    mounter
        .mount_personalized_with_callback(
            provider,
            payload.image.clone(),
            payload.trust_cache.clone(),
            &payload.build_manifest,
            None,
            unique_chip_id,
            |((done, total), reported): ((usize, usize), Arc<AtomicUsize>)| async move {
                let percent = (done * 100).checked_div(total).unwrap_or(100);
                let quarter = percent / 25;
                if quarter > reported.swap(quarter, Ordering::Relaxed) {
                    debug!(percent, "uploading the developer disk image");
                }
            },
            reported,
        )
        .await
        .map_err(|err| anyhow!("mounting the developer disk image: {err:?}"))?;

    settle_lockdown(provider).await;
    info!("developer disk image mounted");
    Ok(MountOutcome::Mounted)
}

/// `UniqueChipID`, which the personalization ticket is issued against.
///
/// The retry mirrors `idevice`'s own mounter tool: a fresh lockdown client
/// answers some keys unsessioned and refuses others, and which is which varies
/// by iOS version, so the session is started only if the first read fails.
async fn unique_chip_id(provider: &dyn IdeviceProvider) -> Result<u64> {
    let mut lockdown = LockdownClient::connect(provider)
        .await
        .map_err(|err| anyhow!("lockdown connect: {err:?}"))?;

    let value = match lockdown.get_value(Some("UniqueChipID"), None).await {
        Ok(value) => value,
        Err(_) => {
            let pairing = provider
                .get_pairing_file()
                .await
                .map_err(|err| anyhow!("pairing file: {err:?}"))?;
            lockdown
                .start_session(&pairing)
                .await
                .map_err(|err| anyhow!("lockdown session: {err:?}"))?;
            lockdown
                .get_value(Some("UniqueChipID"), None)
                .await
                .map_err(|err| anyhow!("read UniqueChipID: {err:?}"))?
        }
    };

    value
        .as_unsigned_integer()
        .ok_or_else(|| anyhow!("UniqueChipID was not an integer: {value:?}"))
}

/// One lockdown round trip after using the mounter.
///
/// `ImageMounter`'s docs are explicit: "A lockdown client must be established
/// and queried after establishing a mounter client, or the device will stop
/// responding to requests." Failures here are ignored — the device is either
/// fine or about to fail the tunnel bring-up with a better error.
async fn settle_lockdown(provider: &dyn IdeviceProvider) {
    match LockdownClient::connect(provider).await {
        Ok(mut lockdown) => {
            let _ = lockdown.get_value(Some("ProductVersion"), None).await;
        }
        Err(err) => debug!(?err, "post-mount lockdown query failed"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A throwaway mirror. Counts requests, so "the cache was used" is an
    /// assertion about the network rather than about a log line.
    struct Mirror {
        base: String,
        hits: Arc<AtomicUsize>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl Mirror {
        async fn start(missing: Option<&'static str>) -> Self {
            use axum::extract::{Path as AxumPath, State};
            use axum::routing::get;

            let hits = Arc::new(AtomicUsize::new(0));
            let state = (hits.clone(), missing);
            let app = axum::Router::new()
                .route(
                    "/{file}",
                    get(
                        |State((hits, missing)): State<(
                            Arc<AtomicUsize>,
                            Option<&'static str>,
                        )>,
                         AxumPath(file): AxumPath<String>| async move {
                            hits.fetch_add(1, Ordering::Relaxed);
                            if Some(file.as_str()) == missing {
                                return (axum::http::StatusCode::NOT_FOUND, Vec::new());
                            }
                            (axum::http::StatusCode::OK, body_for(&file))
                        },
                    ),
                )
                .with_state(state);

            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let base = format!("http://{}", listener.local_addr().unwrap());
            let handle = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            Self { base, hits, handle }
        }

        fn hits(&self) -> usize {
            self.hits.load(Ordering::Relaxed)
        }
    }

    impl Drop for Mirror {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    fn body_for(file: &str) -> Vec<u8> {
        match file {
            MANIFEST_FILE => {
                let mut manifest = plist::Dictionary::new();
                manifest.insert("ProductBuildVersion".into(), "27A5228h".into());
                let mut buffer = Vec::new();
                plist::to_writer_xml(&mut buffer, &manifest).unwrap();
                buffer
            }
            IMAGE_FILE => b"image-bytes".to_vec(),
            _ => b"trust-cache-bytes".to_vec(),
        }
    }

    fn cache_at(dir: &Path, base: &str) -> Arc<DdiCache> {
        DdiCache::new(DdiConfig {
            enabled: true,
            cache_dir: dir.to_path_buf(),
            base_url: base.to_owned(),
        })
    }

    #[tokio::test]
    async fn a_cold_cache_downloads_and_writes_all_three_files() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(None).await;
        let cache = cache_at(dir.path(), &mirror.base);

        let payload = cache.payload().await.unwrap();
        assert_eq!(payload.image, b"image-bytes");
        assert_eq!(payload.trust_cache, b"trust-cache-bytes");
        assert_eq!(payload.build_version().as_deref(), Some("27A5228h"));
        assert_eq!(mirror.hits(), 3);

        for name in [IMAGE_FILE, MANIFEST_FILE, TRUST_CACHE_FILE] {
            assert!(dir.path().join(name).exists(), "{name} was not cached");
        }
        // And no `.part` left behind.
        assert!(!dir.path().join("Image.part").exists());
    }

    #[tokio::test]
    async fn a_warm_cache_never_touches_the_network() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(None).await;
        for name in [IMAGE_FILE, MANIFEST_FILE, TRUST_CACHE_FILE] {
            std::fs::write(dir.path().join(name), body_for(name)).unwrap();
        }

        let cache = cache_at(dir.path(), &mirror.base);
        let payload = cache.payload().await.unwrap();

        assert_eq!(payload.image, b"image-bytes");
        assert_eq!(mirror.hits(), 0);
    }

    /// Ten devices on one host must not mean ten downloads.
    #[tokio::test]
    async fn concurrent_devices_share_one_download() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(None).await;
        let cache = cache_at(dir.path(), &mirror.base);

        let calls = (0..8).map(|_| {
            let cache = cache.clone();
            tokio::spawn(async move { cache.payload().await.map(|p| p.image.len()) })
        });
        for call in calls {
            assert_eq!(call.await.unwrap().unwrap(), b"image-bytes".len());
        }
        assert_eq!(mirror.hits(), 3);
    }

    /// A half-populated cache directory is a refetch, not an error and not a
    /// mount with a missing file.
    #[tokio::test]
    async fn a_partial_cache_is_refetched() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(None).await;
        std::fs::write(dir.path().join(IMAGE_FILE), body_for(IMAGE_FILE)).unwrap();

        let cache = cache_at(dir.path(), &mirror.base);
        cache.payload().await.unwrap();
        assert_eq!(mirror.hits(), 3);
    }

    #[tokio::test]
    async fn an_empty_cached_file_is_refetched_rather_than_mounted() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(None).await;
        for name in [IMAGE_FILE, MANIFEST_FILE, TRUST_CACHE_FILE] {
            std::fs::write(dir.path().join(name), body_for(name)).unwrap();
        }
        std::fs::write(dir.path().join(TRUST_CACHE_FILE), b"").unwrap();

        let cache = cache_at(dir.path(), &mirror.base);
        let payload = cache.payload().await.unwrap();

        assert_eq!(payload.trust_cache, b"trust-cache-bytes");
        assert_eq!(mirror.hits(), 3);
    }

    /// A mirror missing a file must leave nothing behind that a later start
    /// would read back as a complete cache.
    #[tokio::test]
    async fn a_failed_fetch_caches_nothing() {
        let dir = tempdir::Dir::new();
        let mirror = Mirror::start(Some(TRUST_CACHE_FILE)).await;
        let cache = cache_at(dir.path(), &mirror.base);

        let err = cache.payload().await.unwrap_err().to_string();
        assert!(err.contains("404"), "{err}");
        assert!(!dir.path().join(IMAGE_FILE).exists());
    }

    #[tokio::test]
    async fn an_unreachable_mirror_is_an_error_not_a_hang() {
        let dir = tempdir::Dir::new();
        // Port 1 on loopback: nothing listens, and the connection is refused
        // rather than timing out.
        let cache = cache_at(dir.path(), "http://127.0.0.1:1");
        assert!(cache.payload().await.is_err());
    }

    #[test]
    fn a_failed_mount_is_left_alone_for_the_backoff_window() {
        let now = Instant::now();
        assert!(may_attempt_mount(None, now));
        assert!(!may_attempt_mount(Some(now), now));
        assert!(!may_attempt_mount(
            Some(now),
            now + MOUNT_RETRY_BACKOFF - Duration::from_secs(1)
        ));
        assert!(may_attempt_mount(Some(now), now + MOUNT_RETRY_BACKOFF));
    }

    /// Minimal scoped temp directory, as `provider-core::config`'s tests do —
    /// a dev-dependency for one module is not worth it.
    mod tempdir {
        use std::path::{Path, PathBuf};

        pub struct Dir(PathBuf);

        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

        impl Dir {
            pub fn new() -> Self {
                let path = std::env::temp_dir().join(format!(
                    "farm-ddi-test-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
                ));
                std::fs::create_dir_all(&path).unwrap();
                Self(path)
            }

            pub fn path(&self) -> &Path {
                &self.0
            }
        }

        impl Drop for Dir {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
    }
}
