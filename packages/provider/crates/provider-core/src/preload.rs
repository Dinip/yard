//! Durable, farm-owned app preloads.
//!
//! A session install is disposable by design. A preload is different: the
//! provider keeps the package and a small manifest on its own disk, so cleanup
//! can compare the manifest with the device's installed apps and repair a
//! removal before the device returns to the pool.

use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use anyhow::{bail, Context as _, Result};
use serde::{Deserialize, Serialize};
use tokio::sync::{Mutex, RwLock};
use tracing::warn;

const MANIFEST_VERSION: u32 = 1;
const MANIFEST_FILE: &str = "manifest.json";
const ARTIFACTS_DIR: &str = "artifacts";

/// One app the provider must keep installed on one device.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProtectedPreload {
    pub device_id: String,
    pub app_id: String,
    pub platform: String,
    pub user_id: String,
    pub filename: String,
    pub size: i64,
    pub sha256: String,
    /// A single filename below `artifacts/`, never an arbitrary path.
    pub artifact: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    preloads: Vec<ProtectedPreload>,
}

/// The provider-local desired state for protected preloads.
#[derive(Clone)]
pub struct PreloadStore {
    /// `None` is used by unit tests and by callers that only need the in-memory
    /// policy. The real provider always opens a durable directory.
    root: Option<Arc<PathBuf>>,
    entries: Arc<RwLock<Vec<ProtectedPreload>>>,
    write_lock: Arc<Mutex<()>>,
}

impl PreloadStore {
    /// Opens or creates a durable preload directory.
    pub async fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        tokio::fs::create_dir_all(root.join(ARTIFACTS_DIR))
            .await
            .with_context(|| format!("creating preload directory {}", root.display()))?;

        let manifest_path = root.join(MANIFEST_FILE);
        let entries = match tokio::fs::read(&manifest_path).await {
            Ok(bytes) => {
                let manifest: Manifest = serde_json::from_slice(&bytes).with_context(|| {
                    format!("parsing preload manifest {}", manifest_path.display())
                })?;
                if manifest.version != MANIFEST_VERSION {
                    bail!(
                        "unsupported preload manifest version {} in {}",
                        manifest.version,
                        manifest_path.display()
                    );
                }
                manifest.preloads
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(err) => {
                return Err(err).with_context(|| {
                    format!("reading preload manifest {}", manifest_path.display())
                })
            }
        };

        Ok(Self {
            root: Some(Arc::new(root)),
            entries: Arc::new(RwLock::new(entries)),
            write_lock: Arc::new(Mutex::new(())),
        })
    }

    /// Creates a policy store without filesystem persistence. Production code
    /// uses [`Self::open`]; this keeps existing provider-core test harnesses
    /// lightweight and makes their lifetime explicit.
    pub fn in_memory() -> Self {
        Self {
            root: None,
            entries: Arc::new(RwLock::new(Vec::new())),
            write_lock: Arc::new(Mutex::new(())),
        }
    }

    /// Adds or replaces the protected package for `(device_id, app_id)`.
    ///
    /// The package is copied to a temporary file and atomically renamed before
    /// the manifest is committed. A provider crash therefore leaves either the
    /// old desired package or the new complete package, never a half-upload.
    #[allow(
        clippy::too_many_arguments,
        reason = "arguments are the persisted manifest fields"
    )]
    pub async fn protect(
        &self,
        device_id: &str,
        app_id: &str,
        platform: &str,
        user_id: &str,
        filename: &str,
        size: i64,
        sha256: &str,
        staged: &Path,
    ) -> Result<ProtectedPreload> {
        let _write = self.write_lock.lock().await;
        let artifact = artifact_name(sha256, filename);
        let entry = ProtectedPreload {
            device_id: device_id.to_owned(),
            app_id: app_id.to_owned(),
            platform: platform.to_owned(),
            user_id: user_id.to_owned(),
            filename: filename.to_owned(),
            size,
            sha256: sha256.to_owned(),
            artifact,
        };

        if let Some(root) = &self.root {
            let artifacts = root.join(ARTIFACTS_DIR);
            let final_path = artifacts.join(&entry.artifact);
            let temporary = artifacts.join(format!(".{}.part", uuid::Uuid::new_v4()));

            if let Err(err) = async {
                tokio::fs::copy(staged, &temporary)
                    .await
                    .with_context(|| format!("copying preload artifact {}", staged.display()))?;
                tokio::fs::rename(&temporary, &final_path)
                    .await
                    .with_context(|| {
                        format!("committing preload artifact {}", final_path.display())
                    })?;
                Result::<()>::Ok(())
            }
            .await
            {
                let _ = tokio::fs::remove_file(&temporary).await;
                return Err(err);
            }
        }

        let mut next = self.entries.read().await.clone();
        let old = next
            .iter()
            .find(|existing| existing.device_id == device_id && existing.app_id == app_id)
            .cloned();
        next.retain(|existing| existing.device_id != device_id || existing.app_id != app_id);
        next.push(entry.clone());

        if let Some(root) = &self.root {
            write_manifest(root, &next).await?;
            if let Some(old) = old.filter(|old| old.artifact != entry.artifact) {
                if let Some(path) = self.artifact_path(&old) {
                    let _ = tokio::fs::remove_file(path).await;
                }
            }
        }

        *self.entries.write().await = next;
        Ok(entry)
    }

    pub async fn for_device(&self, device_id: &str) -> Vec<ProtectedPreload> {
        self.entries
            .read()
            .await
            .iter()
            .filter(|entry| entry.device_id == device_id)
            .cloned()
            .collect()
    }

    /// Returns the complete provider-local desired state for the control-plane
    /// hello. Package bytes stay on this host; only the manifest metadata is
    /// reported upstream.
    pub async fn all(&self) -> Vec<ProtectedPreload> {
        self.entries.read().await.clone()
    }

    pub async fn is_protected(&self, device_id: &str, app_id: &str) -> bool {
        self.entries
            .read()
            .await
            .iter()
            .any(|entry| entry.device_id == device_id && entry.app_id == app_id)
    }

    /// Removes one app from desired state and deletes its retained package when
    /// no other device references the same artifact.
    pub async fn remove(&self, device_id: &str, app_id: &str) -> Result<Option<ProtectedPreload>> {
        let _write = self.write_lock.lock().await;
        let mut next = self.entries.read().await.clone();
        let removed = next
            .iter()
            .find(|entry| entry.device_id == device_id && entry.app_id == app_id)
            .cloned();
        let Some(removed) = removed else {
            return Ok(None);
        };
        next.retain(|entry| entry.device_id != device_id || entry.app_id != app_id);

        if let Some(root) = &self.root {
            write_manifest(root, &next).await?;
        }
        *self.entries.write().await = next.clone();

        if !next.iter().any(|entry| entry.artifact == removed.artifact) {
            if let Some(path) = self.artifact_path(&removed) {
                match tokio::fs::remove_file(&path).await {
                    Ok(()) => {}
                    Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                    Err(err) => warn!(
                        path = %path.display(),
                        %err,
                        "could not delete unreferenced preload artifact"
                    ),
                }
            }
        }

        Ok(Some(removed))
    }

    pub fn artifact_path(&self, entry: &ProtectedPreload) -> Option<PathBuf> {
        let root = self.root.as_deref()?;
        let component = Path::new(&entry.artifact);
        if !is_single_component(component) {
            return None;
        }
        Some(root.join(ARTIFACTS_DIR).join(component))
    }
}

async fn write_manifest(root: &Path, entries: &[ProtectedPreload]) -> Result<()> {
    let manifest = serde_json::to_vec_pretty(&Manifest {
        version: MANIFEST_VERSION,
        preloads: entries.to_owned(),
    })?;
    let path = root.join(MANIFEST_FILE);
    let temporary = root.join(format!(".{MANIFEST_FILE}.{}.tmp", uuid::Uuid::new_v4()));

    if let Err(err) = async {
        tokio::fs::write(&temporary, manifest)
            .await
            .with_context(|| {
                format!("writing temporary preload manifest {}", temporary.display())
            })?;
        tokio::fs::rename(&temporary, &path)
            .await
            .with_context(|| format!("committing preload manifest {}", path.display()))?;
        Result::<()>::Ok(())
    }
    .await
    {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(err);
    }
    Ok(())
}

fn artifact_name(sha256: &str, filename: &str) -> String {
    let filename = filename.rsplit(['/', '\\']).next().unwrap_or("upload.bin");
    let filename: String = filename
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        .collect();
    let filename = if filename.is_empty() || filename.chars().all(|c| c == '.') {
        "upload.bin"
    } else {
        filename.as_str()
    };
    format!("{sha256}-{filename}")
}

fn is_single_component(path: &Path) -> bool {
    let mut components = path.components();
    matches!(components.next(), Some(Component::Normal(_))) && components.next().is_none()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn durable_manifest_and_artifact_survive_reload() {
        let root =
            std::env::temp_dir().join(format!("yard-preload-store-test-{}", uuid::Uuid::new_v4()));
        let staged = root.join("staged.apk");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(&staged, b"apk bytes").await.unwrap();

        let store = PreloadStore::open(&root).await.unwrap();
        let entry = store
            .protect(
                "device-1",
                "com.example.app",
                "android",
                "admin-1",
                "release.apk",
                9,
                "abc123",
                &staged,
            )
            .await
            .unwrap();
        assert_eq!(store.for_device("device-1").await, vec![entry.clone()]);
        assert_eq!(
            tokio::fs::read(store.artifact_path(&entry).unwrap())
                .await
                .unwrap(),
            b"apk bytes"
        );

        let reloaded = PreloadStore::open(&root).await.unwrap();
        assert_eq!(reloaded.for_device("device-1").await, vec![entry]);

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[tokio::test]
    async fn removing_a_preload_updates_the_manifest_and_deletes_its_artifact() {
        let root =
            std::env::temp_dir().join(format!("yard-preload-store-test-{}", uuid::Uuid::new_v4()));
        let staged = root.join("staged.apk");
        tokio::fs::create_dir_all(&root).await.unwrap();
        tokio::fs::write(&staged, b"apk bytes").await.unwrap();

        let store = PreloadStore::open(&root).await.unwrap();
        let entry = store
            .protect(
                "device-1",
                "com.example.app",
                "android",
                "admin-1",
                "release.apk",
                9,
                "abc123",
                &staged,
            )
            .await
            .unwrap();
        let artifact = store.artifact_path(&entry).unwrap();

        assert_eq!(
            store.remove("device-1", "com.example.app").await.unwrap(),
            Some(entry)
        );
        assert!(store.for_device("device-1").await.is_empty());
        assert!(!artifact.exists());
        assert!(PreloadStore::open(&root)
            .await
            .unwrap()
            .for_device("device-1")
            .await
            .is_empty());

        let _ = tokio::fs::remove_dir_all(&root).await;
    }

    #[test]
    fn artifact_names_are_one_safe_path_component() {
        let name = artifact_name("abc123", "../../release$(id).apk");
        assert_eq!(name, "abc123-releaseid.apk");
        assert!(is_single_component(Path::new(&name)));
    }
}
