//! Reservation-bound access to the companion's App Group, over the USB tunnel.

use anyhow::{bail, Context as _, Result};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use yard_protocol::{FileEntry, FileKind, FileListing};

use crate::files::Files;

const ROOT: &str = "Library/Caches/YARD Drop";
const LEASE: &str = "Library/Caches/YARD Drop/active-capture.json";
const INCOMING: &str = "Library/Caches/YARD Drop/Incoming";
const MARKER: &str = "_YARD_READY";

pub(crate) struct Inbox {
    reservation: Option<String>,
    refreshed: Option<Instant>,
    synced: bool,
}

impl Default for Inbox {
    fn default() -> Self {
        Self {
            reservation: None,
            refreshed: None,
            synced: true,
        }
    }
}

impl Inbox {
    pub fn needs_sync(&self) -> bool {
        !self.synced
    }
    pub fn select(&mut self, reservation: Option<&str>) -> Result<bool> {
        if let Some(id) = reservation {
            uuid::Uuid::parse_str(id).context("invalid Drop reservation")?;
        }
        let changed = self.reservation.as_deref() != reservation;
        if changed {
            self.reservation = reservation.map(str::to_owned);
            self.synced = false;
            self.refreshed = None;
        }
        Ok(changed)
    }

    pub fn should_refresh(&self) -> bool {
        !self.synced
            || self
                .refreshed
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
    }

    pub async fn bind(&mut self, files: &mut Files, reservation: Option<&str>) -> Result<()> {
        self.select(reservation)?;
        if self.synced
            && self
                .refreshed
                .is_some_and(|t| t.elapsed() < Duration::from_secs(30))
        {
            return Ok(());
        }
        ensure_directory(files, "Library").await?;
        ensure_directory(files, "Library/Caches").await?;
        ensure_directory(files, ROOT).await?;
        ensure_directory(files, INCOMING).await?;
        if !self.synced {
            if files
                .list(ROOT)
                .await?
                .iter()
                .any(|name| name == "active-capture.json")
            {
                files.remove(LEASE, false).await?;
            }
            for name in files.list(INCOMING).await? {
                if !safe_component(&name) {
                    bail!("invalid Drop directory entry");
                }
                if Some(name.as_str()) != reservation {
                    remove_tree(files, &format!("{INCOMING}/{name}"), 0).await?;
                }
            }
        }
        if let Some(id) = reservation {
            ensure_directory(files, &format!("{INCOMING}/{id}")).await?;
            let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
            let bytes =
                serde_json::to_vec(&serde_json::json!({"id":id,"expiresAt":now + 120_000}))?;
            let temporary = format!("{ROOT}/lease-{}.tmp", uuid::Uuid::new_v4());
            files.write_small(&temporary, bytes).await?;
            files.rename(&temporary, LEASE).await?;
        }
        self.synced = true;
        self.refreshed = Some(Instant::now());
        Ok(())
    }

    pub async fn list(&mut self, files: &mut Files, path: &str) -> Result<FileListing> {
        let id = self
            .reservation
            .as_deref()
            .context("no active Drop reservation")?;
        let inner = resolve(path, id)?;
        check_path(files, &inner, "directory").await?;
        let root = path == "drop:/";
        let mut entries = Vec::new();
        for name in files.list(&inner).await? {
            if !safe_component(&name) || (root && uuid::Uuid::parse_str(&name).is_err()) {
                continue;
            }
            let child = format!("{inner}/{name}");
            let attrs = files.attributes(&child).await?;
            let kind = match attrs["fileType"].as_str() {
                Some("directory") if root => {
                    // The extension moves an already completed batch into Incoming atomically.
                    if !files.list(&child).await?.iter().any(|name| name == MARKER) {
                        continue;
                    }
                    FileKind::Directory
                }
                Some("regular") if !root => FileKind::File,
                _ => continue,
            };
            entries.push(FileEntry {
                path: if root {
                    format!("drop:/{id}/{name}")
                } else {
                    format!("{path}/{name}")
                },
                name,
                size: if kind == FileKind::File {
                    attrs["size"].as_i64()
                } else {
                    None
                },
                modified_at: attrs["modificationTime"]
                    .as_f64()
                    .map(|t| ((t + 978307200.0) * 1000.0) as i64),
                kind,
            });
        }
        Ok(FileListing {
            path: path.into(),
            parent: if root { None } else { Some("drop:/".into()) },
            entries,
        })
    }

    pub async fn pull(
        &mut self,
        files: &mut Files,
        path: &str,
        destination: &std::path::Path,
    ) -> Result<u64> {
        let id = self
            .reservation
            .as_deref()
            .context("no active Drop reservation")?;
        let inner = resolve(path, id)?;
        if path.split('/').count() != 4 {
            bail!("Drop path is not a batch file");
        }
        check_path(files, &inner, "regular").await?;
        files.pull(&inner, destination).await
    }
}

fn safe_component(value: &str) -> bool {
    !value.is_empty()
        && value != "."
        && value != ".."
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '/' | '\\' | ':'))
}

fn resolve(path: &str, reservation: &str) -> Result<String> {
    if path == "drop:/" {
        return Ok(format!("{INCOMING}/{reservation}"));
    }
    let parts: Vec<_> = path
        .strip_prefix("drop:/")
        .context("invalid Drop path")?
        .split('/')
        .collect();
    if !(2..=3).contains(&parts.len())
        || parts[0] != reservation
        || uuid::Uuid::parse_str(parts[1]).is_err()
        || !parts.iter().all(|p| safe_component(p))
    {
        bail!("Drop path does not belong to the active reservation");
    }
    Ok(format!("{INCOMING}/{}", parts.join("/")))
}

async fn check_path(files: &mut Files, path: &str, final_kind: &str) -> Result<()> {
    let mut current = String::new();
    let parts: Vec<_> = path.split('/').collect();
    for (index, part) in parts.iter().enumerate() {
        if !safe_component(part) {
            bail!("invalid Drop path component");
        }
        if !current.is_empty() {
            current.push('/');
        }
        current.push_str(part);
        let attrs = files.attributes(&current).await?;
        let expected = if index + 1 == parts.len() {
            final_kind
        } else {
            "directory"
        };
        if attrs["fileType"].as_str() != Some(expected) {
            bail!("Drop path contains a symbolic link or unexpected file type");
        }
    }
    Ok(())
}

async fn ensure_directory(files: &mut Files, path: &str) -> Result<()> {
    let (parent, name) = path.rsplit_once('/').unwrap_or((".", path));
    if !files.list(parent).await?.iter().any(|entry| entry == name) {
        files.mkdir(path).await?;
    }
    check_path(files, path, "directory").await
}

async fn remove_tree(files: &mut Files, path: &str, depth: usize) -> Result<()> {
    if depth > 8 {
        bail!("Drop directory nesting exceeds 8 levels");
    }
    let attrs = files.attributes(path).await?;
    if attrs["fileType"] == "directory" {
        for name in files.list(path).await? {
            if !safe_component(&name) {
                bail!("invalid Drop directory entry");
            }
            Box::pin(remove_tree(files, &format!("{path}/{name}"), depth + 1)).await?;
        }
        files.remove(path, true).await
    } else {
        files.remove(path, false).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const ID: &str = "aa000000-0000-0000-0000-000000000001";
    const BATCH: &str = "bb000000-0000-0000-0000-000000000002";
    #[test]
    fn paths_are_bound_to_the_current_reservation() {
        assert_eq!(resolve("drop:/", ID).unwrap(), format!("{INCOMING}/{ID}"));
        assert!(resolve(&format!("drop:/{ID}/{BATCH}/photo.jpg"), ID).is_ok());
        for path in [
            format!("drop:/{BATCH}/{BATCH}/photo.jpg"),
            format!("drop:/{ID}/{BATCH}/../secret"),
            format!("drop:/{ID}/{BATCH}/.."),
            format!("drop:/{ID}/{BATCH}/folder/file"),
            format!("drop:/{ID}/{BATCH}/a\\b"),
            "drop:/Exports".into(),
        ] {
            assert!(resolve(&path, ID).is_err(), "{path}");
        }
    }
}
